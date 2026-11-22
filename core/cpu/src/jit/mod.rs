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
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                         ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ blocks of game     │   │ jit/mod.rs               │   │ native x86/ARM     │
//! │ code               │──►│ ──────────────────────── │──►│ code               │
//! │                    │   │ JIT recompiler           │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The fast way to run game code: translate blocks of PowerPC code into your PC's own machine
//! code once, then run that directly. Like shader compilation, the first time costs a little.
//! Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
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

mod jit_impl;
mod jit_impl_2;
#[allow(unused_imports)]
pub(crate) use jit_impl::*;
#[allow(unused_imports)]
pub(crate) use jit_impl_2::*;
