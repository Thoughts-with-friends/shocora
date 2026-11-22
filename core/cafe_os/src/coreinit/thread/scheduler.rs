//! Cooperative scheduler for guest threads.
//!
//! On a single host thread, all guest threads share the emulated main core's register file:
//! the scheduler saves the running thread's [`CpuState`] and loads another one, and the three
//! cores take turns. With [`Scheduler::multi_core`], each core runs on a host thread of its
//! own, like Cemu's multi-core mode: every core has its own register file and current thread,
//! and runs only the threads that belong to it. The scheduler is then used under a lock,
//! by one core at a time, between [`Scheduler::enter_core`] and [`Scheduler::leave_core`].
//!
//! Blocking calls are *restartable*: an HLE function that must wait records a
//! [`Wait`], rewinds the PC onto its call stub and asks for a reschedule. When
//! the wait is satisfied the thread runs again, re-executes the same HLE call
//! and completes it (using [`ThreadRecord::woke_from`] when the call needs to
//! know it already waited, e.g. sleeps and condition variables).
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ threads ready to   │   │ coreinit/thread/schedule │   │ thread to run next │
//! │ run                │──►│ r.rs                     │──►│                    │
//! │                    │   │ ──────────────────────── │   │                    │
//! │                    │   │ thread scheduler         │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Picks which game thread runs next on each core, and switches between them, like Windows'
//! scheduler does for your programs. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.
use std::collections::{BTreeMap, HashMap};

use wiiu_cpu::CpuState;
use wiiu_memory::Memory;

/// What a waiting thread waits for.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Another thread to terminate.
    Join(u32),
    /// An `OSMutex`/`OSFastMutex`/spinlock to become free.
    Mutex(u32),
    /// The timebase to reach a deadline (ticks since boot).
    Sleep(u64),
    /// A message queue to become non-empty.
    MessageReceive(u32),
    /// A message queue to have a free slot.
    MessageSend(u32),
    /// An `OSEvent` to be signaled.
    Event(u32),
    /// An `OSEvent` to be signaled, or a deadline.
    EventTimeout { event: u32, deadline: u64 },
    /// An `OSSemaphore` count to become positive.
    Semaphore(u32),
    /// An `OSCond` to be signaled (woken explicitly by `OSSignalCond`).
    Cond(u32),
    /// Every core in `mask` to reach an `OSRendezvous`.
    Rendezvous { rendezvous: u32, mask: u32 },
    /// The GPU to execute a command buffer (`GX2DrawDone`, timestamps, a full queue).
    Gpu(u64),
}

/// Guest-visible lifecycle of a thread.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Ready,
    Running,
    Waiting(Wait),
    Suspended,
    /// Terminated; waiting to be joined (or reused by `OSRunThread`).
    Moribund,
}

impl ThreadState {
    /// `OSThread::state` byte (`THREAD_STATE`).
    pub const fn guest_value(self) -> u8 {
        match self {
            Self::Ready => 1,
            Self::Running => 2,
            Self::Waiting(_) | Self::Suspended => 4,
            Self::Moribund => 8,
        }
    }
}

/// A condition-variable wait in progress (survives the restart).
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingCond {
    pub cond: u32,
    pub mutex: u32,
    /// Recursion count to restore on reacquire.
    pub count: u32,
    pub signaled: bool,
}

/// Host-side state of one guest thread.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct ThreadRecord {
    /// Registers while not running.
    pub context: CpuState,
    pub state: ThreadState,
    /// Lower is more important (0..=31).
    pub priority: i32,
    pub suspend_count: i32,
    pub exit_value: i32,
    pub detached: bool,
    pub name: u32,
    /// The wait that just ended, for the restarted HLE call.
    pub woke_from: Option<Wait>,
    pub pending_cond: Option<PendingCond>,
    /// Set by `OSWakeupThread`; lets the restarted `OSSleepThread` return.
    pub woken_from_queue: Option<u32>,
    last_run: u64,
}

impl ThreadRecord {
    pub fn new(context: CpuState, priority: i32) -> Self {
        Self {
            context,
            state: ThreadState::Suspended,
            priority,
            suspend_count: 1,
            exit_value: 0,
            detached: false,
            name: 0,
            woke_from: None,
            pending_cond: None,
            woken_from_queue: None,
            last_run: 0,
        }
    }
}

/// Owner and recursion count of a locked mutex.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct MutexState {
    pub owner: u32,
    pub count: u32,
    /// A thread may wait for it: set when one blocks on it, cleared once a release finds
    /// nobody waiting. Releases of locks nobody waits for skip looking through every thread.
    #[serde(default)]
    pub contended: bool,
}

impl MutexState {
    pub const FREE: Self = Self { owner: 0, count: 0, contended: false };

    /// Taken by `owner`, `count` times; whether threads wait stays as it was.
    pub const fn take(&mut self, owner: u32, count: u32) {
        self.owner = owner;
        self.count = count;
    }
}

/// How eagerly to switch away from the current thread.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reschedule {
    /// The current thread blocked or exited.
    Block,
    /// Round-robin among threads of equal or higher priority.
    Yield,
    /// Only switch to a strictly more important thread.
    Priority,
}

/// Result of a scheduling decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Kept running the current thread.
    Kept,
    /// Switched to another thread.
    Switched,
    /// Nothing can run until time passes or an interrupt arrives.
    Idle,
}

/// Offsets inside guest objects the wait conditions inspect.
mod offset {
    pub const MESSAGE_QUEUE_COUNT: u32 = 0x30;
    pub const MESSAGE_QUEUE_USED: u32 = 0x38;
    pub const EVENT_STATE: u32 = 0x0C;
    pub const SEMAPHORE_COUNT: u32 = 0x0C;
}

/// Cores threads can be assigned to.
const CORE_COUNT: u32 = 3;

/// All threads and the locks they contend on.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct Scheduler {
    pub threads: BTreeMap<u32, ThreadRecord>,
    /// `OSThread` address of the running thread.
    pub current: u32,
    pub mutexes: HashMap<u32, MutexState>,
    pending: Option<Reschedule>,
    run_counter: u64,
    /// The current thread of each core (0: none), when they run side by side.
    #[serde(default)]
    pub cores: [u32; CORE_COUNT as usize],
    /// Whether the cores run side by side, each on a host thread of its own.
    #[serde(skip)]
    pub multi_core: bool,
    /// Counts scheduling requests: a change means a thread may have become runnable, which
    /// idle cores want to hear about.
    #[serde(skip)]
    pub requests: u64,
    /// The GPU's retired timestamp, for [`Wait::Gpu`]; without it, the GPU is never behind.
    #[serde(skip)]
    pub gpu_retired: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
}

#[cfg(test)]
#[path = "scheduler_tests.rs"]
mod tests;

mod switching;
#[allow(unused_imports)]
pub(crate) use switching::*;
