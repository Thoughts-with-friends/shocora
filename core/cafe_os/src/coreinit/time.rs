//! Time: `OSGetTime`, `OSGetTick`, `OSTicksToCalendarTime`, ...
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ "what time is it?" │   │ coreinit/time.rs         │   │ ticks, calendar    │
//! │                    │──►│ ──────────────────────── │──►│ date               │
//! │                    │   │ time and clock           │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Gives the game the current time, in console clock ticks or as a calendar date. Terms such as
//! HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

use wiiu_hle_macros::hle_export;
use wiiu_memory::GuestPtr;
use wiiu_peripherals::CalendarTime;

use crate::hle::{HleContext, HleError};

/// Ticks since 2000-01-01.
#[hle_export(lib = "coreinit", name = "OSGetTime")]
fn os_get_time(ctx: &mut HleContext<'_>) -> i64 {
    ctx.os.timebase.ticks_since_epoch() as i64
}

/// Ticks since boot.
#[hle_export(lib = "coreinit", name = "OSGetSystemTime")]
fn os_get_system_time(ctx: &mut HleContext<'_>) -> i64 {
    ctx.os.timebase.ticks_since_boot() as i64
}

#[hle_export(lib = "coreinit", name = "OSGetTick")]
fn os_get_tick(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.timebase.ticks_since_epoch() as u32
}

#[hle_export(lib = "coreinit", name = "OSGetSystemTick")]
fn os_get_system_tick(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.timebase.ticks_since_boot() as u32
}

/// Fills an `OSCalendarTime` (ten `int32`s).
#[hle_export(lib = "coreinit", name = "OSTicksToCalendarTime")]
fn os_ticks_to_calendar_time(
    ctx: &mut HleContext<'_>,
    ticks: i64,
    out: GuestPtr<i32>,
) -> Result<(), HleError> {
    let t = CalendarTime::from_ticks(ticks);
    let fields = [
        t.second,
        t.minute,
        t.hour,
        t.day,
        t.month,
        t.year,
        t.weekday,
        t.yearday,
        t.millisecond,
        t.microsecond,
    ];
    for (i, value) in fields.into_iter().enumerate() {
        out.add(i as u32).write(ctx.memory, value)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn calendar_time_layout() {
        let mut env = TestEnv::new();
        // ticks = 0 in r3:r4, out pointer in r5
        env.call("coreinit", "OSTicksToCalendarTime", &[0, 0, SCRATCH]);
        assert_eq!(env.memory.read_u32(SCRATCH + 3 * 4).unwrap(), 1); // mday
        assert_eq!(env.memory.read_u32(SCRATCH + 5 * 4).unwrap(), 2000); // year
        let now = env.call("coreinit", "OSGetTime", &[]);
        assert_ne!(now, 0);
    }
}
