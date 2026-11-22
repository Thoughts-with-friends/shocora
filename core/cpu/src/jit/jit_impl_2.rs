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
//! │ blocks of game     │   │ jit/jit_impl_2.rs        │   │ native x86/ARM     │
//! │ code               │──►│ ──────────────────────── │──►│ code               │
//! │                    │   │ JIT recompiler: jit impl │   │                    │
//! │                    │   │ 2                        │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "jit impl 2" share of mod.rs (JIT recompiler), moved here so that no file is longer than
//! 500 lines. It works exactly as if it were still written in that file. The fast way to run
//! game code: translate blocks of PowerPC code into your PC's own machine code once, then run
//! that directly. Like shader compilation, the first time costs a little. Terms such as HLE,
//! JIT or shader are explained in `docs/guide/wiiu_term.md`.

use super::*;

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
    pub(super) fn interpret<B: Bus>(
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
