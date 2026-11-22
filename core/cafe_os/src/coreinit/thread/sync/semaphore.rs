//! Part of [`super`]: see its documentation.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ lock/wait/signal   │   │ coreinit/thread/sync/sem │   │ threads            │
//! │                    │   │ aphore.rs                │   │ woken/blocked      │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ locks, events,           │   │                    │
//! │                    │   │ semaphores: semaphore    │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "semaphore" share of sync.rs (locks, events, semaphores), moved here so that no file is
//! longer than 500 lines. It works exactly as if it were still written in that file. The tools
//! threads use to wait for each other: mutexes, condition variables, events, semaphores. Terms
//! such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.

use super::*;

#[hle_export(lib = "coreinit", name = "OSInitSemaphore")]
pub(super) fn os_init_semaphore(
    ctx: &mut HleContext<'_>,
    semaphore: u32,
    count: i32,
) -> Result<(), HleError> {
    ctx.memory.write_u32(semaphore, SEMAPHORE_MAGIC)?;
    set_semaphore_count(ctx, semaphore, count)
}

#[hle_export(lib = "coreinit", name = "OSInitSemaphoreEx")]
pub(super) fn os_init_semaphore_ex(
    ctx: &mut HleContext<'_>,
    semaphore: u32,
    count: i32,
    _name: u32,
) -> Result<(), HleError> {
    ctx.memory.write_u32(semaphore, SEMAPHORE_MAGIC)?;
    set_semaphore_count(ctx, semaphore, count)
}

/// Returns the count before decrementing.
#[hle_export(lib = "coreinit", name = "OSWaitSemaphore")]
pub(super) fn os_wait_semaphore(ctx: &mut HleContext<'_>, semaphore: u32) -> Result<i32, HleError> {
    let count = semaphore_count(ctx, semaphore)?;
    if count > 0 {
        set_semaphore_count(ctx, semaphore, count - 1)?;
    } else {
        block(ctx, Wait::Semaphore(semaphore));
    }
    Ok(count)
}

#[hle_export(lib = "coreinit", name = "OSTryWaitSemaphore")]
pub(super) fn os_try_wait_semaphore(
    ctx: &mut HleContext<'_>,
    semaphore: u32,
) -> Result<i32, HleError> {
    let count = semaphore_count(ctx, semaphore)?;
    if count > 0 {
        set_semaphore_count(ctx, semaphore, count - 1)?;
    }
    Ok(count)
}

#[hle_export(lib = "coreinit", name = "OSSignalSemaphore")]
pub(super) fn os_signal_semaphore(
    ctx: &mut HleContext<'_>,
    semaphore: u32,
) -> Result<i32, HleError> {
    let count = semaphore_count(ctx, semaphore)?;
    set_semaphore_count(ctx, semaphore, count + 1)?;
    ctx.os.coreinit.threads.request(Reschedule::Priority);
    Ok(count)
}

#[hle_export(lib = "coreinit", name = "OSGetSemaphoreCount")]
pub(super) fn os_get_semaphore_count(
    ctx: &mut HleContext<'_>,
    semaphore: u32,
) -> Result<i32, HleError> {
    semaphore_count(ctx, semaphore)
}

#[hle_export(lib = "coreinit", name = "OSInitRendezvous")]
pub(super) fn os_init_rendezvous(
    ctx: &mut HleContext<'_>,
    rendezvous: u32,
) -> Result<(), HleError> {
    Ok(ctx.memory.fill(rendezvous, 12, 0)?)
}

/// Marks the calling core and waits for every core in `mask`.
pub(super) fn wait_rendezvous(
    ctx: &mut HleContext<'_>,
    rendezvous: u32,
    mask: u32,
) -> Result<bool, HleError> {
    let core = ctx.cpu.core_id().min(2);
    ctx.memory.write_u32(rendezvous + 4 * core, 1)?;
    let mut all = true;
    for other in (0..3).filter(|c| mask & (1 << c) != 0) {
        all &= ctx.memory.read_u32(rendezvous + 4 * other)? != 0;
    }
    if !all {
        block(ctx, Wait::Rendezvous { rendezvous, mask });
    }
    Ok(all)
}

#[hle_export(lib = "coreinit", name = "OSWaitRendezvous")]
pub(super) fn os_wait_rendezvous(
    ctx: &mut HleContext<'_>,
    rendezvous: u32,
    mask: u32,
) -> Result<bool, HleError> {
    wait_rendezvous(ctx, rendezvous, mask)
}

#[hle_export(lib = "coreinit", name = "OSWaitRendezvousWithTimeout")]
pub(super) fn os_wait_rendezvous_with_timeout(
    ctx: &mut HleContext<'_>,
    rendezvous: u32,
    mask: u32,
    _timeout: i64,
) -> Result<bool, HleError> {
    wait_rendezvous(ctx, rendezvous, mask)
}
