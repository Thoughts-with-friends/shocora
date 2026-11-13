//! Block JIT on top of Cranelift.
//!
//! ```text
//!  Jit::run ──► block at pc translated? ──yes──► native code ──tail call──► next block
//!      ▲                │ no                          │ budget used up, target
//!      │                ▼                             ▼ not translated, or an event
//!      │        queue it for a worker, interpret      │
//!      └──────────────────┴───────────────────────────┘
//! ```
//!
//! Like Cemu's recompiler thread, translation never stops the guest: workers
//! compile in the background while the block runs on the interpreter, so new
//! code costs speed for a moment instead of a stall on the frame.
//!
//! Translating comes in two steps, because the interpreter is what makes a new scene stutter:
//! a block is first translated as quickly as possible. When nothing more urgent is left, it is
//! translated again with Cranelift's optimizations, and that code takes over. Blocks find each
//! other through the page table, so the next time they jump there they get the faster code.
//!
//! Blocks jump straight to each other: a single dispatch loop would funnel every
//! guest branch through one host call site, which the host CPU cannot predict.
//!
//! Translated instructions operate on the [`CpuState`] directly and access
//! guest memory through [`Bus::fast_memory`]. Every instruction `emit` does not
//! know falls back to one [`step`] of the reference interpreter, so the JIT
//! is an optimisation only: both must behave identically.
mod code_memory;
mod emit;

