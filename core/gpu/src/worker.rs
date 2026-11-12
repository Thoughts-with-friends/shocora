//! The GPU as the CPU sees it, executing on the caller's thread or on one of its own.
//!
//! ```text
//!  GX2 (CPU thread) ──submit──► jobs ──► GPU thread: Engine (PM4, Latte) ──► render commands
//!        ▲                                         │
//!        └──── retired timestamp, waits ◄──────────┘
//! ```
//!
//! Like Cemu's `LatteThread`, the GPU thread works through command buffers while the CPU
//! goes on. Both see one guest memory, as Espresso and Latte do on the console, and the CPU
//! waits only where the console's waits for its GPU: `GX2DrawDone`, timestamps, or a GPU
//! that is frames behind.
use std::{
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::Duration,
};

use wiiu_memory::Memory;

use crate::{
    GpuError, GpuState, GpuStats, RenderCommand, gpu::Engine, regs::RegisterFile,
    render::TextureUpload,
};

/// Frames the CPU may be ahead of the GPU thread before `GX2SwapScanBuffers` waits for it,
/// like the command ring of the console filling up.
const MAX_FRAMES_AHEAD: usize = 2;
/// A wait looks again this often whether the GPU thread is still there.
const WAIT_CHECK: Duration = Duration::from_millis(100);

/// Work for the GPU thread, done in the order it was given.
enum Job {
    Submit { memory: Memory, words: Vec<u32> },
    CpuWrite { address: u32, size: u32 },
    Run(Box<dyn FnOnce(&mut Engine) + Send>),
}

/// What the GPU thread reports back.
#[derive(Default)]
struct Shared {
    /// Command buffers executed so far: the GPU's retired timestamp.
    retired: Mutex<u64>,
    retired_changed: Condvar,
    /// The same, for the thread scheduler, which looks without waiting.
    retired_now: Arc<AtomicU64>,
    /// Render commands produced and not yet taken by the front end.
    commands: Mutex<Vec<RenderCommand>>,
    stats: Mutex<GpuStats>,
    /// The first error since it was last reported.
    error: Mutex<Option<GpuError>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The GPU thread and what the CPU keeps track of.
struct Worker {
    jobs: Option<Sender<Job>>,
    thread: Option<std::thread::JoinHandle<()>>,
    shared: Arc<Shared>,
    /// Command buffers handed over: the last submitted timestamp.
    submitted: u64,
    /// The submitted timestamp at each of the last swaps.
    frames: VecDeque<u64>,
}

impl Worker {
    fn spawn(engine: Engine) -> Self {
        let shared = Arc::new(Shared::default());
        let submitted = engine.timestamp;
        *lock(&shared.retired) = submitted;
        shared.retired_now.store(submitted, Ordering::Release);
        let (jobs, receiver) = mpsc::channel();
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("shocora gpu".to_owned())
            .spawn(move || run(engine, &receiver, &thread_shared))
            .inspect_err(|error| log::error!("cannot start the GPU thread: {error}"))
            .ok();
        Self { jobs: Some(jobs), thread, shared, submitted, frames: VecDeque::new() }
    }

    fn send(&self, job: Job) -> bool {
        self.jobs.as_ref().is_some_and(|jobs| jobs.send(job).is_ok())
    }

    fn alive(&self) -> bool {
        self.thread.as_ref().is_some_and(|thread| !thread.is_finished())
    }

