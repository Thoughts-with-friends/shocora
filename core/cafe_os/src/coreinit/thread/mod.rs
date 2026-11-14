//! Threads and synchronisation primitives.
//!
//! * [`scheduler`] — thread table, wait conditions, context switching.
//! * [`api`] — `OSCreateThread`, `OSJoinThread`, ...
//! * [`sync`] — mutexes, spinlocks, events, semaphores, condition variables, rendezvous.
//! * [`message`] — message queues.
pub mod api;
pub mod message;
pub mod scheduler;
pub mod sync;

pub use scheduler::{Outcome, Reschedule, Scheduler, ThreadRecord, ThreadState, Wait};
use wiiu_cpu::CpuState;
use wiiu_memory::{Memory, MemoryError};

use crate::hle::{HleContext, HleExport};

/// `sizeof(OSThread)`.
pub const OS_THREAD_SIZE: u32 = 0x6A0;
/// Priority given to the main thread.
pub const MAIN_THREAD_PRIORITY: i32 = 16;
/// Instructions a thread may run before another of equal priority gets a turn.
pub const TIME_SLICE: u64 = 100_000;

/// Offsets inside `OSThread`.
pub mod offset {
    pub const MAGIC: u32 = 0x320;
    pub const STATE: u32 = 0x324;
    pub const ATTR: u32 = 0x325;
    pub const ID: u32 = 0x326;
    pub const SUSPEND_COUNTER: u32 = 0x328;
    pub const EFFECTIVE_PRIORITY: u32 = 0x32C;
    pub const BASE_PRIORITY: u32 = 0x330;
    pub const EXIT_VALUE: u32 = 0x334;
    pub const STACK_BASE: u32 = 0x394;
    pub const STACK_END: u32 = 0x398;
    pub const ENTRY: u32 = 0x39C;
    pub const NAME: u32 = 0x5C0;
}

/// `"tHrD"`.
pub const THREAD_MAGIC: u32 = 0x7448_7244;

/// Mirrors the host record into the guest `OSThread` fields games read.
///
/// # Errors
/// Fails if the structure is unmapped.
pub fn sync_guest(
    memory: &mut Memory,
    thread: u32,
    record: &ThreadRecord,
) -> Result<(), MemoryError> {
    memory.write_u32(thread + offset::MAGIC, THREAD_MAGIC)?;
    memory.write_u8(thread + offset::STATE, record.state.guest_value())?;
    memory.write_u32(thread + offset::SUSPEND_COUNTER, record.suspend_count.cast_unsigned())?;
    memory.write_u32(thread + offset::EFFECTIVE_PRIORITY, record.priority.cast_unsigned())?;
    memory.write_u32(thread + offset::BASE_PRIORITY, record.priority.cast_unsigned())?;
    memory.write_u32(thread + offset::EXIT_VALUE, record.exit_value.cast_unsigned())?;
    memory.write_u32(thread + offset::NAME, record.name)
}

/// Blocks the calling thread on `wait`; its HLE call restarts when the wait ends.
#[inline]
pub fn block(ctx: &mut HleContext<'_>, wait: Wait) {
    ctx.cpu.pc = ctx.cpu.pc.wrapping_sub(4);
    ctx.os.coreinit.threads.block_current(wait);
}

/// Core a thread runs on, from its `OS_THREAD_ATTR_AFFINITY_*` bits.
pub const fn core_from_attr(attr: u32, default: u32) -> u32 {
    match attr & 7 {
        0 | 7 => default,
        bits => bits.trailing_zeros(),
    }
}

/// Fresh register state for a thread starting at `entry`.
pub fn new_context(
    parent: &CpuState,
    core: u32,
    entry: u32,
    argc: u32,
    argv: u32,
    stack_top: u32,
    lr: u32,
) -> CpuState {
    let mut context = CpuState::new(core);
    context.pc = entry;
    context.lr = lr;
    context.gpr[1] = (stack_top.wrapping_sub(16)) & !0xF;
    context.gpr[2] = parent.gpr[2];
    context.gpr[3] = argc;
    context.gpr[4] = argv;
    context.gpr[13] = parent.gpr[13];
    context.gqr = parent.gqr;
    context
}

