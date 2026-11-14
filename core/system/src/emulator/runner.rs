//! The frame loop: run the main core, dispatch HLE calls, stop on events.
use std::time::{Duration, Instant};

use wiiu_cafe_os::{HleContext, coreinit::thread::TIME_SLICE, dispatch};
use wiiu_cpu::{CpuError, StepEvent, disassemble::disassemble};

use super::{Emulator, MAIN_CORE, Status, bus::CoreBus};

/// A blocked guest is checked this often for expired sleeps, alarms and the like.
const IDLE_POLL: Duration = Duration::from_millis(1);
/// The last part of a wait for the vertical blank spins: sleeping can overshoot by this much.
/// Spinning keeps a core busy, so it is as little as keeps frames on time.
const SPIN_MARGIN: Duration = Duration::from_micros(500);
/// Further behind the display than this, real time is given up and re-based.
const MAX_LAG_FRAMES: u32 = 4;

/// Waits for `deadline` without oversleeping it.
fn wait_until(deadline: Instant) {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining > SPIN_MARGIN {
        std::thread::sleep(remaining - SPIN_MARGIN);
    }
    while Instant::now() < deadline {
        std::thread::yield_now();
    }
}

impl Emulator {
    /// Run the guest until the next vertical blank (VBlank).
    ///
    /// - Trigger a VBlank interrupt when reached.
    /// - Guest sleep and alarm timers continue progressing while idle.
    /// - VBlank occurs even if the guest is still busy.
    /// - When paced is disabled:
    ///   VBlank occurs when the guest becomes idle or the instruction budget is exhausted.
    pub fn run_frame(&mut self) {
        // 1. Check emulator status; return if not running.
        if self.status != Status::Running {
            return;
        }

        // 2. Apply cheats to memory.
        crate::cheat::apply(&self.cheats, &mut self.memory);

        // Vertical blanks come as often as the guest's time says.
        let frame_time = self.config.frame_time * 2 / self.speed.halves();
        let now = Instant::now();

        // 3. Calculate the next VSync deadline.
        let deadline = match self.next_vsync {
            Some(vsync) if now < vsync + frame_time * MAX_LAG_FRAMES => vsync,
            _ => now + frame_time,
        };
        self.next_vsync = Some(deadline + frame_time);

        // 4. Update the GPU retired counter.
        // Threads waiting for the GPU look at its progress themselves.
        self.os.coreinit.threads.gpu_retired = self.gpu.retired_counter();

        // 5. Switch to multi-core mode if enabled.
        // Run cores in parallel; main core handles frames.
        if self.switch_core_mode() {
            // 6. Run the multi-core frame loop and finalize the frame.
            self.run_frame_multi_core(deadline);
            if let Some(code) = self.os.exit_code {
                self.status = Status::Exited(code);
            }

            self.finish_gx2_frame();
            self.stats.frames += 1;
            return;
        }

        // 7. Initialize the instruction budget and time slice.
        // Frame budget: Next VSync counts per frame
        let mut budget = self.config.instructions_per_frame;
        let mut slice = TIME_SLICE;

        // 8. Process pending alarms, callbacks, and runnable threads.
        if let Err(error) = self.os.begin_frame(&mut self.cores[MAIN_CORE], &mut self.memory) {
            self.crash(error.to_string());
            return;
        }

        // 9. Run the main CPU core until VBlank or the instruction budget expires.
        // Frame loop: run the main core until the next vertical blank.
        while budget > 0 {
            if self.os.exit_code.is_some() {
                break;
            }

            if self.os.frame_done {
                // 10. Wait until the VSync deadline if pacing is enabled.
                if !self.config.paced || Instant::now() >= deadline {
                    break;
                }

                // Sleeps, like Cemu's waiting threads: spinning through every idle moment would
                // keep a whole core at full load for nothing.
                if deadline.saturating_duration_since(Instant::now()) > IDLE_POLL + SPIN_MARGIN {
                    std::thread::sleep(IDLE_POLL);
                } else {
                    wait_until(deadline);
                }

                // Housekeeping: check for expired sleeps, alarms, and the like.
                if let Err(error) =
                    self.os.begin_frame(&mut self.cores[MAIN_CORE], &mut self.memory)
                {
                    self.crash(error.to_string());
                    return;
                }
                continue;
            }

            let timer = self.config.profile.then(Instant::now);
            let result = {
                // Bus: CPU accesses to memory and peripherals, plus the timebase.
                let mut bus = CoreBus {
                    memory: &mut self.memory,
                    timebase: self.os.timebase,
                    jit_memory: self.config.jit,
                };
                // The JIT can access memory directly.
                self.jit.run(&mut self.cores[MAIN_CORE], &mut bus, budget.min(slice))
            };

            if let Some(timer) = timer {
                self.stats.guest_nanos += timer.elapsed().as_nanos() as u64;
            }

            let (event, executed) = match result {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.crash_on_cpu_error(&error);
                    return;
                }
            };

            self.stats.instructions += executed;
            budget = budget.saturating_sub(executed);
            slice = slice.saturating_sub(executed);

            // 10. Handle CPU events (continue, HLE calls, syscalls, traps).
            match event {
                // 11. Yield to other threads when the time slice expires.
                StepEvent::Continue => {
                    if slice == 0 {
                        // Let other game threads run after the current thread uses its allotted CPU time.
                        slice = TIME_SLICE;
                        if self.config.profile {
                            *self.slice_ends.entry(self.cores[MAIN_CORE].pc).or_insert(0) += 1;
                        }

                        if let Err(error) =
                            self.os.end_time_slice(&mut self.cores[MAIN_CORE], &mut self.memory)
                        {
                            self.crash(error.to_string());
                            return;
                        }
                    }
                }

                // Prepare the HLE execution context and start profiling.
                StepEvent::HleCall(index) => {
                    self.stats.hle_calls += 1;
                    let timer = self.config.profile.then(Instant::now);
                    let mut ctx = HleContext {
                        cpu: &mut self.cores[MAIN_CORE],
                        memory: &mut self.memory,
                        os: &mut self.os,
                        gpu: &mut self.gpu,
                        iosu: &mut self.iosu,
                    };

                    if let Err(error) = dispatch(&mut ctx, index) {
                        self.crash(format!("{error} (lr {:#010x})", self.cores[MAIN_CORE].lr));
                        return;
                    }

                    if let Some(timer) = timer {
                        let nanos = timer.elapsed().as_nanos() as u64;
                        self.stats.hle_nanos += nanos;
                        self.os.registry.note_time(index, nanos);
                    }
                }

                // Stop emulation when the game calls an unsupported system function.
                StepEvent::SystemCall => {
                    self.crash(format!(
                        "unsupported syscall at {:#010x}",
                        self.cores[MAIN_CORE].pc
                    ));
                    return;
                }

                // Stop emulation when the game triggers a CPU exception (trap).
                StepEvent::Trap => {
                    self.crash(format!(
                        "trap at {:#010x}",
                        self.cores[MAIN_CORE].pc.wrapping_sub(4)
                    ));
                    return;
                }
            }

            if Instant::now() >= deadline {
                break;
            }
        }

