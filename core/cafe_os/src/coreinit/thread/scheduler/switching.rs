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
//! │ threads ready to   │   │ coreinit/thread/schedule │   │ thread to run next │
//! │ run                │   │ r/switching.rs           │   │                    │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ thread scheduler:        │   │                    │
//! │                    │   │ switching                │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "switching" share of scheduler.rs (thread scheduler), moved here so that no file is
//! longer than 500 lines. It works exactly as if it were still written in that file. Picks
//! which game thread runs next on each core, and switches between them, like Windows' scheduler
//! does for your programs. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;

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

    pub(super) fn wait_satisfied(&self, wait: Wait, memory: &Memory, now: u64) -> bool {
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

    pub(super) fn runnable(&self, record: &ThreadRecord, memory: &Memory, now: u64) -> bool {
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
                    old.context.clone_from(cpu);
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
                    cpu.clone_from(&new.context);
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
                let owner = match t.state {
                    ThreadState::Waiting(Wait::Mutex(key)) => self
                        .mutexes
                        .get(&key)
                        .map(|m| format!(" (owner {:#010x} x{})", m.owner, m.count))
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                format!(
                    "{address:#010x} core {} prio {} {:?}{owner} pc {:#010x} lr {:#010x}",
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