    /// Waits until the command buffer `timestamp` has been executed.
    fn wait_retired(&self, timestamp: u64) {
        let target = timestamp.min(self.submitted);
        let mut retired = lock(&self.shared.retired);
        while *retired < target && self.alive() {
            retired = self
                .shared
                .retired_changed
                .wait_timeout(retired, WAIT_CHECK)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        drop(retired);
    }

    /// Waits until the GPU thread has done every job given so far.
    fn wait_idle(&self) {
        let (reply, answer) = mpsc::channel::<()>();
        if self.send(Job::Run(Box::new(move |_| {
            let _ = reply.send(());
        }))) {
            let _ = answer.recv();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Without anyone to give it jobs, the thread ends.
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The GPU thread.
fn run(mut engine: Engine, jobs: &Receiver<Job>, shared: &Shared) {
    let mut busy_nanos = 0;
    while let Ok(job) = jobs.recv() {
        let started = std::time::Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| match job {
            Job::Submit { mut memory, words } => engine.submit(&mut memory, &words),
            Job::CpuWrite { address, size } => {
                engine.note_cpu_write(address, size);
                Ok(())
            }
            Job::Run(work) => {
                // The front end must see what came before `work` before it sees its answer.
                publish(&mut engine, shared);
                work(&mut engine);
                Ok(())
            }
        }));
        let error = match result {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error),
            Err(panic) => {
                let message = panic
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_owned())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                Some(GpuError::Panicked(message))
            }
        };
        if let Some(error) = error {
            lock(&shared.error).get_or_insert(error);
        }
        busy_nanos += started.elapsed().as_nanos() as u64;
        publish(&mut engine, shared);
        lock(&shared.stats).busy_nanos = busy_nanos;
    }
}

/// Hands render commands, counters and the retired timestamp to the CPU's side.
fn publish(engine: &mut Engine, shared: &Shared) {
    let commands = engine.take_render_commands();
    if !commands.is_empty() {
        lock(&shared.commands).extend(commands);
    }
    {
        let mut stats = lock(&shared.stats);
        *stats = GpuStats { busy_nanos: stats.busy_nanos, ..engine.stats() };
    }
    *lock(&shared.retired) = engine.timestamp;
    shared.retired_now.store(engine.timestamp, Ordering::Release);
    shared.retired_changed.notify_all();
}

/// Tells the GPU thread what the CPU wrote, from any core and without `&mut Gpu`: in order
/// with the command buffers submitted meanwhile, as [`Gpu::note_cpu_write`] does.
#[derive(Clone)]
pub struct WriteNotifier(Sender<Job>);

impl WriteNotifier {
    #[inline]
    pub fn note(&self, address: u32, size: u32) {
        let _ = self.0.send(Job::CpuWrite { address, size });
    }
}

enum Mode {
    /// Command buffers run when they are submitted, on the caller's thread.
    Inline(Box<Engine>),
    Threaded(Worker),
}

/// Latte, as the CPU sees it: command buffers go in, render commands come out.
///
/// [`Gpu::new`] executes command buffers as they are submitted. [`Gpu::threaded`] hands them
/// to a thread of its own, like Cemu's GPU thread.
pub struct Gpu {
    mode: Mode,
}

impl std::fmt::Debug for Gpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let threaded = matches!(self.mode, Mode::Threaded(_));
        f.debug_struct("Gpu").field("threaded", &threaded).finish_non_exhaustive()
    }
}

impl Default for Gpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Gpu {
    /// A GPU executing command buffers as they are submitted.
    #[inline]
    pub fn new() -> Self {
        Self { mode: Mode::Inline(Box::default()) }
    }

    /// A GPU with a thread of its own.
    #[inline]
    pub fn threaded() -> Self {
        Self { mode: Mode::Threaded(Worker::spawn(Engine::default())) }
    }

    /// Whether command buffers run on a thread of their own.
    #[inline]
    pub const fn is_threaded(&self) -> bool {
        matches!(self.mode, Mode::Threaded(_))
    }

