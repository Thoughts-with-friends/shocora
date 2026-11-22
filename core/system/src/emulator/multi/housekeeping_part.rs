//! Part of [`super`]: see its documentation.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                         ▲▲▲                ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ one frame of time  │   │ emulator/multi/housekeep │   │ frame of emulation │
//! │                    │   │ ing_part.rs              │   │                    │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ three cores at once:     │   │                    │
//! │                    │   │ housekeeping part        │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "housekeeping part" share of multi.rs (three cores at once), moved here so that no file
//! is longer than 500 lines. It works exactly as if it were still written in that file. Runs
//! the three CPU cores side by side on three PC threads, like the real console. Terms such as
//! HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.

use super::*;

/// Sound, alarms and callbacks (on the main core), then something to run. Returns whether
/// the core has something.
pub(super) fn housekeeping(
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
pub(super) fn end_time_slice(
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

pub(super) fn stop_with(
    shared: &Shared<'_>,
    mut report: CoreReport,
    message: String,
) -> CoreReport {
    shared.stop.store(true, Ordering::Relaxed);
    shared.changed.notify_all();
    report.crash = Some(message);
    report
}

pub(super) fn cpu_error(cpu: &CpuState, memory: &Memory, error: &CpuError) -> String {
    let context = memory
        .read_u32(cpu.pc)
        .map_or_else(|_| String::new(), |raw| format!(": {}", disassemble(cpu.pc, raw)));
    format!("{error}{context}")
}
