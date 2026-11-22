//! Part of [`super`]: see its documentation.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                         ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ blocks of game     │   │ jit/jit_impl.rs          │   │ native x86/ARM     │
//! │ code               │──►│ ──────────────────────── │──►│ code               │
//! │                    │   │ JIT recompiler: jit impl │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "jit impl" share of mod.rs (JIT recompiler), moved here so that no file is longer than
//! 500 lines. It works exactly as if it were still written in that file. The fast way to run
//! game code: translate blocks of PowerPC code into your PC's own machine code once, then run
//! that directly. Like shader compilation, the first time costs a little. Terms such as HLE,
//! JIT or shader are explained in `docs/guide/wiiu_term.md`.

use super::*;

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

    pub(super) fn lock(&self) -> std::sync::MutexGuard<'_, JitState> {
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
    pub(super) fn lookup(&self, pc: u32) -> Option<*const u8> {
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
    pub(super) fn invalidate(&mut self, address: u32, size: u32) {
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
    pub(super) fn lookup(&self, pc: u32) -> Option<*const u8> {
        let code = self.pages[(pc >> PAGE_BITS) as usize].as_ref()?
            [((pc >> 2) as usize) & (PAGE_ENTRIES - 1)];
        (!code.is_null()).then_some(code)
    }

    pub(super) fn insert(&mut self, pc: u32, code: *const u8) {
        let page = self.pages[(pc >> PAGE_BITS) as usize]
            .get_or_insert_with(|| Box::new([std::ptr::null(); PAGE_ENTRIES]));
        page[((pc >> 2) as usize) & (PAGE_ENTRIES - 1)] = code;
    }

    /// Hands the code of a finished translation to the module.
    pub(super) fn define(&mut self, translated: &Translated) -> Option<FuncId> {
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
    pub(super) fn collect(&mut self, first: Option<Translated>) {
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
    pub(super) fn optimize_hot_blocks<B: Bus>(&mut self, bus: &mut B) {
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
    pub(super) fn new_counter(&mut self) -> *mut u32 {
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
    pub(super) fn collect_if_due<B: Bus>(&mut self, bus: &mut B) {
        if self.last_collect.elapsed() >= COLLECT_INTERVAL {
            self.collect(None);
        }
        if self.last_hot_scan.elapsed() >= HOT_SCAN_INTERVAL {
            self.optimize_hot_blocks(bus);
        }
    }

    /// Queues the block at `pc`, unless it is on its way. Returns how many
    /// instructions to interpret meanwhile.
    pub(super) fn queue<B: Bus>(&mut self, pc: u32, bus: &mut B) -> Result<u32, CpuError> {
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
    pub(super) fn release_quick_code(&mut self) -> bool {
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
    pub(super) fn wait_for(&mut self, pc: u32) {
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
