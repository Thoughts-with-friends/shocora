//! Mutexes, spinlocks, condition variables, events, semaphores and rendezvous.
//!
//! Lock ownership lives in [`Scheduler::mutexes`](super::Scheduler::mutexes);
//! event and semaphore counters live in guest memory where games can see them.
use wiiu_hle_macros::hle_export;

use super::{
    Reschedule, ThreadState, Wait, block,
    scheduler::{MutexState, PendingCond},
};
use crate::hle::{HleContext, HleError};

/// Key of the Green Hills C runtime's global lock (not a guest address).
const GHS_LOCK_KEY: u32 = 0xFFFF_FFF0;

mod offset {
    pub const EVENT_STATE: u32 = 0x0C;
    pub const EVENT_MODE: u32 = 0x20;
    pub const SEMAPHORE_COUNT: u32 = 0x0C;
}

const EVENT_MAGIC: u32 = 0x6556_6E54; // "eVnT"
const SEMAPHORE_MAGIC: u32 = 0x734D_5048; // "sMPH"
/// `OS_EVENT_MODE_AUTO`: a successful wait resets the event.
const EVENT_MODE_AUTO: u32 = 1;

/// Outcome of trying to take a lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lock {
    Acquired,
    Contended,
}

fn try_lock(ctx: &mut HleContext<'_>, key: u32) -> Lock {
    let me = ctx.os.coreinit.threads.current;
    let state =
        ctx.os.coreinit.threads.mutexes.entry(key).or_insert(MutexState { owner: 0, count: 0 });
    if state.count == 0 {
        *state = MutexState { owner: me, count: 1 };
        Lock::Acquired
    } else if state.owner == me {
        state.count += 1;
        Lock::Acquired
    } else {
        Lock::Contended
    }
}

pub(crate) fn lock(ctx: &mut HleContext<'_>, key: u32) {
    if try_lock(ctx, key) == Lock::Contended {
        block(ctx, Wait::Mutex(key));
    }
}

pub(crate) fn unlock(ctx: &mut HleContext<'_>, key: u32) {
    let scheduler = &mut ctx.os.coreinit.threads;
    let me = scheduler.current;
    let Some(state) = scheduler.mutexes.get_mut(&key) else { return };
    if state.owner == me && state.count > 0 {
        state.count -= 1;
        if state.count == 0 {
            state.owner = 0;
            // Most releases find nobody waiting: a scheduling decision would be wasted on them,
            // and titles release locks a million times a second.
            let waited = Wait::Mutex(key);
            if scheduler.threads.values().any(|t| t.state == ThreadState::Waiting(waited)) {
                scheduler.request(Reschedule::Priority);
            }
        }
    }
}

fn reset_lock(ctx: &mut HleContext<'_>, key: u32) {
    ctx.os.coreinit.threads.mutexes.remove(&key);
}

#[hle_export(lib = "coreinit", name = "OSInitMutex")]
fn os_init_mutex(ctx: &mut HleContext<'_>, mutex: u32) {
    reset_lock(ctx, mutex);
}

#[hle_export(lib = "coreinit", name = "OSInitMutexEx")]
fn os_init_mutex_ex(ctx: &mut HleContext<'_>, mutex: u32, _name: u32) {
    reset_lock(ctx, mutex);
}

#[hle_export(lib = "coreinit", name = "OSLockMutex")]
fn os_lock_mutex(ctx: &mut HleContext<'_>, mutex: u32) {
    lock(ctx, mutex);
}

#[hle_export(lib = "coreinit", name = "OSTryLockMutex")]
fn os_try_lock_mutex(ctx: &mut HleContext<'_>, mutex: u32) -> bool {
    try_lock(ctx, mutex) == Lock::Acquired
}

#[hle_export(lib = "coreinit", name = "OSUnlockMutex")]
fn os_unlock_mutex(ctx: &mut HleContext<'_>, mutex: u32) {
    unlock(ctx, mutex);
}

#[hle_export(lib = "coreinit", name = "OSFastMutex_Init")]
fn os_fast_mutex_init(ctx: &mut HleContext<'_>, mutex: u32, _name: u32) {
    reset_lock(ctx, mutex);
}

#[hle_export(lib = "coreinit", name = "OSFastMutex_Lock")]
fn os_fast_mutex_lock(ctx: &mut HleContext<'_>, mutex: u32) {
    lock(ctx, mutex);
}

#[hle_export(lib = "coreinit", name = "OSFastMutex_TryLock")]
fn os_fast_mutex_try_lock(ctx: &mut HleContext<'_>, mutex: u32) -> bool {
    try_lock(ctx, mutex) == Lock::Acquired
}

