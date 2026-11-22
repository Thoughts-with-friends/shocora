//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ coreinit/thread/schedule │   │ pass / fail        │
//! │                    │──►│ r_tests.rs               │──►│                    │
//! │                    │   │ ──────────────────────── │   │                    │
//! │                    │   │ tests of scheduler.rs    │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for scheduler.rs (thread scheduler). They run with `cargo test` only and are not
//! part of the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use wiiu_mem_addr::Region;

use super::*;

fn setup() -> (Scheduler, CpuState, Memory) {
    let mut memory = Memory::new();
    memory.map(Region::new("M", 0x1000, 0x1000)).unwrap();
    let mut scheduler = Scheduler::default();
    scheduler.adopt_running(0x100, 16);
    (scheduler, CpuState::new(1), memory)
}

fn spawn(scheduler: &mut Scheduler, thread: u32, priority: i32, pc: u32) {
    let mut context = CpuState::new(1);
    context.pc = pc;
    let mut record = ThreadRecord::new(context, priority);
    record.state = ThreadState::Ready;
    record.suspend_count = 0;
    scheduler.threads.insert(thread, record);
}

#[test]
fn blocking_switches_and_join_resumes() {
    let (mut s, mut cpu, memory) = setup();
    cpu.pc = 0xAAAA;
    spawn(&mut s, 0x200, 16, 0xBBBB);
    s.block_current(Wait::Join(0x200));
    assert_eq!(s.schedule(&mut cpu, &memory, 0, Reschedule::Block), Outcome::Switched);
    assert_eq!((s.current, cpu.pc), (0x200, 0xBBBB));

    // The worker exits; the joiner becomes runnable again.
    s.threads.get_mut(&0x200).unwrap().state = ThreadState::Moribund;
    assert_eq!(s.schedule(&mut cpu, &memory, 0, Reschedule::Block), Outcome::Switched);
    assert_eq!((s.current, cpu.pc), (0x100, 0xAAAA));
    assert_eq!(s.take_woke_from(), Some(Wait::Join(0x200)));
}

#[test]
fn yield_round_robins_and_priority_preempts() {
    let (mut s, mut cpu, memory) = setup();
    spawn(&mut s, 0x200, 16, 0x2);
    spawn(&mut s, 0x300, 20, 0x3);
    assert_eq!(s.schedule(&mut cpu, &memory, 0, Reschedule::Priority), Outcome::Kept);
    assert_eq!(s.schedule(&mut cpu, &memory, 0, Reschedule::Yield), Outcome::Switched);
    assert_eq!(s.current, 0x200);
    assert_eq!(s.schedule(&mut cpu, &memory, 0, Reschedule::Yield), Outcome::Switched);
    assert_eq!(s.current, 0x100, "lower-priority 0x300 never runs while others are ready");
}

#[test]
fn sleeping_alone_is_idle_until_deadline() {
    let (mut s, mut cpu, memory) = setup();
    s.block_current(Wait::Sleep(100));
    assert_eq!(s.schedule(&mut cpu, &memory, 50, Reschedule::Block), Outcome::Idle);
    assert!(s.current_is_blocked());
    assert_eq!(s.schedule(&mut cpu, &memory, 100, Reschedule::Block), Outcome::Kept);
    assert_eq!(s.take_woke_from(), Some(Wait::Sleep(100)));
}

#[test]
fn side_by_side_cores_keep_their_own_threads() {
    let (mut s, mut core1, memory) = setup();
    s.multi_core = true;
    s.cores[1] = s.current;
    let main = s.current;
    spawn(&mut s, 0x300, 16, 0x2);
    let mut record = ThreadRecord::new(CpuState::new(0), 16);
    record.state = ThreadState::Ready;
    record.suspend_count = 0;
    s.threads.insert(0x200, record);
    // Core 1 round-robins among its own threads only.
    s.enter_core(1);
    assert_eq!(s.schedule(&mut core1, &memory, 0, Reschedule::Yield), Outcome::Switched);
    assert_eq!(s.current, 0x300);
    s.leave_core(1);
    // Core 0 picks up its thread, and never one another core is running.
    let mut core0 = CpuState::new(0);
    s.enter_core(0);
    assert_eq!(s.current, 0);
    assert_eq!(s.schedule(&mut core0, &memory, 0, Reschedule::Block), Outcome::Switched);
    assert_eq!(s.current, 0x200);
    s.leave_core(0);
    assert_eq!(s.cores, [0x200, 0x300, 0]);
    s.enter_core(1);
    assert_eq!(s.schedule(&mut core1, &memory, 0, Reschedule::Yield), Outcome::Switched);
    assert_eq!(s.current, main);
}

#[test]
fn message_queue_condition_reads_guest_memory() {
    let (mut s, mut cpu, mut memory) = setup();
    s.block_current(Wait::MessageReceive(0x1000));
    assert_eq!(s.schedule(&mut cpu, &memory, 0, Reschedule::Block), Outcome::Idle);
    memory.write_u32(0x1000 + offset::MESSAGE_QUEUE_USED, 1).unwrap();
    assert_eq!(s.schedule(&mut cpu, &memory, 0, Reschedule::Block), Outcome::Kept);
}