pub const EXPORTS: &[HleExport] = &[
    api::OS_CREATE_THREAD,
    api::OS_CREATE_THREAD_TYPE,
    api::OS_RUN_THREAD,
    api::OS_RESUME_THREAD,
    api::OS_SUSPEND_THREAD,
    api::OS_JOIN_THREAD,
    api::OS_DETACH_THREAD,
    api::OS_EXIT_THREAD,
    api::THREAD_RETURN,
    api::OS_GET_CURRENT_THREAD,
    api::TLS_GET_ADDR,
    api::OS_GET_DEFAULT_THREAD,
    api::OS_GET_THREAD_PRIORITY,
    api::OS_SET_THREAD_PRIORITY,
    api::OS_SET_THREAD_NAME,
    api::OS_GET_THREAD_NAME,
    api::OS_IS_THREAD_TERMINATED,
    api::OS_IS_THREAD_SUSPENDED,
    api::OS_GET_THREAD_SPECIFIC,
    api::OS_SET_THREAD_SPECIFIC,
    api::OS_SET_THREAD_AFFINITY,
    api::OS_GET_THREAD_AFFINITY,
    api::OS_YIELD_THREAD,
    api::OS_SLEEP_TICKS,
    api::OS_GET_STACK_POINTER,
    api::OS_TEST_THREAD_CANCEL,
    api::OS_SET_THREAD_CLEANUP_CALLBACK,
    api::OS_SET_THREAD_DEALLOCATOR,
    api::OS_CHECK_ACTIVE_THREADS,
    api::OS_DISABLE_INTERRUPTS,
    api::OS_RESTORE_INTERRUPTS,
    api::OS_IS_INTERRUPT_ENABLED,
    sync::OS_INIT_MUTEX,
    sync::OS_INIT_MUTEX_EX,
    sync::OS_LOCK_MUTEX,
    sync::OS_TRY_LOCK_MUTEX,
    sync::OS_UNLOCK_MUTEX,
    sync::OS_FAST_MUTEX_INIT,
    sync::OS_FAST_MUTEX_LOCK,
    sync::OS_FAST_MUTEX_TRY_LOCK,
    sync::OS_FAST_MUTEX_UNLOCK,
    sync::OS_INIT_SPIN_LOCK,
    sync::OS_UNINTERRUPTIBLE_SPIN_LOCK_ACQUIRE,
    sync::OS_UNINTERRUPTIBLE_SPIN_LOCK_TRY_ACQUIRE,
    sync::OS_UNINTERRUPTIBLE_SPIN_LOCK_RELEASE,
    sync::OS_ACQUIRE_SPIN_LOCK,
    sync::OS_TRY_ACQUIRE_SPIN_LOCK,
    sync::OS_RELEASE_SPIN_LOCK,
    sync::GHS_LOCK,
    sync::GHS_UNLOCK,
    sync::OS_INIT_COND,
    sync::OS_INIT_COND_EX,
    sync::OS_WAIT_COND,
    sync::OS_SIGNAL_COND,
    sync::OS_FAST_COND_INIT,
    sync::OS_FAST_COND_WAIT,
    sync::OS_FAST_COND_SIGNAL,
    sync::OS_INIT_EVENT,
    sync::OS_INIT_EVENT_EX,
    sync::OS_SIGNAL_EVENT,
    sync::OS_SIGNAL_EVENT_ALL,
    sync::OS_RESET_EVENT,
    sync::OS_WAIT_EVENT,
    sync::OS_WAIT_EVENT_WITH_TIMEOUT,
    sync::OS_INIT_SEMAPHORE,
    sync::OS_INIT_SEMAPHORE_EX,
    sync::OS_WAIT_SEMAPHORE,
    sync::OS_TRY_WAIT_SEMAPHORE,
    sync::OS_SIGNAL_SEMAPHORE,
    sync::OS_GET_SEMAPHORE_COUNT,
    sync::OS_INIT_RENDEZVOUS,
    sync::OS_WAIT_RENDEZVOUS,
    sync::OS_WAIT_RENDEZVOUS_WITH_TIMEOUT,
    message::OS_INIT_THREAD_QUEUE,
    message::OS_INIT_THREAD_QUEUE_EX,
    message::OS_INIT_MESSAGE_QUEUE,
    message::OS_INIT_MESSAGE_QUEUE_EX,
    message::OS_SEND_MESSAGE,
    message::OS_JAM_MESSAGE,
    message::OS_RECEIVE_MESSAGE,
    message::OS_PEEK_MESSAGE,
    message::OS_GET_SYSTEM_MESSAGE_QUEUE,
];

#[cfg(test)]
mod tests {
    use wiiu_cpu::CpuState;

    use super::{core_from_attr, new_context};

    /// Intent: `OS_THREAD_ATTR_AFFINITY_*` picks a thread's core; "any core" (0 or all three
    /// bits) keeps the creator's.
    /// Pass: bit 0 → core 0, bit 2 → core 2, 0 and 7 → the default.
    #[test]
    fn affinity_bits_pick_the_core() {
        assert_eq!(core_from_attr(1, 1), 0);
        assert_eq!(core_from_attr(4, 1), 2);
        assert_eq!(core_from_attr(0, 1), 1);
        assert_eq!(core_from_attr(7, 2), 2);
    }

    /// Intent: a new thread starts at its entry with the PowerPC EABI set up: arguments in
    /// r3/r4, an aligned stack below its top, and the small data bases of its creator.
    /// Pass: pc, lr, r1 (16-byte aligned, under the top), r2, r3, r4 and r13 are as set.
    #[test]
    fn new_threads_start_with_the_eabi_registers() {
        let mut parent = CpuState::new(1);
        parent.gpr[2] = 0x1000_8000;
        parent.gpr[13] = 0x1056_8600;
        let context =
            new_context(&parent, 2, 0x0200_0000, 1, 0x1000_0000, 0x1001_0007, 0x0E00_1040);
        assert_eq!((context.pc, context.lr), (0x0200_0000, 0x0E00_1040));
        assert_eq!(context.gpr[1] % 16, 0);
        assert!(context.gpr[1] < 0x1001_0007);
        assert_eq!((context.gpr[2], context.gpr[13]), (0x1000_8000, 0x1056_8600));
        assert_eq!((context.gpr[3], context.gpr[4]), (1, 0x1000_0000));
        assert_eq!(context.core_id(), 2);
    }
}