#[hle_export(lib = "coreinit", name = "OSFastMutex_Unlock")]
fn os_fast_mutex_unlock(ctx: &mut HleContext<'_>, mutex: u32) {
    unlock(ctx, mutex);
}

#[hle_export(lib = "coreinit", name = "OSInitSpinLock")]
fn os_init_spin_lock(ctx: &mut HleContext<'_>, spinlock: u32) {
    reset_lock(ctx, spinlock);
}

#[hle_export(lib = "coreinit", name = "OSUninterruptibleSpinLock_Acquire")]
fn os_uninterruptible_spin_lock_acquire(ctx: &mut HleContext<'_>, spinlock: u32) -> bool {
    lock(ctx, spinlock);
    true
}

#[hle_export(lib = "coreinit", name = "OSUninterruptibleSpinLock_TryAcquire")]
fn os_uninterruptible_spin_lock_try_acquire(ctx: &mut HleContext<'_>, spinlock: u32) -> bool {
    try_lock(ctx, spinlock) == Lock::Acquired
}

#[hle_export(lib = "coreinit", name = "OSUninterruptibleSpinLock_Release")]
fn os_uninterruptible_spin_lock_release(ctx: &mut HleContext<'_>, spinlock: u32) -> bool {
    unlock(ctx, spinlock);
    true
}

#[hle_export(lib = "coreinit", name = "OSAcquireSpinLock")]
fn os_acquire_spin_lock(ctx: &mut HleContext<'_>, spinlock: u32) -> bool {
    lock(ctx, spinlock);
    true
}

#[hle_export(lib = "coreinit", name = "OSTryAcquireSpinLock")]
fn os_try_acquire_spin_lock(ctx: &mut HleContext<'_>, spinlock: u32) -> bool {
    try_lock(ctx, spinlock) == Lock::Acquired
}

#[hle_export(lib = "coreinit", name = "OSReleaseSpinLock")]
fn os_release_spin_lock(ctx: &mut HleContext<'_>, spinlock: u32) -> bool {
    unlock(ctx, spinlock);
    true
}

#[hle_export(lib = "coreinit", name = "__ghsLock")]
fn ghs_lock(ctx: &mut HleContext<'_>) {
    lock(ctx, GHS_LOCK_KEY);
}

#[hle_export(lib = "coreinit", name = "__ghsUnlock")]
fn ghs_unlock(ctx: &mut HleContext<'_>) {
    unlock(ctx, GHS_LOCK_KEY);
}

/// `OSWaitCond`: release `mutex`, wait for a signal, reacquire `mutex`.
fn wait_cond(ctx: &mut HleContext<'_>, cond: u32, mutex: u32) {
    let pending = ctx.os.coreinit.threads.current_mut().and_then(|t| t.pending_cond);
    match pending {
        Some(p) if p.cond == cond && p.signaled => {
            let me = ctx.os.coreinit.threads.current;
            let state = ctx
                .os
                .coreinit
                .threads
                .mutexes
                .entry(p.mutex)
                .or_insert(MutexState { owner: 0, count: 0 });
            if state.count == 0 {
                *state = MutexState { owner: me, count: p.count.max(1) };
                if let Some(thread) = ctx.os.coreinit.threads.current_mut() {
                    thread.pending_cond = None;
                }
            } else {
                block(ctx, Wait::Mutex(p.mutex));
            }
        }
        _ => {
            let me = ctx.os.coreinit.threads.current;
            let count = match ctx.os.coreinit.threads.mutexes.get_mut(&mutex) {
                Some(state) if state.owner == me => {
                    std::mem::replace(state, MutexState { owner: 0, count: 0 }).count
                }
                _ => 1,
            };
            if let Some(thread) = ctx.os.coreinit.threads.current_mut() {
                thread.pending_cond = Some(PendingCond { cond, mutex, count, signaled: false });
            }
            block(ctx, Wait::Cond(cond));
        }
    }
}

/// Wakes every thread waiting on `cond`.
fn signal_cond(ctx: &mut HleContext<'_>, cond: u32) {
    for thread in ctx.os.coreinit.threads.threads.values_mut() {
        if let Some(pending) =
            thread.pending_cond.as_mut().filter(|p| p.cond == cond && !p.signaled)
        {
            pending.signaled = true;
            thread.state = ThreadState::Ready;
        }
    }
    ctx.os.coreinit.threads.request(Reschedule::Priority);
}

