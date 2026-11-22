//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ coreinit/thread/sync_tes │   │ pass / fail        │
//! │                    │──►│ ts.rs                    │──►│                    │
//! │                    │   │ ──────────────────────── │   │                    │
//! │                    │   │ tests of sync.rs         │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for sync.rs (locks, events, semaphores). They run with `cargo test` only and are
//! not part of the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

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