    /// Runs `work` on the engine after everything submitted so far; `None` if that failed.
    fn call<R: Send + 'static>(
        &mut self,
        work: impl FnOnce(&mut Engine) -> R + Send + 'static,
    ) -> Option<R> {
        match &mut self.mode {
            Mode::Inline(engine) => Some(work(engine)),
            Mode::Threaded(worker) => {
                let (reply, answer) = mpsc::channel();
                let job = Job::Run(Box::new(move |engine| {
                    let _ = reply.send(work(engine));
                }));
                if !worker.send(job) {
                    return None;
                }
                let result = answer.recv().ok();
                // What `work` produced is published once the job is over.
                worker.wait_idle();
                result
            }
        }
    }

    /// Asks the renderer for what the GPU drew, which guest memory does not hold. The answer
    /// comes once the commands queued so far have reached the renderer; without one, the
    /// receiver just reports its sender gone.
    pub fn request_surfaces(&mut self) -> Receiver<Vec<TextureUpload>> {
        self.call(Engine::request_surfaces).unwrap_or_else(|| mpsc::channel().1)
    }

    /// What a saved state keeps of the GPU, once it has executed everything submitted.
    pub fn save_state(&mut self, surfaces: Vec<TextureUpload>) -> Option<GpuState> {
        self.call(move |engine| engine.save_state(surfaces))
    }

    /// Goes on from `state`; the host renderer starts over like at a boot.
    pub fn restore_state(&mut self, state: GpuState, resolution_factor: u32) {
        let timestamp = state.timestamp;
        self.call(move |engine| engine.restore_state(state, resolution_factor));
        if let Mode::Threaded(worker) = &mut self.mode {
            worker.submitted = timestamp;
            worker.frames.clear();
            *lock(&worker.shared.retired) = timestamp;
            worker.shared.retired_now.store(timestamp, Ordering::Release);
        }
    }

    /// Executes a PM4 command buffer living in `memory`, now or on the GPU thread.
    ///
    /// # Errors
    /// Fails on malformed packets or accesses outside guest memory. On the GPU thread, the
    /// error of an earlier command buffer is reported by a later call.
    pub fn submit(&mut self, memory: &mut Memory, words: &[u32]) -> Result<(), GpuError> {
        match &mut self.mode {
            Mode::Inline(engine) => engine.submit(memory, words),
            Mode::Threaded(worker) => {
                let error = lock(&worker.shared.error).take();
                if let Some(error) = error {
                    return Err(error);
                }
                worker.submitted += 1;
                let job = Job::Submit { memory: memory.share(), words: words.to_vec() };
                if worker.send(job) { Ok(()) } else { Err(GpuError::Stopped) }
            }
        }
    }

    /// The last command buffer submitted.
    #[inline]
    pub fn submitted_timestamp(&self) -> u64 {
        match &self.mode {
            Mode::Inline(engine) => engine.timestamp,
            Mode::Threaded(worker) => worker.submitted,
        }
    }

    /// The last command buffer the GPU has executed.
    #[inline]
    pub fn retired_timestamp(&self) -> u64 {
        match &self.mode {
            Mode::Inline(engine) => engine.timestamp,
            Mode::Threaded(worker) => *lock(&worker.shared.retired),
        }
    }

    /// Waits until the command buffer `timestamp` has been executed.
    #[inline]
    pub fn wait_timestamp(&self, timestamp: u64) {
        if let Mode::Threaded(worker) = &self.mode {
            worker.wait_retired(timestamp);
        }
    }

    /// Waits until the GPU has done everything it was given (`GX2DrawDone`).
    #[inline]
    pub fn wait_idle(&self) {
        if let Mode::Threaded(worker) = &self.mode {
            worker.wait_idle();
        }
    }

    /// The retired timestamp as it changes, for whoever checks waits without `&Gpu`; `None`
    /// when command buffers are executed as they are submitted.
    pub fn retired_counter(&self) -> Option<Arc<AtomicU64>> {
        match &self.mode {
            Mode::Inline(_) => None,
            Mode::Threaded(worker) => Some(Arc::clone(&worker.shared.retired_now)),
        }
    }

    /// Before the title swaps its scan buffers: the command buffer to wait for when the CPU
    /// is [`MAX_FRAMES_AHEAD`] frames ahead of the GPU, like a full command ring.
    pub fn frame_wait(&mut self) -> Option<u64> {
        let Mode::Threaded(worker) = &mut self.mode else { return None };
        let retired = worker.shared.retired_now.load(Ordering::Acquire);
        while worker.frames.front().is_some_and(|frame| *frame <= retired) {
            worker.frames.pop_front();
        }
        (worker.frames.len() >= MAX_FRAMES_AHEAD).then(|| worker.frames.front().copied()).flatten()
    }

    /// The title swapped its scan buffers: the frame ends with what was submitted so far.
    pub fn end_frame(&mut self) {
        if let Mode::Threaded(worker) = &mut self.mode {
            worker.frames.push_back(worker.submitted);
        }
    }

    /// Draws the title's pictures this many times larger, as far as that stays within
    /// [`ResolutionScale::MAX_WIDTH`](crate::render::ResolutionScale::MAX_WIDTH).
    #[inline]
    pub fn set_resolution_factor(&mut self, factor: u32) {
        self.call(move |engine| engine.set_resolution_factor(factor));
    }

    /// Makes the host renderer forget everything of the previous boot.
    #[inline]
    pub fn reset_renderer(&mut self) {
        self.call(Engine::reset_renderer);
    }

    /// Blanks both screens (boot / shutdown).
    #[inline]
    pub fn blank_screens(&mut self) {
        self.call(Engine::blank_screens);
    }

    /// Drains pending render commands for the host renderer.
    #[inline]
    pub fn take_render_commands(&mut self) -> Vec<RenderCommand> {
        match &mut self.mode {
            Mode::Inline(engine) => engine.take_render_commands(),
            Mode::Threaded(worker) => std::mem::take(&mut *lock(&worker.shared.commands)),
        }
    }

    /// Render commands waiting for the host, to keep the queue bounded.
    #[inline]
    pub fn pending_commands(&self) -> usize {
        match &self.mode {
            Mode::Inline(engine) => engine.pending_commands(),
            Mode::Threaded(worker) => lock(&worker.shared.commands).len(),
        }
    }

    /// The CPU wrote `[address, address + size)` (effective) for the GPU to read: the title
    /// flushed it from the data cache, or the system wrote it.
    #[inline]
    pub fn note_cpu_write(&mut self, address: u32, size: u32) {
        match &mut self.mode {
            Mode::Inline(engine) => engine.note_cpu_write(address, size),
            Mode::Threaded(worker) => {
                worker.send(Job::CpuWrite { address, size });
            }
        }
    }

    /// A way to note CPU writes from other threads, when the GPU has a thread of its own.
    pub fn write_notifier(&self) -> Option<WriteNotifier> {
        match &self.mode {
            Mode::Inline(_) => None,
            Mode::Threaded(worker) => worker.jobs.clone().map(WriteNotifier),
        }
    }

    /// The command processor's registers, once everything submitted has been executed.
    pub fn registers(&mut self) -> RegisterFile {
        self.call(|engine| engine.registers().clone()).unwrap_or_default()
    }

    pub fn stats(&self) -> GpuStats {
        match &self.mode {
            Mode::Inline(engine) => engine.stats(),
            Mode::Threaded(worker) => *lock(&worker.shared.stats),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{opcode, pm4::type3_header};

    fn memory() -> Memory {
        let mut memory = Memory::new();
        memory
            .map(wiiu_mem_addr::Region::new("MEM2", wiiu_mem_addr::MEM2.start, 0x10_0000))
            .unwrap();
        memory
    }

    #[test]
    fn the_thread_executes_in_order_and_retires_timestamps() {
        let mut memory = memory();
        let mut gpu = Gpu::threaded();
        let words = [type3_header(opcode::SET_CONFIG_REG, 2), 0x10, 0xABCD];
        gpu.submit(&mut memory, &words).unwrap();
        gpu.submit(&mut memory, &words).unwrap();
        assert_eq!(gpu.submitted_timestamp(), 2);
        gpu.wait_timestamp(2);
        assert_eq!(gpu.retired_timestamp(), 2);
        assert_eq!(gpu.registers().get(crate::regs::base::CONFIG + 0x10), 0xABCD);
    }

    #[test]
    fn the_thread_writes_guest_memory_the_cpu_sees() {
        let mut memory = memory();
        let mut gpu = Gpu::threaded();
        let address = wiiu_mem_addr::MEM2.start + 0x100;
        let physical = wiiu_mem_addr::effective_to_physical(address).unwrap();
        // MEM_WRITE: address, then 32-bit data.
        let words = [type3_header(opcode::MEM_WRITE, 4), physical, 0, 0x1234_5678, 0];
        gpu.submit(&mut memory, &words).unwrap();
        gpu.wait_idle();
        assert_eq!(memory.read_u32(address).unwrap(), 0x1234_5678);
    }
}
