use super::super::{ThreadRecord, ThreadState};
use crate::hle::test_env::{MAIN_THREAD, SCRATCH, TestEnv};

const THREAD: u32 = SCRATCH + 0x1000;
const ENTRY: u32 = 0x0200_0100;
const STACK_TOP: u32 = SCRATCH + 0x2_0000;
/// `OS_THREAD_ATTR_AFFINITY_CORE2`.
const ON_CORE_2: u32 = 4;

/// Three cores side by side, with `THREAD` left as core 2's current thread in `state`.
fn side_by_side(state: ThreadState) -> TestEnv {
    let mut env = TestEnv::new();
    let threads = &mut env.os.coreinit.threads;
    threads.multi_core = true;
    threads.cores = [0, MAIN_THREAD, THREAD];
    let mut record = ThreadRecord::new(wiiu_cpu::CpuState::new(2), 16);
    record.state = state;
    threads.threads.insert(THREAD, record);
    env
}

fn create(env: &mut TestEnv) -> u32 {
    let args = [THREAD, ENTRY, 0, 0, STACK_TOP, 0x1000, 16, ON_CORE_2];
    env.call("coreinit", "OSCreateThread", &args)
}

/// Intent: a thread created again at the address of one that ended on another core starts
/// fresh there (Mario Tennis restarts its sound thread this way during boot).
/// Pass: the call succeeds, core 2 no longer holds the old thread, and the new record is
/// suspended at the new entry point, as `OSCreateThread` leaves every thread.
#[test]
fn recreating_a_thread_that_ended_on_another_core_starts_it_fresh() {
    let mut env = side_by_side(ThreadState::Moribund);
    assert_eq!(create(&mut env), 1);
    let threads = &env.os.coreinit.threads;
    assert_eq!(threads.cores[2], 0);
    let record = &threads.threads[&THREAD];
    assert_eq!((record.state, record.context.pc), (ThreadState::Suspended, ENTRY));
}

/// Intent: while the old thread still runs its last instructions on another core, the
/// creator waits for it to end instead of replacing its record under it.
/// Pass: the call blocks the caller and leaves the old record alone.
#[test]
fn recreating_a_thread_still_running_on_another_core_waits_for_it() {
    let mut env = side_by_side(ThreadState::Running);
    create(&mut env);
    let threads = &env.os.coreinit.threads;
    assert!(threads.current_is_blocked());
    assert_eq!(threads.threads[&THREAD].state, ThreadState::Running);
    assert_eq!(threads.cores[2], THREAD);
}