use std::{
    collections::{HashMap, VecDeque},
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Condvar, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

use cranelift_codegen::{
    ir::{AbiParam, InstBuilder, Signature, Type, types::I32},
    isa::{CallConv, OwnedTargetIsa},
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};

use self::emit::{Job, Reach, Translated};
use crate::{Bus, CpuError, CpuState, StepEvent, interpreter::step};

/// Longest block of the quick first translation.
const MAX_BLOCK_INSTRUCTIONS: usize = 64;
/// Most instructions of one region: Cranelift takes longer than linearly for more.
const MAX_REGION_INSTRUCTIONS: usize = 256;
/// How far from its entry a region follows branches: about as far as a function goes.
const REGION_REACH: u32 = 0x4000;
const PAGE_BITS: u32 = 12;
const PAGE_ENTRIES: usize = 1 << (PAGE_BITS - 2);
/// Translation threads at most; the emulator and renderer need cores too.
const MAX_WORKERS: usize = 6;
/// Of those, how many also optimize: this can wait, and must not take cores from the guest.
const OPTIMIZING_WORKERS: usize = 2;
/// How often finished translations are taken in while running: making code executable means
/// asking the system to change page protections.
const COLLECT_INTERVAL: Duration = Duration::from_millis(1);
/// Runs after which a block is optimized. Most blocks run a handful of times (start-up,
/// loading); optimizing all of them doubled the time spent translating for no gain.
const HOT_RUNS: u32 = 1000;
/// Quick translations kept before they are all let go at the next safe moment. Most of them
/// ran a few times while the title loaded; what still runs is translated again. Their code
/// took most of the gigabyte the JIT used in Breath of the Wild.
const QUICK_CODE_BUDGET: usize = 256 << 20;
/// How often the run counters are looked at.
const HOT_SCAN_INTERVAL: Duration = Duration::from_millis(50);
/// Run counters per allocation; they never move, as translated code holds their address.
const COUNTER_CHUNK: usize = 4096;
/// Candidates looked at per scan: a title has hundreds of thousands of blocks, and going
/// through all of them every scan took a core's time.
const HOT_SCAN_BATCH: usize = 16384;
/// Calls of [`Jit::run`] between looks at the clock for collecting and scanning: a core
/// calls it after every HLE call, millions of times a second.
const HOUSEKEEPING_CALLS: u32 = 64;

/// A quickly translated block waiting to become hot: what it takes to find it again.
#[derive(Clone, Copy)]
struct Candidate {
    pc: u32,
    range: (u32, u32),
    counter: *const u32,
}

impl Candidate {
    const fn overlaps(&self, start: u32, end: u32) -> bool {
        self.range.0 < end && start < self.range.1
    }
}

thread_local! {
    /// Calls of [`Jit::run`] on this thread, for [`HOUSEKEEPING_CALLS`].
    static RUN_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// What a block's helper calls need besides the registers.
pub(crate) struct HelperContext {
    bus: *mut c_void,
    /// Why a block returned early.
    result: Option<Result<StepEvent, CpuError>>,
    /// Instructions left to run; blocks subtract theirs and stop at zero.
    remaining: i64,
}

/// Calls the block at the last argument with the tail-calling convention.
type EnterFn = unsafe extern "C" fn(*mut CpuState, *mut u8, *mut HelperContext, *const u8) -> u32;

/// Translations of one guest page; null where there is none. Native code reads this.
type Page = [*const u8; PAGE_ENTRIES];

/// One instruction of the reference interpreter, called from native code.
/// Returns non-zero when the block must stop.
unsafe extern "C" fn interpret_one<B: Bus>(
    state: *mut CpuState,
    context: *mut HelperContext,
) -> u32 {
    // SAFETY: `Jit::run` passes live, exclusive pointers for the duration of the block.
    let (state, context) = unsafe { (&mut *state, &mut *context) };
    // SAFETY: `bus` was erased from a `&mut B` in `Jit::run`.
    let bus = unsafe { &mut *context.bus.cast::<B>() };
    match step(state, bus) {
        Ok(StepEvent::Continue) => 0,
        other => {
            context.result = Some(other);
            1
        }
    }
}

/// What the workers have to do.
#[derive(Default)]
struct JobQueues {
    /// Blocks the guest runs on the interpreter right now.
    first: VecDeque<Job>,
    /// Blocks with quickly made code, to be made faster when there is time.
    optimize: VecDeque<Job>,
    closed: bool,
}

/// The queues and how the workers wait for them.
#[derive(Default)]
struct JobBoard {
    queues: Mutex<JobQueues>,
    changed: Condvar,
}

impl JobBoard {
    fn change(&self, change: impl FnOnce(&mut JobQueues)) {
        change(&mut self.queues.lock().unwrap_or_else(PoisonError::into_inner));
        self.changed.notify_all();
    }

    /// The most urgent job this worker takes, as soon as there is one; `None` when work is over.
    fn next(&self, optimizes: bool) -> Option<Job> {
        let mut queues = self.queues.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if queues.closed {
                return None;
            }
            let optimize = |queues: &mut JobQueues| {
                if optimizes { queues.optimize.pop_front() } else { None }
            };
            if let Some(job) = queues.first.pop_front().or_else(|| optimize(&mut queues)) {
                return Some(job);
            }
            queues = self.changed.wait(queues).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// Translated code of one guest address space, shared by the cores running it.
///
/// Like Cemu's recompiler, all cores run the same code: native code finds its way through
/// the page table without asking anyone, and only what changes the table (new translations,
/// invalidations) takes the lock.
pub struct Jit {
    state: Mutex<JitState>,
    enter: EnterFn,
    /// The page table of `state`, which never moves, for lookups without the lock.
    pages: *const Option<Box<Page>>,
    interpreted: AtomicU64,
    /// Waits for each translation instead of interpreting meanwhile.
    synchronous: bool,
}

// SAFETY: native code and its page table are only changed under the lock; entries are
// pointer-sized and written whole, so a core reading one meanwhile sees the old or the new
// code, and code is never freed while the `Jit` lives.
unsafe impl Send for Jit {}
// SAFETY: see above.
unsafe impl Sync for Jit {}

/// An installed region: where its code is, and whether it is a quick translation.
struct Installed {
    range: (u32, u32),
    entries: Vec<u32>,
    code: *const u8,
    quick: bool,
}

/// A module whose code lives in [`code_memory::CodeMemory`].
fn new_module(isa: &OwnedTargetIsa) -> JITModule {
    let mut builder =
        JITBuilder::with_isa(Arc::clone(isa), cranelift_module::default_libcall_names());
    builder.memory_provider(Box::new(code_memory::CodeMemory::default()));
    JITModule::new(builder)
}

/// What changes as code is translated.
struct JitState {
    /// Optimized code, and the way in: kept for good.
    module: JITModule,
    /// Quick translations, let go of as a whole when they take too much memory.
    quick: JITModule,
    quick_isa: OwnedTargetIsa,
    /// Bytes of code in `quick`.
    quick_bytes: usize,
    /// Bytes of code in `module`.
    optimized_bytes: usize,
    /// Two-level table: guest page, then instruction within the page.
    pages: Vec<Option<Box<Page>>>,
    /// Instruction count of blocks that are queued, or that failed to translate.
    untranslated: HashMap<u32, u32>,
    /// Blocks being translated for the first time, to optimize once they are hot. Only what
    /// finds them again is kept, not their decoded instructions.
    awaiting: HashMap<u32, Candidate>,
    /// Quickly translated blocks and where they count their runs, until they become hot.
    candidates: Vec<Candidate>,
    /// Where the next hot scan goes on in `candidates`.
    scan_cursor: usize,
    /// Every installed region, to forget them when code changes.
    installed: Vec<Installed>,
    /// Storage of the run counters.
    counters: Vec<Box<[u32; COUNTER_CHUNK]>>,
    counters_used: usize,
    last_hot_scan: Instant,
    board: Arc<JobBoard>,
    finished: Receiver<Translated>,
    last_collect: Instant,
    /// Bumped on invalidation: translations of older code are dropped on arrival.
    generation: u64,
    /// What the first translation of new code takes in.
    first_reach: Reach,
    compiled_blocks: u64,
    compile_nanos: u64,
}

// SAFETY: the module's raw code pointers are only handed out under the `Jit`'s lock.
#[allow(clippy::non_send_fields_in_send_ty, reason = "the raw pointers are code and counters")]
unsafe impl Send for JitState {}

impl std::fmt::Debug for Jit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jit")
            .field("compiled_blocks", &self.compiled_blocks())
            .finish_non_exhaustive()
    }
}

/// The block's calling convention: every block ends in a jump to the next one.
fn block_signature(pointer: Type) -> Signature {
    let mut signature = Signature::new(CallConv::Tail);
    signature.params.extend([AbiParam::new(pointer); 3]);
    signature.returns.push(AbiParam::new(I32));
    signature
}

/// The way from Rust into the tail-calling convention of the blocks.
#[allow(clippy::expect_used, reason = "these few fixed instructions always compile")]
fn compile_enter(module: &mut JITModule, isa: &OwnedTargetIsa) -> EnterFn {
    let pointer = isa.pointer_type();
    let mut signature = Signature::new(isa.default_call_conv());
    signature.params.extend([AbiParam::new(pointer); 4]);
    signature.returns.push(AbiParam::new(I32));
    let id = module
        .declare_function("shocora_enter_block", Linkage::Local, &signature)
        .expect("a fresh module has no such function");
    let mut context = module.make_context();
    context.func.signature = signature;
    let mut builder_context = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        b.seal_block(entry);
        let params = b.block_params(entry).to_vec();
        let block = b.import_signature(block_signature(pointer));
        let call = b.ins().call_indirect(block, params[3], &params[..3]);
        let stop = b.inst_results(call)[0];
        b.ins().return_(&[stop]);
        b.finalize(isa.frontend_config());
    }
    module.define_function(id, &mut context).expect("valid function");
    module.finalize_definitions().expect("memory for a few instructions");
    let address = module.get_finalized_function(id);
    // SAFETY: `address` was just generated with exactly this signature.
    unsafe { std::mem::transmute::<*const u8, EnterFn>(address) }
}

/// Starts the threads translating blocks.
fn spawn_workers(
    isa: &OwnedTargetIsa,
    optimizing_isa: &OwnedTargetIsa,
    pages: usize,
    board: &Arc<JobBoard>,
    finished: &Sender<Translated>,
) {
    let count = std::thread::available_parallelism()
        .map_or(1, |n| n.get().saturating_sub(4))
        .clamp(1, MAX_WORKERS);
    for index in 0..count {
        let (isa, optimizing_isa) = (Arc::clone(isa), Arc::clone(optimizing_isa));
        let (board, finished) = (Arc::clone(board), finished.clone());
        let spawned = std::thread::Builder::new().name("shocora-jit".into()).spawn(move || {
            let new_worker =
                || emit::Worker::new(Arc::clone(&isa), Arc::clone(&optimizing_isa), pages);
            let mut worker = new_worker();
            while let Some(job) = board.next(index < OPTIMIZING_WORKERS) {
                // A panic in the compiler must not lose the job: its block stays
                // interpreted, and whoever waits for it hears about that.
                let translated = catch_unwind(AssertUnwindSafe(|| worker.translate(&job)))
                    .unwrap_or_else(|_| {
                        worker = new_worker();
                        job.failed()
                    });
                if finished.send(translated).is_err() {
                    break;
                }
            }
        });
        if let Err(error) = spawned {
            log::error!("cannot start a JIT thread: {error}");
        }
    }
}

/// The compiler for the host at an optimization level of Cranelift.
#[allow(
    clippy::expect_used,
    reason = "no host without Cranelift support can run shocora at usable speed"
)]
fn host_isa(level: &str) -> OwnedTargetIsa {
    let mut flags = settings::builder();
    flags.set("opt_level", level).expect("known flag");
    flags.set("use_colocated_libcalls", "false").expect("known flag");
    flags.set("is_pic", "false").expect("known flag");
    // Cranelift's tail calls rely on them.
    flags.set("preserve_frame_pointers", "true").expect("known flag");
    let verify = if cfg!(debug_assertions) { "true" } else { "false" };
    flags.set("enable_verifier", verify).expect("known flag");
    cranelift_native::builder()
        .expect("host supported by Cranelift")
        .finish(settings::Flags::new(flags))
        .expect("valid flags")
}