        // 12. Wait until the VSync deadline if pacing is enabled.
        // 13. Update exit status.
        if let Some(code) = self.os.exit_code {
            self.status = Status::Exited(code);
        } else if self.config.paced {
            wait_until(deadline);
        }

        // 14. Finalize the GX2 frame and frame statistics.
        self.finish_gx2_frame();
        self.stats.frames += 1;
    }

    /// Goes over to the mode [`Config::multi_core`](super::Config::multi_core) asks for, at
    /// a moment no guest callback borrows a core. Returns whether the cores run side by side.
    fn switch_core_mode(&mut self) -> bool {
        let wanted = self.config.multi_core && self.config.paced && self.config.jit;
        if wanted != self.multi_core_running && !self.os.callbacks.active() {
            self.os.coreinit.threads.park(&self.cores, MAIN_CORE);
            self.os.coreinit.threads.multi_core = wanted;
            self.multi_core_running = wanted;
        }
        self.multi_core_running
    }

    /// What happens at every vertical blank.
    fn finish_gx2_frame(&mut self) {
        // SAFETY: between frames no core runs translated code: side by side, the cores'
        // threads have ended; on one thread, `run_frame` has left the JIT.
        unsafe { self.jit.release_cold_code() };
        self.os.vsync();
    }

    fn crash_on_cpu_error(&mut self, error: &CpuError) {
        let pc = self.cores[MAIN_CORE].pc;
        let context = self
            .memory
            .read_u32(pc)
            .map_or_else(|_| String::new(), |raw| format!(": {}", disassemble(pc, raw)));
        self.crash(format!("{error}{context}"));
    }

    fn crash(&mut self, message: String) {
        log::error!("emulation stopped: {message}");
        self.status = Status::Crashed(message);
    }
}
