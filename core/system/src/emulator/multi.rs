//! The frame loop with the three cores side by side, like Cemu's multi-core mode.
//!
//! ```text
//!  run_frame ──► core 0 thread ─┐   guest code: in parallel, on shared memory
//!            ──► core 1 (here) ─┼─► HLE calls and scheduling: one core at a time,
//!            ──► core 2 thread ─┘   under the machine lock
//!                     │ deadline
//!                     ▼
//!                 vertical blank
//! ```
//!
//! Each core runs its own threads with its own registers until the vertical blank is due.
//! What the cores share in the OS (threads, mutexes, the GPU) is behind one lock, which a
//! core takes for an HLE call or a scheduling decision only: Cemu's coreinit does the same
//! with its scheduler lock. A core with nothing to run waits for another core's HLE call,
//! which is what makes threads runnable.
use std::{
    sync::{
        Condvar, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use wiiu_cafe_os::{
    CALLBACK_CORE, CafeOs, HleContext,
    coreinit::thread::{Outcome, Reschedule, TIME_SLICE},
    dispatch,
    fast::{self, FastCalls},
};
use wiiu_cpu::{CpuError, CpuState, Jit, StepEvent, disassemble::disassemble};
use wiiu_gpu::{Gpu, WriteNotifier};
use wiiu_iosu::Iosu;
use wiiu_memory::Memory;
use wiiu_peripherals::TimeBase;

use super::{CORE_COUNT, Emulator, Status, bus::CoreBus};

/// A core with nothing to run looks again this often, for sleeps and alarms that are due.
const IDLE_WAIT: Duration = Duration::from_micros(500);
/// Turns of a core's loop between looks at the clock for the vertical blank.
const DEADLINE_TURNS: u32 = 16;

/// What the cores share, behind the machine lock.
struct Machine<'a> {
    os: &'a mut CafeOs,
    gpu: &'a mut Gpu,
    iosu: &'a mut Iosu,
}

/// The machine lock, and a wake-up for idle cores whenever a core changed something.
struct Shared<'a> {
    machine: Mutex<Machine<'a>>,
    changed: Condvar,
    /// Set when a core crashes or the title exits: the others stop too.
    stop: AtomicBool,
    /// Host time each core waited for the lock, when profiling.
    lock_waits: [AtomicU64; CORE_COUNT],
    profile: bool,
}

impl<'a> Shared<'a> {
    /// The machine, for `core`.
    fn lock(&self, core: u32) -> MutexGuard<'_, Machine<'a>> {
        if !self.profile {
            return self.machine.lock().unwrap_or_else(PoisonError::into_inner);
        }

        let timer = Instant::now();
        let guard = self.machine.lock().unwrap_or_else(PoisonError::into_inner);
        self.lock_waits[core as usize]
            .fetch_add(timer.elapsed().as_nanos() as u64, Ordering::Relaxed);

        guard
    }
}

/// What one core did during the frame.
#[derive(Default)]
struct CoreReport {
    instructions: u64,
    hle_calls: u64,
    core: usize,
    guest_nanos: u64,
    hle_nanos: u64,
    idle_nanos: u64,
    fast_nanos: u64,
    core_nanos: u64,
    crash: Option<String>,
}

/// What every core needs to run.
#[derive(Clone, Copy)]
struct CoreSetup<'s, 'a> {
    shared: &'s Shared<'a>,
    jit: &'s Jit,
    /// HLE calls that run without the lock, and how they tell the GPU what they wrote.
    fast: &'s FastCalls,
    writes: Option<&'s WriteNotifier>,
    timebase: TimeBase,
    deadline: Instant,
    jit_memory: bool,
    profile: bool,
}