impl Jit {
    /// Creates an empty code cache for the host CPU.
    ///
    /// # Panics
    /// If Cranelift does not support the host (shocora only targets ones it does).
    pub fn new() -> Self {
        let isa = host_isa("none");
        let optimizing_isa = host_isa("speed");
        let mut module = new_module(&isa);
        let quick = new_module(&isa);
        let enter = compile_enter(&mut module, &isa);
        let pages: Vec<Option<Box<Page>>> =
            (0..1_usize << (32 - PAGE_BITS)).map(|_| None).collect();
        let board = Arc::new(JobBoard::default());
        let (finished_sender, finished) = mpsc::channel();
        spawn_workers(&isa, &optimizing_isa, pages.as_ptr() as usize, &board, &finished_sender);
        let table = pages.as_ptr();
        let state = JitState {
            module,
            quick,
            quick_isa: Arc::clone(&isa),
            quick_bytes: 0,
            optimized_bytes: 0,
            pages,
            untranslated: HashMap::new(),
            awaiting: HashMap::new(),
            candidates: Vec::new(),
            scan_cursor: 0,
            installed: Vec::new(),
            counters: Vec::new(),
            counters_used: 0,
            last_hot_scan: Instant::now(),
            board,
            finished,
            last_collect: Instant::now(),
            generation: 0,
            first_reach: Reach::Block,
            compiled_blocks: 0,
            compile_nanos: 0,
        };
        Self {
            state: Mutex::new(state),
            enter,
            pages: table,
            interpreted: AtomicU64::new(0),
            synchronous: std::env::var_os("SHOCORA_JIT_SYNC").is_some(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, JitState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether to wait for each translation instead of interpreting meanwhile.
    #[inline]
    pub const fn set_synchronous(&mut self, synchronous: bool) {
        self.synchronous = synchronous;
    }

    /// Translates new code as regions right away (tests of the region translation).
    #[cfg(test)]
    pub(crate) fn set_regions_first(&self) {
        self.lock().first_reach = Reach::Region;
    }

    /// Host time spent translating so far, summed over the workers.
    #[inline]
    pub fn queue_lengths(&self) -> (usize, usize) {
        let board = Arc::clone(&self.lock().board);
        let queues = board.queues.lock().unwrap_or_else(PoisonError::into_inner);
        (queues.first.len(), queues.optimize.len())
    }

    #[inline]
    pub fn compile_nanos(&self) -> u64 {
        self.lock().compile_nanos
    }

    /// Guest instructions run by the interpreter while waiting for their translation.
    #[inline]
    pub fn interpreted(&self) -> u64 {
        self.interpreted.load(Ordering::Relaxed)
    }

    /// Number of blocks translated so far.
    #[inline]
    pub fn compiled_blocks(&self) -> u64 {
        self.lock().compiled_blocks
    }

    /// Forgets translations of `[address, address + size)` (self-modifying code,
    /// `ICInvalidateRange`).
    pub fn invalidate(&self, address: u32, size: u32) {
        self.lock().invalidate(address, size);
    }

    /// Lets go of quick translations if they take too much memory. Returns whether it did.
    ///
    /// # Safety
    /// No core may be running translated code meanwhile (between frames).
    pub unsafe fn release_cold_code(&self) -> bool {
        self.lock().release_quick_code()
    }

    /// Bytes of translated code: quick translations, then optimized code.
    pub fn code_bytes(&self) -> (usize, usize) {
        let state = self.lock();
        (state.quick_bytes, state.optimized_bytes)
    }

    /// Native code for `pc`, read without the lock.
    #[inline]
    fn lookup(&self, pc: u32) -> Option<*const u8> {
        // SAFETY: the table has an entry for every guest page and lives as long as `self`;
        // see the `Sync` implementation for entries changing meanwhile. An `Option<Box<_>>`
        // is a nullable pointer; the box stays owned by the table.
        let page = unsafe {
            std::ptr::read_volatile(
                self.pages.add((pc >> PAGE_BITS) as usize).cast::<*const Page>(),
            )
        };
        if page.is_null() {
            return None;
        }
        // SAFETY: a page, once there, lives as long as the table.
        let code = unsafe { (*page)[((pc >> 2) as usize) & (PAGE_ENTRIES - 1)] };
        (!code.is_null()).then_some(code)
    }
}

impl JitState {
    fn invalidate(&mut self, address: u32, size: u32) {
        self.generation += 1;
        self.untranslated.clear();
        self.awaiting.clear();
        self.candidates
            .retain(|candidate| !candidate.overlaps(address, address.saturating_add(size)));
        let end = address.saturating_add(size);
        // What is waiting for optimizing elsewhere is still good, and would otherwise stay slow.
        let generation = self.generation;
        self.board.change(|queues| {
            queues.optimize.retain_mut(|job| {
                job.set_generation(generation);
                !job.overlaps(address, end)
            });
        });
        let mut stale = Vec::new();
        self.installed.retain(|installed| {
            let overlaps = installed.range.0 < end && address < installed.range.1;
            if overlaps {
                stale.extend_from_slice(&installed.entries);
            }
            !overlaps
        });
        for at in stale {
            if let Some(page) = &mut self.pages[(at >> PAGE_BITS) as usize] {
                page[((at >> 2) as usize) & (PAGE_ENTRIES - 1)] = std::ptr::null();
            }
        }
    }

    #[inline]
    fn lookup(&self, pc: u32) -> Option<*const u8> {
        let code = self.pages[(pc >> PAGE_BITS) as usize].as_ref()?
            [((pc >> 2) as usize) & (PAGE_ENTRIES - 1)];
        (!code.is_null()).then_some(code)
    }

    fn insert(&mut self, pc: u32, code: *const u8) {
        let page = self.pages[(pc >> PAGE_BITS) as usize]
            .get_or_insert_with(|| Box::new([std::ptr::null(); PAGE_ENTRIES]));
        page[((pc >> 2) as usize) & (PAGE_ENTRIES - 1)] = code;
    }

    /// Hands the code of a finished translation to the module.
    fn define(&mut self, translated: &Translated) -> Option<FuncId> {
        self.compile_nanos += translated.nanos;
        if translated.generation != self.generation {
            return None;
        }
        let Some(code) = &translated.code else {
            log::error!("JIT failed at {:#010x}, it stays interpreted", translated.pc);
            return None;
        };
        let signature = block_signature(self.module.isa().pointer_type());
        let module = if translated.optimized { &mut self.module } else { &mut self.quick };
        let id = module.declare_anonymous_function(&signature).ok()?;
        module.define_function_bytes(id, translated.alignment, code, &[]).ok()?;
        if translated.optimized {
            self.optimized_bytes += code.len();
        } else {
            self.quick_bytes += code.len();
        }
        Some(id)
    }

    /// Takes in what the workers finished. Everything becomes executable at once:
    /// changing page protection per block would be a large part of translating.
    fn collect(&mut self, first: Option<Translated>) {
        self.last_collect = Instant::now();
        let batch: Vec<_> = first.into_iter().chain(self.finished.try_iter()).collect();
        if batch.is_empty() {
            return;
        }
        let defined: Vec<_> =
            batch.into_iter().filter_map(|t| Some((self.define(&t)?, t))).collect();
        if let Err(error) =
            self.module.finalize_definitions().and_then(|()| self.quick.finalize_definitions())
        {
            log::error!("JIT: cannot make code executable: {error}");
            return;
        }
        for (id, Translated { pc, entries, range, optimized, .. }) in defined {
            let module = if optimized { &self.module } else { &self.quick };
            let code = module.get_finalized_function(id);
            // A return address that already has code keeps it, unless this is optimized code.
            for &entry in &entries {
                if entry == pc || optimized || self.lookup(entry).is_none() {
                    self.insert(entry, code);
                }
            }
            self.installed.push(Installed { range, entries, code, quick: !optimized });
            if !optimized {
                self.untranslated.remove(&pc);
                self.compiled_blocks += 1;
                if let Some(candidate) = self.awaiting.remove(&pc) {
                    self.candidates.push(candidate);
                }
            }
        }
    }

    /// Queues the blocks that ran often enough since the last look for optimizing: as a
    /// region, so their loops and the code their calls return to run without leaving.
    fn optimize_hot_blocks<B: Bus>(&mut self, bus: &mut B) {
        self.last_hot_scan = Instant::now();
        let mut hot_pcs = Vec::new();
        // A batch at a time, going round: a block that turns hot waits a few scans at most.
        if self.scan_cursor >= self.candidates.len() {
            self.scan_cursor = 0;
        }
        let mut index = self.scan_cursor;
        let end = (index + HOT_SCAN_BATCH).min(self.candidates.len());
        let mut end_index = end;
        while index < end_index {
            let candidate = self.candidates[index];
            // SAFETY: counters live as long as the `Jit`; a racing increment from another
            // core only makes the count late.
            let runs = unsafe { candidate.counter.read_volatile() };
            if runs >= HOT_RUNS {
                hot_pcs.push(candidate.pc);
                self.candidates.swap_remove(index);
                end_index = end_index.min(self.candidates.len());
            } else {
                index += 1;
            }
        }
        self.scan_cursor = index;
        let generation = self.generation;
        let hot: Vec<_> = hot_pcs
            .into_iter()
            .filter_map(|pc| emit::decode_block(pc, bus, generation, Reach::Region).ok())
            .map(Job::into_optimized)
            .collect();
        if !hot.is_empty() {
            self.board.change(|queues| queues.optimize.extend(hot));
        }
    }

    /// A fresh run counter for a block's quick translation.
    fn new_counter(&mut self) -> *mut u32 {
        let index = self.counters_used % COUNTER_CHUNK;
        if index == 0 {
            self.counters.push(Box::new([0; COUNTER_CHUNK]));
        }
        self.counters_used += 1;
        let chunk = self.counters.last_mut().map_or(std::ptr::null_mut(), |c| c.as_mut_ptr());
        // SAFETY: `index` is within the chunk just taken from.
        unsafe { chunk.add(index) }
    }

    /// Takes in what the workers finished, unless that was just done.
    fn collect_if_due<B: Bus>(&mut self, bus: &mut B) {
        if self.last_collect.elapsed() >= COLLECT_INTERVAL {
            self.collect(None);
        }
        if self.last_hot_scan.elapsed() >= HOT_SCAN_INTERVAL {
            self.optimize_hot_blocks(bus);
        }
    }

    /// Queues the block at `pc`, unless it is on its way. Returns how many
    /// instructions to interpret meanwhile.
    fn queue<B: Bus>(&mut self, pc: u32, bus: &mut B) -> Result<u32, CpuError> {
        if let Some(count) = self.untranslated.get(&pc) {
            return Ok(*count);
        }
        let mut job = emit::decode_block::<B>(pc, bus, self.generation, self.first_reach)?;
        let counter = self.new_counter();
        job.count_runs_at(counter);
        let count = job.instructions.len() as u32;
        self.untranslated.insert(pc, count);
        self.awaiting.insert(pc, Candidate { pc, range: job.range(), counter });
        self.board.change(|queues| queues.first.push_back(job));
        Ok(count)
    }

    /// Lets go of every quick translation once they take more than [`QUICK_CODE_BUDGET`].
    /// Returns whether it did.
    fn release_quick_code(&mut self) -> bool {
        if self.quick_bytes <= QUICK_CODE_BUDGET {
            return false;
        }
        let mut released = Vec::new();
        self.installed.retain_mut(|installed| {
            if installed.quick {
                released.push((std::mem::take(&mut installed.entries), installed.code));
            }
            !installed.quick
        });
        for (entries, code) in released {
            for at in entries {
                // Entries optimized code took over since stay.
                if let Some(page) = &mut self.pages[(at >> PAGE_BITS) as usize] {
                    let slot = &mut page[((at >> 2) as usize) & (PAGE_ENTRIES - 1)];
                    if *slot == code {
                        *slot = std::ptr::null();
                    }
                }
            }
        }
        // They count runs of code that is gone; blocks translated again count anew.
        self.candidates.clear();
        self.scan_cursor = 0;
        let old = std::mem::replace(&mut self.quick, new_module(&self.quick_isa));
        // SAFETY: the caller guarantees that no core runs translated code, and the page
        // table no longer leads to any of it.
        unsafe { old.free_memory() };
        log::info!("JIT: let go of {} MB of quick translations", self.quick_bytes >> 20);
        self.quick_bytes = 0;
        true
    }

    /// Waits until the block at `pc` is translated, or known not to translate.
    fn wait_for(&mut self, pc: u32) {
        while self.lookup(pc).is_none() {
            let Ok(translated) = self.finished.recv() else { return };
            let failed = translated.pc == pc && translated.code.is_none() && !translated.optimized;
            self.collect(Some(translated));
            if failed {
                return;
            }
        }
    }
}

impl Jit {
    /// Runs until an event other than [`StepEvent::Continue`] occurs or at
    /// least `budget` instructions have executed. Falls back to the interpreter
    /// when the bus has no flat memory. Every core may run at the same time.
    ///
    /// # Errors
    /// See [`step`].
    pub fn run<B: Bus>(
        &self,
        state: &mut CpuState,
        bus: &mut B,
        budget: u64,
    ) -> Result<(StepEvent, u64), CpuError> {
        let Some(memory) = bus.fast_memory() else {
            return crate::interpreter::run(state, bus, budget);
        };
        // When another core holds the lock, it takes in the translations itself.
        let calls = RUN_CALLS.with(|calls| {
            calls.set(calls.get().wrapping_add(1));
            calls.get()
        });
        if calls.is_multiple_of(HOUSEKEEPING_CALLS) {
            if let Ok(mut jit) = self.state.try_lock() {
                jit.collect_if_due(bus);
            }
        }
        let budget = budget as i64;
        let mut context =
            HelperContext { bus: std::ptr::from_mut(bus).cast(), result: None, remaining: budget };
        while context.remaining > 0 {
            let pc = state.pc;
            if let Some(code) = self.lookup(pc) {
                // SAFETY: the code was generated for this `CpuState` layout, and
                // `memory` is the base of the guest's flat address space.
                let stop = unsafe {
                    (self.enter)(std::ptr::from_mut(state), memory, &raw mut context, code)
                };
                if stop != 0 {
                    if let Some(result) = context.result.take() {
                        return result.map(|event| (event, (budget - context.remaining) as u64));
                    }
                }
                continue;
            }
            // SAFETY: `context.bus` is the live `bus`; nothing else uses it meanwhile.
            let bus = unsafe { &mut *context.bus.cast::<B>() };
            let count = {
                let mut jit = self.lock();
                let count = jit.queue(pc, bus)?;
                jit.collect_if_due(bus);
                if self.synchronous {
                    jit.wait_for(pc);
                }
                count
            };
            if self.synchronous && self.lookup(pc).is_some() {
                continue;
            }
            if let Some(event) = self.interpret(state, bus, count, &mut context)? {
                return Ok((event, (budget - context.remaining) as u64));
            }
        }
        Ok((StepEvent::Continue, (budget - context.remaining) as u64))
    }

    /// Steps through an untranslated block until it ends or translated code is reached.
    fn interpret<B: Bus>(
        &self,
        state: &mut CpuState,
        bus: &mut B,
        count: u32,
        context: &mut HelperContext,
    ) -> Result<Option<StepEvent>, CpuError> {
        let mut interpreted = 0;
        let mut result = Ok(None);
        for _ in 0..count {
            let next = state.pc.wrapping_add(4);
            let event = match step(state, bus) {
                Ok(event) => event,
                Err(error) => {
                    result = Err(error);
                    break;
                }
            };
            context.remaining -= 1;
            interpreted += 1;
            if event != StepEvent::Continue {
                result = Ok(Some(event));
                break;
            }
            if state.pc != next || self.lookup(state.pc).is_some() {
                break;
            }
        }
        self.interpreted.fetch_add(interpreted, Ordering::Relaxed);
        result
    }
}

impl Drop for JitState {
    fn drop(&mut self) {
        self.board.change(|queues| queues.closed = true);
    }
}

impl Default for Jit {
    fn default() -> Self {
        Self::new()
    }
}
