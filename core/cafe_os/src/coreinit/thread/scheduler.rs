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
    pub specific: [u32; 16],
    /// The wait that just ended, for the restarted HLE call.
    pub woke_from: Option<Wait>,
    pub pending_cond: Option<PendingCond>,
    /// Set by `OSWakeupThread`; lets the restarted `OSSleepThread` return.
    pub woken_from_queue: Option<u32>,
    /// This thread's copy of the RPX's TLS block (0 until `__tls_get_addr`).
    pub tls_block: u32,
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
            specific: [0; 16],
            woke_from: None,
            pending_cond: None,
            woken_from_queue: None,
            tls_block: 0,
            last_run: 0,
        }
    }
}

/// Owner and recursion count of a locked mutex.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct MutexState {
    pub owner: u32,
    pub count: u32,
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

impl Scheduler {
    /// Registers the thread that is already running on the core.
    pub fn adopt_running(&mut self, thread: u32, priority: i32) {
        let mut record = ThreadRecord::new(CpuState::new(1), priority);
        let core = record.context.core_id();
        record.state = ThreadState::Running;
        record.suspend_count = 0;
        self.threads.insert(thread, record);
        self.current = thread;
        if let Some(slot) = self.cores.get_mut(core as usize) {
            *slot = thread;
        }
    }

    /// Makes `core`'s current thread the current one, for what this core does next.
    #[inline]
    pub fn enter_core(&mut self, core: u32) {
        if self.multi_core {
            self.current = self.cores.get(core as usize).copied().unwrap_or(0);
        }
    }

    /// Remembers the current thread as `core`'s.
    #[inline]
    pub fn leave_core(&mut self, core: u32) {
        if self.multi_core {
            if let Some(slot) = self.cores.get_mut(core as usize) {
                *slot = self.current;
            }
        }
    }

    /// Puts the registers of every running thread back into its record, so that no thread
    /// is current: the form a switch between one host thread and three starts from, and
    /// that saved states of side-by-side cores keep. `cpus` are the cores' registers; on one
    /// host thread, `main_core`'s are the running thread's.
    pub fn park(&mut self, cpus: &[CpuState], main_core: usize) {
        let running: Vec<(u32, usize)> = if self.multi_core {
            self.cores.iter().enumerate().map(|(core, thread)| (*thread, core)).collect()
        } else {
            vec![(self.current, main_core)]
        };
        for (thread, core) in running {
            if let (Some(record), Some(cpu)) = (self.threads.get_mut(&thread), cpus.get(core)) {
                record.context = cpu.clone();
                if record.state == ThreadState::Running {
                    record.state = ThreadState::Ready;
                }
            }
        }
        self.cores = [0; CORE_COUNT as usize];
        self.current = 0;
        self.multi_core = false;
    }

    /// Whether the current thread is there and running.
    #[inline]
    pub fn current_is_running(&self) -> bool {
        self.threads.get(&self.current).is_some_and(|t| t.state == ThreadState::Running)
    }

    #[inline]
    pub fn current_mut(&mut self) -> Option<&mut ThreadRecord> {
        self.threads.get_mut(&self.current)
    }

    /// Asks for a scheduling decision after the current HLE call.
    pub fn request(&mut self, mode: Reschedule) {
        self.requests = self.requests.wrapping_add(1);
        self.pending = Some(match (self.pending, mode) {
            (Some(Reschedule::Block), _) | (_, Reschedule::Block) => Reschedule::Block,
            (Some(Reschedule::Yield), _) | (_, Reschedule::Yield) => Reschedule::Yield,
            _ => Reschedule::Priority,
        });
    }

    #[inline]
    pub const fn take_request(&mut self) -> Option<Reschedule> {
        self.pending.take()
    }

    /// Puts the current thread to sleep on `wait`.
    pub fn block_current(&mut self, wait: Wait) {
        if let Some(thread) = self.current_mut() {
            thread.state = ThreadState::Waiting(wait);
        }
        self.request(Reschedule::Block);
    }

    /// Consumes the wait the current thread just returned from.
    #[inline]
    pub fn take_woke_from(&mut self) -> Option<Wait> {
        self.current_mut().and_then(|t| t.woke_from.take())
    }

    fn wait_satisfied(&self, wait: Wait, memory: &Memory, now: u64) -> bool {
        let read = |address: u32| memory.read_u32(address).unwrap_or(0);
        match wait {
            Wait::Join(thread) => {
                self.threads.get(&thread).is_none_or(|t| t.state == ThreadState::Moribund)
            }
            Wait::Mutex(mutex) => self.mutexes.get(&mutex).is_none_or(|m| m.count == 0),
            Wait::Sleep(deadline) => now >= deadline,
            Wait::MessageReceive(queue) => read(queue + offset::MESSAGE_QUEUE_USED) > 0,
            Wait::MessageSend(queue) => {
                read(queue + offset::MESSAGE_QUEUE_USED) < read(queue + offset::MESSAGE_QUEUE_COUNT)
            }
            Wait::Event(event) => read(event + offset::EVENT_STATE) != 0,
            Wait::EventTimeout { event, deadline } => {
                read(event + offset::EVENT_STATE) != 0 || now >= deadline
            }
            Wait::Semaphore(semaphore) => {
                read(semaphore + offset::SEMAPHORE_COUNT).cast_signed() > 0
            }
            Wait::Cond(_) => false,
            Wait::Gpu(timestamp) => self.gpu_retired.as_ref().is_none_or(|retired| {
                retired.load(std::sync::atomic::Ordering::Acquire) >= timestamp
            }),
            Wait::Rendezvous { rendezvous, mask } => (0..3)
                .filter(|core| mask & (1 << core) != 0)
                .all(|core| read(rendezvous + 4 * core) != 0),
        }
    }