impl Emulator {
    /// [`Self::run_frame`] with each core on a host thread of its own.
    pub(super) fn run_frame_multi_core(&mut self, deadline: Instant) {
        self.os.coreinit.threads.multi_core = true;

        let timebase = self.os.timebase;
        let fast = FastCalls::new(&self.os.registry);
        let writes = self.gpu.write_notifier();
        let (jit_memory, profile) = (self.config.jit, self.config.profile);

        // Cores 0 and 2 see guest memory through handles of their own.
        let mut memories: Vec<Memory> = (0..CORE_COUNT).map(|_| self.memory.share()).collect();

        let shared = Shared {
            machine: Mutex::new(Machine {
                os: &mut self.os,
                gpu: &mut self.gpu,
                iosu: &mut self.iosu,
            }),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
            lock_waits: Default::default(),
            profile,
        };

        let setup = CoreSetup {
            shared: &shared,
            jit: &self.jit,
            fast: &fast,
            writes: writes.as_ref(),
            timebase,
            deadline,
            jit_memory,
            profile,
        };

        let main_memory = &mut self.memory;
        let reports: Vec<CoreReport> = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            let mut main = None;

            for (core, (cpu, memory)) in self.cores.iter_mut().zip(memories.iter_mut()).enumerate()
            {
                if core as u32 == CALLBACK_CORE {
                    main = Some(cpu);
                    continue;
                }

                let spawned = std::thread::Builder::new()
                    .name(format!("shocora core {core}"))
                    .spawn_scoped(scope, move || run_core(core as u32, cpu, memory, setup));

                match spawned {
                    Ok(handle) => handles.push(handle),
                    Err(error) => log::error!("cannot start core {core}: {error}"),
                }
            }

            let mut reports = Vec::new();
            if let Some(cpu) = main {
                reports.push(run_core(CALLBACK_CORE, cpu, main_memory, setup));
            }

            for handle in handles {
                reports.push(handle.join().unwrap_or_else(|_| CoreReport {
                    crash: Some("a core thread panicked".to_owned()),
                    ..CoreReport::default()
                }));
            }
            reports
        });

        let lock_nanos: u64 =
            shared.lock_waits.iter().map(|wait| wait.load(Ordering::Relaxed)).sum();
        self.stats.lock_nanos += lock_nanos;

        // Regions a core mapped (an HLE call) belong to all of them.
        for memory in &memories {
            self.memory.adopt_regions(memory);
        }

        for report in reports {
            self.stats.instructions += report.instructions;
            self.stats.hle_calls += report.hle_calls;
            self.stats.guest_nanos += report.guest_nanos;
            self.stats.hle_nanos += report.hle_nanos;
            self.stats.idle_nanos += report.idle_nanos;
            self.stats.fast_nanos += report.fast_nanos;
            self.stats.core_nanos += report.core_nanos;

            if let Some(nanos) = self.stats.core_guest_nanos.get_mut(report.core) {
                *nanos += report.guest_nanos;
            }

            if let Some(message) = report.crash {
                if self.status == Status::Running {
                    log::error!("emulation stopped: {message}");
                    self.status = Status::Crashed(message);
                }
            }
        }
    }
}

/// Runs core `core` until the deadline.
fn run_core(
    core: u32,
    cpu: &mut CpuState,
    memory: &mut Memory,
    setup: CoreSetup<'_, '_>,
) -> CoreReport {
    let started = setup.profile.then(Instant::now);
    let mut report = run_core_until(core, cpu, memory, setup);

    if let Some(started) = started {
        report.core_nanos = started.elapsed().as_nanos() as u64;
    }
    report
}

fn run_core_until(
    core: u32,
    cpu: &mut CpuState,
    memory: &mut Memory,
    setup: CoreSetup<'_, '_>,
) -> CoreReport {
    let shared = setup.shared;
    let mut report = CoreReport { core: core as usize, ..CoreReport::default() };
    let mut slice = TIME_SLICE;

    // The scheduler has a look before anything runs: the core may have nothing to do.
    let mut reschedule = true;

    // The clock is looked at every few turns: a turn is often one short HLE call, and the
    // clock cost as much as the call.
    let mut turns = 0_u32;

    while !shared.stop.load(Ordering::Relaxed) {
        turns = turns.wrapping_add(1);
        if (reschedule || turns.is_multiple_of(DEADLINE_TURNS)) && Instant::now() >= setup.deadline
        {
            break;
        }

        if reschedule {
            let timer = setup.profile.then(Instant::now);
            match find_work(core, cpu, memory, setup) {
                Err(message) => return stop_with(shared, report, message),
                Ok(found) => reschedule = !found,
            }
            if reschedule {
                if let Some(timer) = timer {
                    report.idle_nanos += timer.elapsed().as_nanos() as u64;
                }
                continue;
            }
        }

        let timer = setup.profile.then(Instant::now);
        let result = {
            let mut bus = CoreBus {
                memory: &mut *memory,
                timebase: setup.timebase,
                jit_memory: setup.jit_memory,
            };
            setup.jit.run(cpu, &mut bus, slice)
        };

        if let Some(timer) = timer {
            report.guest_nanos += timer.elapsed().as_nanos() as u64;
        }

        let (event, executed) = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                let message = cpu_error(cpu, memory, &error);
                return stop_with(shared, report, message);
            }
        };
        report.instructions += executed;
        slice = slice.saturating_sub(executed);

        match event {
            StepEvent::Continue => {
                if slice == 0 {
                    // Time slice used up: threads of equal priority on this core get a turn.
                    slice = TIME_SLICE;
                    let ended = end_time_slice(core, cpu, memory, &mut shared.lock(core));
                    match ended {
                        Err(message) => return stop_with(shared, report, message),
                        Ok(idle) => reschedule = idle,
                    }
                }
            }
            StepEvent::HleCall(index) => {
                report.hle_calls += 1;
                if let Some(call) =
                    setup.fast.get(index).filter(|call| !call.writes() || setup.writes.is_some())
                {
                    let timer = setup.profile.then(Instant::now);
                    let mut written = |address, size| {
                        if let Some(writes) = setup.writes {
                            writes.note(address, size);
                        }
                    };
                    match fast::run(call, cpu, memory, &setup.timebase, &mut written) {
                        Err(error) => {
                            let message = format!("{error} (lr {:#010x})", cpu.lr);
                            return stop_with(shared, report, message);
                        }
                        Ok(done) => {
                            if let Some(timer) = timer {
                                report.fast_nanos += timer.elapsed().as_nanos() as u64;
                            }
                            if done {
                                continue;
                            }
                        }
                    }
                }
                match locked_call(core, index, cpu, memory, setup, &mut report) {
                    Err(message) => return stop_with(shared, report, message),
                    Ok(idle) => reschedule = idle,
                }
            }
            StepEvent::SystemCall => {
                let message = format!("unsupported syscall at {:#010x}", cpu.pc);
                return stop_with(shared, report, message);
            }
            StepEvent::Trap => {
                let message = format!("trap at {:#010x}", cpu.pc.wrapping_sub(4));
                return stop_with(shared, report, message);
            }
        }
    }
    report
}

