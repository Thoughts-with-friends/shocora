//! Finishing command encoders on threads of their own.
//!
//! wgpu does most of a frame's work in `CommandEncoder::finish`: every recorded command is
//! validated and tracked there, and only then encoded for the driver. That took half of the
//! render thread (Breath of the Wild's field: four thousand draws a frame). Cemu records
//! straight into Vulkan and submits every few hundred draws; here the frame is cut into parts
//! as it is recorded, other threads finish the parts while later ones are recorded, and the
//! render thread submits them in their order.
//!
//! ```text
//!  render thread: record part 1 ─► record part 2 ─► record part 3 ─► ...
//!                       │                │
//!  finish threads:      └► finish 1      └► finish 2
//!                              │                │
//!  render thread:              └► submit 1 ─────└► submit 2 (always in order)
//! ```
//!
//! Queue writes still come from the render thread. A write is carried out before the next
//! submit, which may be one of an earlier part: the arena only ever writes past what earlier
//! parts read (its wrap waits for every part), and the upload ring starts over only once all
//! parts are submitted.
use std::collections::VecDeque;

use super::*;

/// Threads finishing encoders: two keep up with recording.
const FINISH_THREADS: usize = 2;
/// Parts waiting to be submitted before recording waits for the oldest: bounds how far the
/// host's commands run ahead of the driver's.
const MAX_PENDING_PARTS: usize = 16;

type FinishJob = (wgpu::CommandEncoder, Sender<wgpu::CommandBuffer>);

pub(super) struct Finisher {
    jobs: Option<Sender<FinishJob>>,
    /// Parts handed over, oldest first.
    pending: VecDeque<Receiver<wgpu::CommandBuffer>>,
    /// Parts handed over, and parts submitted, since the start.
    handed_over: u64,
    submitted: u64,
}

impl Finisher {
    pub(super) fn new() -> Self {
        let (jobs, receiver) = mpsc::channel::<FinishJob>();
        let receiver = Arc::new(Mutex::new(receiver));
        let mut started = 0;
        for index in 0..FINISH_THREADS {
            let receiver = Arc::clone(&receiver);
            let spawned = std::thread::Builder::new()
                .name(format!("shocora-finish {index}"))
                .spawn(move || {
                    loop {
                        // The lock is held only while waiting: one thread takes each part.
                        let job = receiver.lock().unwrap_or_else(PoisonError::into_inner).recv();
                        let Ok((encoder, reply)) = job else { break };
                        // Nobody waiting any more (a reboot) is no reason to stop.
                        let _ = reply.send(encoder.finish());
                    }
                });
            if spawned.is_ok() {
                started += 1;
            }
        }
        // Without threads, parts are finished where they are handed over.
        let jobs = (started > 0).then_some(jobs);
        Self { jobs, pending: VecDeque::new(), handed_over: 0, submitted: 0 }
    }

    /// Parts handed over so far: the number of the last one.
    pub(super) const fn handed_over(&self) -> u64 {
        self.handed_over
    }

    /// Parts submitted so far.
    pub(super) const fn submitted(&self) -> u64 {
        self.submitted
    }

    /// Has `encoder` finished, then submits what is done, in order.
    pub(super) fn hand_over(&mut self, queue: &wgpu::Queue, encoder: wgpu::CommandEncoder) {
        let (reply, finished) = mpsc::channel();
        let encoder = match &self.jobs {
            Some(jobs) => jobs.send((encoder, reply)).err().map(|error| error.0.0),
            None => Some(encoder),
        };
        if let Some(encoder) = encoder {
            // No threads (any more): finished right here, in turn with the parts before it.
            self.wait_all(queue);
            queue.submit([encoder.finish()]);
            self.handed_over += 1;
            self.submitted += 1;
            return;
        }
        self.pending.push_back(finished);
        self.handed_over += 1;
        while self.pending.len() > MAX_PENDING_PARTS {
            self.submit_oldest(queue);
        }
        self.submit_ready(queue);
    }

    /// Submits the parts finished so far, in order, without waiting.
    pub(super) fn submit_ready(&mut self, queue: &wgpu::Queue) {
        while let Some(front) = self.pending.front() {
            match front.try_recv() {
                Ok(buffer) => {
                    self.pending.pop_front();
                    queue.submit([buffer]);
                    self.submitted += 1;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    // Its thread died with it: what it held is lost, the rest goes on.
                    self.pending.pop_front();
                    self.submitted += 1;
                }
            }
        }
    }

    fn submit_oldest(&mut self, queue: &wgpu::Queue) {
        if let Some(front) = self.pending.pop_front() {
            if let Ok(buffer) = front.recv() {
                queue.submit([buffer]);
            }
            self.submitted += 1;
        }
    }

    /// Waits for every part handed over and submits them.
    pub(super) fn wait_all(&mut self, queue: &wgpu::Queue) {
        while !self.pending.is_empty() {
            self.submit_oldest(queue);
        }
    }
}

impl Drop for Finisher {
    fn drop(&mut self) {
        // The threads end once the sender is gone.
        self.jobs = None;
    }
}