    fn runnable(&self, record: &ThreadRecord, memory: &Memory, now: u64) -> bool {
        match record.state {
            ThreadState::Ready | ThreadState::Running => true,
            ThreadState::Waiting(wait) => self.wait_satisfied(wait, memory, now),
            ThreadState::Suspended | ThreadState::Moribund => false,
        }
    }

    /// Chooses the thread to run and swaps register state on `cpu`.
    pub fn schedule(
        &mut self,
        cpu: &mut CpuState,
        memory: &Memory,
        now: u64,
        mode: Reschedule,
    ) -> Outcome {
        let current = self.current;
        let current_runnable =
            self.threads.get(&current).is_some_and(|t| t.state == ThreadState::Running);
        let current_core = cpu.core_id();
        let multi_core = self.multi_core;
        // The most important other runnable thread of `core`. Side by side, the other cores'
        // current threads are theirs.
        let best_on = |core: Option<u32>| {
            self.threads
                .iter()
                .filter(|(address, record)| {
                    **address != current
                        && !(multi_core && self.cores.contains(address))
                        && core.is_none_or(|c| record.context.core_id() == c)
                        && self.runnable(record, memory, now)
                })
                .min_by_key(|(_, record)| (record.priority, record.last_run))
                .map(|(address, record)| (*address, record.priority))
        };
        let current_priority = self.threads.get(&current).map_or(i32::MAX, |t| t.priority);
        // A blocked current thread whose wait is already satisfied may continue.
        let current_resumable = !current_runnable
            && self.threads.get(&current).is_some_and(|t| self.runnable(t, memory, now));

        let same_core = best_on(Some(current_core));
        // Side by side, a core runs only its own threads: round-robin among those of equal
        // priority, and the others' cores take care of theirs.
        let mode = match mode {
            Reschedule::Yield if multi_core && !current_runnable => Reschedule::Block,
            mode => mode,
        };
        let next = match mode {
            Reschedule::Block if current_runnable => None,
            Reschedule::Block if multi_core => {
                same_core.filter(|(_, p)| !(current_resumable && *p >= current_priority))
            }
            Reschedule::Yield if multi_core => same_core.filter(|(_, p)| *p <= current_priority),
            Reschedule::Block => {
                // Stay on the core if it has work; otherwise any core may run.
                let candidate = same_core.or_else(|| best_on(None));
                candidate.filter(|(_, p)| !(current_resumable && *p >= current_priority))
            }
            // Time-sliced like three cores running side by side: the other
            // cores' threads get their turn whatever their priority is, so a
            // thread spinning on this core cannot starve them.
            Reschedule::Yield => best_on(Some((current_core + 1) % CORE_COUNT))
                .or_else(|| best_on(Some((current_core + 2) % CORE_COUNT)))
                .or_else(|| same_core.filter(|(_, p)| *p <= current_priority)),
            Reschedule::Priority => same_core.filter(|(_, p)| *p < current_priority),
        }
        .map(|(thread, _)| thread);

        self.run_counter += 1;
        let counter = self.run_counter;
        match next {
            Some(thread) => {
                if let Some(old) = self.threads.get_mut(&current) {
                    old.context = cpu.clone();
                    if old.state == ThreadState::Running {
                        old.state = ThreadState::Ready;
                    }
                }
                if let Some(new) = self.threads.get_mut(&thread) {
                    if let ThreadState::Waiting(wait) = new.state {
                        new.woke_from = Some(wait);
                    }
                    new.state = ThreadState::Running;
                    new.last_run = counter;
                    *cpu = new.context.clone();
                }
                self.current = thread;
                Outcome::Switched
            }
            None if current_runnable => {
                if let Some(record) = self.threads.get_mut(&current) {
                    record.last_run = counter;
                }
                Outcome::Kept
            }
            None if current_resumable => {
                if let Some(record) = self.threads.get_mut(&current) {
                    if let ThreadState::Waiting(wait) = record.state {
                        record.woke_from = Some(wait);
                    }
                    record.state = ThreadState::Running;
                    record.last_run = counter;
                }
                Outcome::Kept
            }
            None => Outcome::Idle,
        }
    }

    /// `true` while the current thread cannot continue (the core is idle).
    /// One line per thread (debugging).
    pub fn report(&self) -> Vec<String> {
        self.threads
            .iter()
            .map(|(address, t)| {
                format!(
                    "{address:#010x} core {} prio {} {:?} pc {:#010x} lr {:#010x}",
                    t.context.core_id(),
                    t.priority,
                    t.state,
                    t.context.pc,
                    t.context.lr
                )
            })
            .collect()
    }

    #[inline]
    pub fn current_is_blocked(&self) -> bool {
        self.threads.get(&self.current).is_some_and(|t| t.state != ThreadState::Running)
    }
}

#[cfg(test)]
mod tests {
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
}