#[hle_export(lib = "coreinit", name = "OSInitCond")]
fn os_init_cond(_ctx: &mut HleContext<'_>, _cond: u32) {}

#[hle_export(lib = "coreinit", name = "OSInitCondEx")]
fn os_init_cond_ex(_ctx: &mut HleContext<'_>, _cond: u32, _name: u32) {}

#[hle_export(lib = "coreinit", name = "OSWaitCond")]
fn os_wait_cond(ctx: &mut HleContext<'_>, cond: u32, mutex: u32) {
    wait_cond(ctx, cond, mutex);
}

#[hle_export(lib = "coreinit", name = "OSSignalCond")]
fn os_signal_cond(ctx: &mut HleContext<'_>, cond: u32) {
    signal_cond(ctx, cond);
}

#[hle_export(lib = "coreinit", name = "OSFastCond_Init")]
fn os_fast_cond_init(_ctx: &mut HleContext<'_>, _cond: u32, _name: u32) {}

#[hle_export(lib = "coreinit", name = "OSFastCond_Wait")]
fn os_fast_cond_wait(ctx: &mut HleContext<'_>, cond: u32, mutex: u32) {
    wait_cond(ctx, cond, mutex);
}

#[hle_export(lib = "coreinit", name = "OSFastCond_Signal")]
fn os_fast_cond_signal(ctx: &mut HleContext<'_>, cond: u32) {
    signal_cond(ctx, cond);
}

fn init_event(
    ctx: &mut HleContext<'_>,
    event: u32,
    signaled: bool,
    mode: u32,
) -> Result<(), HleError> {
    ctx.memory.write_u32(event, EVENT_MAGIC)?;
    ctx.memory.write_u32(event + offset::EVENT_STATE, u32::from(signaled))?;
    ctx.memory.write_u32(event + offset::EVENT_MODE, mode)?;
    Ok(())
}

/// Consumes a signaled event; returns `false` if it is not signaled.
fn take_event(ctx: &mut HleContext<'_>, event: u32) -> Result<bool, HleError> {
    if ctx.memory.read_u32(event + offset::EVENT_STATE)? == 0 {
        return Ok(false);
    }
    if ctx.memory.read_u32(event + offset::EVENT_MODE)? == EVENT_MODE_AUTO {
        ctx.memory.write_u32(event + offset::EVENT_STATE, 0)?;
    }
    Ok(true)
}

#[hle_export(lib = "coreinit", name = "OSInitEvent")]
fn os_init_event(
    ctx: &mut HleContext<'_>,
    event: u32,
    signaled: bool,
    mode: u32,
) -> Result<(), HleError> {
    init_event(ctx, event, signaled, mode)
}

#[hle_export(lib = "coreinit", name = "OSInitEventEx")]
fn os_init_event_ex(
    ctx: &mut HleContext<'_>,
    event: u32,
    signaled: bool,
    mode: u32,
    _name: u32,
) -> Result<(), HleError> {
    init_event(ctx, event, signaled, mode)
}

/// Whether `state` is a wait on `event`.
const fn waits_on_event(state: ThreadState, event: u32) -> bool {
    match state {
        ThreadState::Waiting(Wait::Event(waited)) => waited == event,
        ThreadState::Waiting(Wait::EventTimeout { event: waited, .. }) => waited == event,
        _ => false,
    }
}

/// Wakes threads waiting on `event`, most important first; they return from their wait whatever
/// happens to the event's state before they run. Returns how many woke.
fn wake_event_waiters(ctx: &mut HleContext<'_>, event: u32, limit: usize) -> usize {
    let threads = &mut ctx.os.coreinit.threads.threads;
    let mut waiters: Vec<_> =
        threads.iter_mut().filter(|(_, t)| waits_on_event(t.state, event)).collect();
    waiters.sort_by_key(|(_, t)| t.priority);
    let woken = waiters.len().min(limit);
    for (_, thread) in waiters.into_iter().take(limit) {
        thread.state = ThreadState::Ready;
        thread.woken_from_queue = Some(event);
    }
    woken
}

/// Port of Cemu's `OSSignalEvent[All]Internal`: an automatic event wakes `limit` threads and only
/// stays signaled if none waits; a manual one stays signaled and wakes them all.
fn signal_event(ctx: &mut HleContext<'_>, event: u32, limit: usize) -> Result<(), HleError> {
    if ctx.memory.read_u32(event + offset::EVENT_STATE)? != 0 {
        return Ok(());
    }
    if ctx.memory.read_u32(event + offset::EVENT_MODE)? == EVENT_MODE_AUTO {
        if wake_event_waiters(ctx, event, limit) == 0 {
            ctx.memory.write_u32(event + offset::EVENT_STATE, 1)?;
        }
    } else {
        ctx.memory.write_u32(event + offset::EVENT_STATE, 1)?;
        wake_event_waiters(ctx, event, usize::MAX);
    }
    ctx.os.coreinit.threads.request(Reschedule::Priority);
    Ok(())
}

/// Whether the restarted wait was ended by a signal that left the event unsignaled.
fn woken_by_signal(ctx: &mut HleContext<'_>, event: u32) -> bool {
    let woken = ctx
        .os
        .coreinit
        .threads
        .current_mut()
        .filter(|t| t.woken_from_queue == Some(event))
        .map(|t| t.woken_from_queue = None);
    woken.is_some()
}

#[hle_export(lib = "coreinit", name = "OSSignalEvent")]
fn os_signal_event(ctx: &mut HleContext<'_>, event: u32) -> Result<(), HleError> {
    signal_event(ctx, event, 1)
}

#[hle_export(lib = "coreinit", name = "OSSignalEventAll")]
fn os_signal_event_all(ctx: &mut HleContext<'_>, event: u32) -> Result<(), HleError> {
    signal_event(ctx, event, usize::MAX)
}

#[hle_export(lib = "coreinit", name = "OSResetEvent")]
fn os_reset_event(ctx: &mut HleContext<'_>, event: u32) -> Result<(), HleError> {
    ctx.memory.write_u32(event + offset::EVENT_STATE, 0)?;
    Ok(())
}

#[hle_export(lib = "coreinit", name = "OSWaitEvent")]
fn os_wait_event(ctx: &mut HleContext<'_>, event: u32) -> Result<(), HleError> {
    if woken_by_signal(ctx, event) {
        return Ok(());
    }
    if !take_event(ctx, event)? {
        block(ctx, Wait::Event(event));
    }
    Ok(())
}

/// Returns `true` if signaled, `false` on timeout.
#[hle_export(lib = "coreinit", name = "OSWaitEventWithTimeout")]
fn os_wait_event_with_timeout(
    ctx: &mut HleContext<'_>,
    event: u32,
    timeout: i64,
) -> Result<bool, HleError> {
    let woke = ctx.os.coreinit.threads.take_woke_from();
    if woken_by_signal(ctx, event) || take_event(ctx, event)? {
        return Ok(true);
    }
    if matches!(woke, Some(Wait::EventTimeout { .. })) {
        return Ok(false);
    }
    let deadline =
        ctx.os.timebase.ticks_since_boot().saturating_add(timeout.max(0).cast_unsigned());
    block(ctx, Wait::EventTimeout { event, deadline });
    Ok(false)
}

fn semaphore_count(ctx: &HleContext<'_>, semaphore: u32) -> Result<i32, HleError> {
    Ok(ctx.memory.read_u32(semaphore + offset::SEMAPHORE_COUNT)?.cast_signed())
}

fn set_semaphore_count(
    ctx: &mut HleContext<'_>,
    semaphore: u32,
    count: i32,
) -> Result<(), HleError> {
    Ok(ctx.memory.write_u32(semaphore + offset::SEMAPHORE_COUNT, count.cast_unsigned())?)
}

#[hle_export(lib = "coreinit", name = "OSInitSemaphore")]
fn os_init_semaphore(ctx: &mut HleContext<'_>, semaphore: u32, count: i32) -> Result<(), HleError> {
    ctx.memory.write_u32(semaphore, SEMAPHORE_MAGIC)?;
    set_semaphore_count(ctx, semaphore, count)
}

#[hle_export(lib = "coreinit", name = "OSInitSemaphoreEx")]
fn os_init_semaphore_ex(
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
fn os_wait_semaphore(ctx: &mut HleContext<'_>, semaphore: u32) -> Result<i32, HleError> {
    let count = semaphore_count(ctx, semaphore)?;
    if count > 0 {
        set_semaphore_count(ctx, semaphore, count - 1)?;
    } else {
        block(ctx, Wait::Semaphore(semaphore));
    }
    Ok(count)
}

#[hle_export(lib = "coreinit", name = "OSTryWaitSemaphore")]
fn os_try_wait_semaphore(ctx: &mut HleContext<'_>, semaphore: u32) -> Result<i32, HleError> {
    let count = semaphore_count(ctx, semaphore)?;
    if count > 0 {
        set_semaphore_count(ctx, semaphore, count - 1)?;
    }
    Ok(count)
}

#[hle_export(lib = "coreinit", name = "OSSignalSemaphore")]
fn os_signal_semaphore(ctx: &mut HleContext<'_>, semaphore: u32) -> Result<i32, HleError> {
    let count = semaphore_count(ctx, semaphore)?;
    set_semaphore_count(ctx, semaphore, count + 1)?;
    ctx.os.coreinit.threads.request(Reschedule::Priority);
    Ok(count)
}

#[hle_export(lib = "coreinit", name = "OSGetSemaphoreCount")]
fn os_get_semaphore_count(ctx: &mut HleContext<'_>, semaphore: u32) -> Result<i32, HleError> {
    semaphore_count(ctx, semaphore)
}

#[hle_export(lib = "coreinit", name = "OSInitRendezvous")]
fn os_init_rendezvous(ctx: &mut HleContext<'_>, rendezvous: u32) -> Result<(), HleError> {
    Ok(ctx.memory.fill(rendezvous, 12, 0)?)
}

/// Marks the calling core and waits for every core in `mask`.
fn wait_rendezvous(ctx: &mut HleContext<'_>, rendezvous: u32, mask: u32) -> Result<bool, HleError> {
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
fn os_wait_rendezvous(
    ctx: &mut HleContext<'_>,
    rendezvous: u32,
    mask: u32,
) -> Result<bool, HleError> {
    wait_rendezvous(ctx, rendezvous, mask)
}

#[hle_export(lib = "coreinit", name = "OSWaitRendezvousWithTimeout")]
fn os_wait_rendezvous_with_timeout(
    ctx: &mut HleContext<'_>,
    rendezvous: u32,
    mask: u32,
    _timeout: i64,
) -> Result<bool, HleError> {
    wait_rendezvous(ctx, rendezvous, mask)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::test_env::{SCRATCH, TestEnv};

    const MANUAL: u32 = 0;

    fn add_waiter(env: &mut TestEnv, thread: u32, priority: i32, event: u32) {
        let mut record = super::super::ThreadRecord::new(wiiu_cpu::CpuState::new(1), priority);
        record.state = ThreadState::Waiting(Wait::Event(event));
        env.os.coreinit.threads.threads.insert(thread, record);
    }

    #[test]
    fn automatic_event_wakes_one_thread_and_stays_unsignaled() {
        let mut env = TestEnv::new();
        env.call("coreinit", "OSInitEvent", &[SCRATCH, 0, EVENT_MODE_AUTO]);
        add_waiter(&mut env, 0x100, 20, SCRATCH);
        add_waiter(&mut env, 0x200, 18, SCRATCH);
        env.call("coreinit", "OSSignalEvent", &[SCRATCH]);
        let threads = &env.os.coreinit.threads.threads;
        assert_eq!(threads[&0x200].state, ThreadState::Ready, "the most important one");
        assert_eq!(threads[&0x100].state, ThreadState::Waiting(Wait::Event(SCRATCH)));
        assert_eq!(env.memory.read_u32(SCRATCH + offset::EVENT_STATE).unwrap(), 0);
    }

    #[test]
    fn manual_event_reset_at_once_still_releases_its_waiters() {
        let mut env = TestEnv::new();
        env.call("coreinit", "OSInitEvent", &[SCRATCH, 0, MANUAL]);
        add_waiter(&mut env, 0x100, 20, SCRATCH);
        add_waiter(&mut env, 0x200, 18, SCRATCH);
        env.call("coreinit", "OSSignalEvent", &[SCRATCH]);
        env.call("coreinit", "OSResetEvent", &[SCRATCH]);
        let threads = &env.os.coreinit.threads.threads;
        assert_eq!(threads[&0x100].woken_from_queue, Some(SCRATCH));
        assert_eq!(threads[&0x200].woken_from_queue, Some(SCRATCH));
    }

    #[test]
    fn signal_all_wakes_every_waiter_of_an_automatic_event() {
        let mut env = TestEnv::new();
        env.call("coreinit", "OSInitEvent", &[SCRATCH, 0, EVENT_MODE_AUTO]);
        add_waiter(&mut env, 0x100, 20, SCRATCH);
        add_waiter(&mut env, 0x200, 18, SCRATCH);
        env.call("coreinit", "OSSignalEventAll", &[SCRATCH]);
        let threads = &env.os.coreinit.threads.threads;
        assert_eq!(threads[&0x100].state, ThreadState::Ready);
        assert_eq!(threads[&0x200].state, ThreadState::Ready);
    }
}