/// An HLE call under the machine lock. Returns whether the core is left with nothing to run.
fn locked_call(
    core: u32,
    index: u32,
    cpu: &mut CpuState,
    memory: &mut Memory,
    setup: CoreSetup<'_, '_>,
    report: &mut CoreReport,
) -> Result<bool, String> {
    let timer = setup.profile.then(Instant::now);
    let mut guard = setup.shared.lock(core);
    let machine = &mut *guard;

    let threads = &mut machine.os.coreinit.threads;
    threads.enter_core(core);
    let requests = threads.requests;

    let result = dispatch(
        &mut HleContext {
            cpu: &mut *cpu,
            memory: &mut *memory,
            os: &mut *machine.os,
            gpu: &mut *machine.gpu,
            iosu: &mut *machine.iosu,
        },
        index,
    );

    let os = &mut *machine.os;
    os.coreinit.threads.leave_core(core);

    result.map_err(|error| format!("{error} (lr {:#010x})", cpu.lr))?;

    if os.exit_code.is_some() {
        setup.shared.stop.store(true, Ordering::Relaxed);
    }

    let idle = !os.coreinit.threads.current_is_running() && !os.callback_active_on(core);
    if let Some(timer) = timer {
        let nanos = timer.elapsed().as_nanos() as u64;
        report.hle_nanos += nanos;
        os.registry.note_time(index, nanos);
    }

    // The call may have made a thread of another core runnable: the calls that do ask for
    // a scheduling decision.
    let woke = os.coreinit.threads.requests != requests;
    drop(guard);

    if woke {
        setup.shared.changed.notify_all();
    }

    Ok(idle)
}

/// Looks for something to run; with nothing, waits until another core changes something or
/// time passes. Returns whether the core has something.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the guard is handed to the wait, which releases it"
)]
fn find_work(
    core: u32,
    cpu: &mut CpuState,
    memory: &mut Memory,
    setup: CoreSetup<'_, '_>,
) -> Result<bool, String> {
    let mut machine = setup.shared.lock(core);
    let found = housekeeping(core, cpu, memory, &mut machine);

    if found != Ok(false) {
        drop(machine);
        return found;
    }

    let wait = setup.deadline.saturating_duration_since(Instant::now()).min(IDLE_WAIT);
    drop(setup.shared.changed.wait_timeout(machine, wait));

    Ok(false)
}

/// Sound, alarms and callbacks (on the main core), then something to run. Returns whether
/// the core has something.
fn housekeeping(
    core: u32,
    cpu: &mut CpuState,
    memory: &mut Memory,
    machine: &mut Machine<'_>,
) -> Result<bool, String> {
    let os = &mut *machine.os;
    os.coreinit.threads.enter_core(core);

    let result = (|| {
        if core == CALLBACK_CORE {
            os.sound_tick(memory);
            os.fire_due_alarms();
            if os.callbacks.active() || os.start_callback(cpu, memory)? {
                return Ok(true);
            }
        }
        if os.coreinit.threads.current_is_running() {
            return Ok(true);
        }
        Ok(os.schedule(cpu, memory, Reschedule::Block) != Outcome::Idle)
    })();

    os.coreinit.threads.leave_core(core);
    result.map_err(|error: wiiu_memory::MemoryError| error.to_string())
}

/// The end of a time slice on `core`. Returns whether the core is left with nothing to run.
fn end_time_slice(
    core: u32,
    cpu: &mut CpuState,
    memory: &mut Memory,
    machine: &mut Machine<'_>,
) -> Result<bool, String> {
    let os = &mut *machine.os;
    os.coreinit.threads.enter_core(core);

    let result = if core == CALLBACK_CORE {
        os.end_time_slice(cpu, memory).map_err(|error| error.to_string())
    } else {
        os.schedule(cpu, memory, Reschedule::Yield);
        Ok(())
    };

    os.coreinit.threads.leave_core(core);
    result.map(|()| !os.coreinit.threads.current_is_running() && !os.callback_active_on(core))
}

fn stop_with(shared: &Shared<'_>, mut report: CoreReport, message: String) -> CoreReport {
    shared.stop.store(true, Ordering::Relaxed);
    shared.changed.notify_all();
    report.crash = Some(message);
    report
}

fn cpu_error(cpu: &CpuState, memory: &Memory, error: &CpuError) -> String {
    let context = memory
        .read_u32(cpu.pc)
        .map_or_else(|_| String::new(), |raw| format!(": {}", disassemble(cpu.pc, raw)));
    format!("{error}{context}")
}
