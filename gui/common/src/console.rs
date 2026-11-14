//! The emulator on its own thread, driven by commands from the UI.
//!
//! ```text
//!  UI thread ──ConsoleCommand──► emulation thread (Emulator::run_frame @ 60 Hz)
//!      ▲                                   │ (run_frame keeps real time)
//!      └──────── Shared (Mutex) ◄──────────┘ render commands, console, status
//! ```
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError,
        mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use wiiu_emu::{EmuStats, Emulator, GamePadState, GameSpeed, RenderCommand, Status};

use crate::audio::{AudioOutput, Volume};

/// How often a stopped emulator looks for commands.
const IDLE_POLL: Duration = Duration::from_millis(66);
/// Rate of the samples the emulator produces.
const AUDIO_RATE: u32 = 48_000;
/// Longest time between repaints without a new picture: controllers are read when painting
/// (keys repaint by themselves). Titles flip all the time; this is for loading and pauses,
/// where drawing the window 30 times a second only kept the GPU busy.
const MAX_REPAINT_INTERVAL: Duration = Duration::from_millis(100);

/// Requests from the UI.
#[derive(Debug, Clone)]
pub enum ConsoleCommand {
    Load(PathBuf),
    Pause,
    Resume,
    Stop,
    /// Reboots the running title.
    Reset,
    /// Reads the running title's cheat codes again.
    ReloadCheats,
    /// Saves the machine to a state slot.
    SaveState(u8),
    /// Goes on from a state slot.
    LoadState(u8),
    GamePad(GamePadState),
    /// A Pro Controller on a `WPAD` channel; `None` unplugs it.
    ProController(usize, Option<GamePadState>),
    /// Where a copy of the sound goes (48 kHz stereo). Full means that part is dropped.
    AudioTap(Option<SyncSender<Vec<i16>>>),
    /// Where to find the system fonts from the next boot on.
    FontsDir(Option<PathBuf>),
    /// Where updates and downloadable content are looked for from the next boot on, as
    /// [`AppConfig::emulator_config`](crate::config::AppConfig::emulator_config) lists them.
    AddOnDirs {
        update_dirs: Vec<PathBuf>,
        dlc_dirs: Vec<PathBuf>,
    },
    /// Vertical blanks per flip from now on, whatever the title asks for.
    SwapInterval(Option<u32>),
    /// How many times larger the title's pictures are drawn from now on.
    ResolutionFactor(u32),
    /// How fast the title runs from now on.
    Speed(GameSpeed),
    /// Least vertical blanks per flip from now on: never faster than this.
    MinSwapInterval(Option<u32>),
}

/// State published by the emulation thread.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub console_lines: Vec<String>,
    pub status: Option<Status>,
    pub paused: bool,
    pub title: Option<String>,
    pub stats: EmuStats,
    /// Frames the guest flips to the screen per second.
    pub fps: f32,
    /// Error from the last `Load`.
    pub load_error: Option<String>,
    /// Where the running title's cheat codes are kept.
    pub cheats_path: Option<PathBuf>,
}

type Shared = Arc<Mutex<Snapshot>>;

/// Handle to the emulation thread. Dropping it stops the thread.
pub struct Console {
    tx: Option<Sender<ConsoleCommand>>,
    shared: Shared,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Console {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Console").finish_non_exhaustive()
    }
}

impl Console {
    /// Starts the emulation thread. `repaint` is called after each frame so
    /// the UI can wake up.
    ///
    /// # Errors
    /// Fails if the emulator cannot be created or the thread cannot spawn.
    pub fn spawn(
        config: wiiu_emu::Config,
        volume: Volume,
        render: SyncSender<Vec<RenderCommand>>,
        repaint: impl Fn() + Send + 'static,
    ) -> Result<Self, String> {
        let emulator = Emulator::new(config).map_err(|e| e.to_string())?;
        let shared = Shared::default();
        let (tx, rx) = mpsc::channel();
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("shocora-emu".into())
            .spawn(move || {
                EmuThread {
                    profile: std::env::var_os("SHOCORA_PROFILE")
                        .map(|_| Profile::new(emulator.stats())),
                    emulator,
                    rx,
                    render,
                    audio: AudioOutput::open(AUDIO_RATE, volume),
                    audio_tap: None,
                    shared: thread_shared,
                    paused: false,
                }
                .run(&repaint);
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { tx: Some(tx), shared, thread: Some(thread) })
    }

    /// A handle for sending commands from another thread. Keeps the emulation thread alive:
    /// drop it before the console.
    #[inline]
    pub fn commands(&self) -> Option<Sender<ConsoleCommand>> {
        self.tx.clone()
    }

    #[inline]
    pub fn send(&self, command: ConsoleCommand) {
        if let Some(tx) = &self.tx {
            // The thread only exits when `tx` is dropped, so this cannot fail while we hold it.
            let _ = tx.send(command);
        }
    }

    /// Takes accumulated output; status/stats are copied, lists drained.
    pub fn drain(&self) -> Snapshot {
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        Snapshot {
            console_lines: std::mem::take(&mut shared.console_lines),
            load_error: shared.load_error.take(),
            ..shared.clone()
        }
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Where the emulation thread's time went, logged every [`PROFILE_INTERVAL`] when
/// `SHOCORA_PROFILE` is set.
struct Profile {
    start: Instant,
    base: EmuStats,
    /// Inside `run_frame`: guest code, HLE, translation and pacing.
    frame: Duration,
    /// Waiting for the render thread to take the frame's commands.
    render_wait: Duration,
    audio_samples: u64,
    audio_peak: i16,
}

const PROFILE_INTERVAL: Duration = Duration::from_secs(5);

impl Profile {
    fn new(stats: EmuStats) -> Self {
        Self {
            start: Instant::now(),
            base: stats,
            frame: Duration::ZERO,
            render_wait: Duration::ZERO,
            audio_samples: 0,
            audio_peak: 0,
        }
    }

    fn audio(&mut self, samples: &[i16]) {
        self.audio_samples += samples.len() as u64;
        let peak = samples.iter().map(|s| s.saturating_abs()).max().unwrap_or(0);
        self.audio_peak = self.audio_peak.max(peak);
    }

    /// Logs and starts over once the interval is up.
    #[allow(clippy::cast_precision_loss)]
    fn report(&mut self, emulator: &Emulator) {
        let elapsed = self.start.elapsed();
        if elapsed < PROFILE_INTERVAL {
            return;
        }
        let stats = emulator.stats();
        let (now, base) = (&stats, &self.base);
        let secs = elapsed.as_secs_f64();
        let ms = |nanos: u64| nanos as f64 / 1e6 / secs;
        log::info!(
            "[profile] {:.1} fps | per second: frame {:.0} ms (guest {:.0}, hle {:.0},              compile {:.0}), render wait {:.0} ms | {:.0} M instr, {:.0} draws ({} skipped) |              audio {:.0} samples/s, peak {}",
            (now.gpu.flips - base.gpu.flips) as f64 / secs,
            self.frame.as_secs_f64() * 1e3 / secs,
            ms(now.guest_nanos - base.guest_nanos),
            ms(now.hle_nanos - base.hle_nanos),
            ms(now.compile_nanos - base.compile_nanos),
            self.render_wait.as_secs_f64() * 1e3 / secs,
            (now.instructions - base.instructions) as f64 / 1e6 / secs,
            (now.gpu.draws - base.gpu.draws) as f64 / secs,
            now.gpu.skipped_draws - base.gpu.skipped_draws,
            self.audio_samples as f64 / secs,
            self.audio_peak,
        );
        log::info!("[profile]   sound: {}", emulator.sound_summary());
        let ax: Vec<String> = emulator
            .hle_profile(usize::MAX)
            .into_iter()
            .filter(|(name, _)| name.starts_with("snd"))
            .map(|(name, calls)| format!("{}={calls}", name.rsplit("::").next().unwrap_or(&name)))
            .collect();
        log::info!("[profile]   sound calls: {}", ax.join(" "));
        for thread in emulator.thread_summary() {
            log::info!("[profile]   thread {thread}");
        }
        for (name, calls, nanos) in emulator.hle_slowest(8) {
            log::info!("[profile]   {name}: {calls} calls, {:.0} ms total", nanos as f64 / 1e6);
        }
        *self = Self::new(stats);
    }
}

struct EmuThread {
    profile: Option<Profile>,
    emulator: Emulator,
    rx: Receiver<ConsoleCommand>,
    render: SyncSender<Vec<RenderCommand>>,
    audio: AudioOutput,
    audio_tap: Option<SyncSender<Vec<i16>>>,
    shared: Shared,
    paused: bool,
}

impl EmuThread {
    fn running(&self) -> bool {
        !self.paused && *self.emulator.status() == Status::Running
    }

    fn report_state(&mut self, done: &str, result: Result<std::path::PathBuf, wiiu_emu::EmuError>) {
        match result {
            Ok(path) => self.emulator.log_line(format!("state {done}: {}", path.display())),
            Err(error) => {
                self.shared.lock().unwrap_or_else(PoisonError::into_inner).load_error =
                    Some(format!("state: {error}"));
            }
        }
    }

    fn handle(&mut self, command: ConsoleCommand) {
        match command {
            ConsoleCommand::Load(path) => {
                self.paused = false;
                let result = self.emulator.load_title(&path);
                let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
                match result {
                    Ok(()) => {
                        shared.title = Some(self.emulator.title().map_or_else(
                            || path.display().to_string(),
                            wiiu_emu::TitleInfo::display_name,
                        ));
                    }
                    Err(error) => shared.load_error = Some(format!("{}: {error}", path.display())),
                }
            }
            ConsoleCommand::Pause => self.paused = true,
            ConsoleCommand::Resume => self.paused = false,
            ConsoleCommand::Stop => {
                self.emulator.stop();
                self.shared.lock().unwrap_or_else(PoisonError::into_inner).title = None;
            }
            ConsoleCommand::Reset => {
                self.paused = false;
                if let Err(error) = self.emulator.reset() {
                    self.shared.lock().unwrap_or_else(PoisonError::into_inner).load_error =
                        Some(format!("reset: {error}"));
                }
            }
            ConsoleCommand::ReloadCheats => {
                if let Err(error) = self.emulator.reload_cheats() {
                    self.shared.lock().unwrap_or_else(PoisonError::into_inner).load_error =
                        Some(error);
                }
            }
            ConsoleCommand::SaveState(slot) => {
                // What the GPU drew comes from the render thread, after everything before it.
                let answer = self.emulator.request_surfaces();
                let commands = self.emulator.take_render_commands();
                let surfaces = match self.render.send(commands) {
                    Ok(()) => answer.recv().unwrap_or_default(),
                    Err(_) => Vec::new(),
                };
                let result = self.emulator.save_state(slot, surfaces);
                self.report_state("saved", result);
            }
            ConsoleCommand::LoadState(slot) => {
                let result = self.emulator.load_state(slot);
                self.report_state("loaded", result);
            }
            ConsoleCommand::GamePad(state) => self.emulator.set_gamepad(state),
            ConsoleCommand::ProController(channel, state) => {
                self.emulator.set_pro_controller(channel, state);
            }
            ConsoleCommand::AudioTap(tap) => self.audio_tap = tap,
            ConsoleCommand::FontsDir(dir) => self.emulator.config.fonts_dir = dir,
            ConsoleCommand::AddOnDirs { update_dirs, dlc_dirs } => {
                self.emulator.config.update_dirs = update_dirs;
                self.emulator.config.dlc_dirs = dlc_dirs;
            }
            ConsoleCommand::SwapInterval(interval) => self.emulator.set_swap_interval(interval),
            ConsoleCommand::ResolutionFactor(factor) => {
                self.emulator.set_resolution_factor(factor);
            }
            ConsoleCommand::Speed(speed) => self.emulator.set_speed(speed),
            ConsoleCommand::MinSwapInterval(interval) => {
                self.emulator.set_min_swap_interval(interval);
            }
        }
    }

    fn publish(&mut self, fps: f32) {
        // Before the renderer, which may take a while: sound must not run out meanwhile.
        let sound = self.emulator.take_audio();
        if let Some(profile) = &mut self.profile {
            profile.audio(&sound);
        }
        self.audio.push(&sound);
        if let Some(tap) = self.audio_tap.as_ref().filter(|_| !sound.is_empty()) {
            let _ = tap.try_send(sound);
        }
        let commands = self.emulator.take_render_commands();
        // Waits while the renderer is behind: frames must not be dropped, they
        // carry uploads and draws later frames build on.
        let waited = Instant::now();
        if !commands.is_empty() && self.render.send(commands).is_err() {
            log::warn!("render thread stopped");
        }
        if let Some(profile) = &mut self.profile {
            profile.render_wait += waited.elapsed();
        }
        let lines = self.emulator.take_console();
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        shared.console_lines.extend(lines);
        shared.status = Some(self.emulator.status().clone());
        shared.paused = self.paused;
        shared.stats = self.emulator.stats();
        shared.fps = fps;
        shared.cheats_path = self.emulator.cheats_path();
    }

    /// Emulation Start: runs the emulator until the UI thread drops its handle.
    fn run(mut self, repaint: &dyn Fn()) {
        let (mut fps_flips, mut fps_start, mut fps) = (0_u64, Instant::now(), 0.0_f32);
        let (mut painted_flips, mut painted_at) = (0_u64, Instant::now());

        // Main emulator loop
        loop {
            // Console commands are handled at the end of each frame.
            let command = if self.running() {
                self.rx.try_recv().map_err(|error| match error {
                    TryRecvError::Empty => RecvTimeoutError::Timeout,
                    TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
                })
            } else {
                self.rx.recv_timeout(IDLE_POLL)
            };

            match command {
                Ok(command) => {
                    self.handle(command);
                    while let Ok(command) = self.rx.try_recv() {
                        self.handle(command);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }

            if self.running() {
                let started = Instant::now();
                self.emulator.run_frame();
                if let Some(profile) = &mut self.profile {
                    profile.frame += started.elapsed();
                    profile.report(&self.emulator);
                }
            }

            let elapsed = fps_start.elapsed();
            if elapsed >= Duration::from_secs(1) {
                let flips = self.emulator.stats().gpu.flips;
                #[allow(clippy::cast_precision_loss)]
                {
                    fps = flips.saturating_sub(fps_flips) as f32 / elapsed.as_secs_f32();
                }
                (fps_flips, fps_start) = (flips, Instant::now());
            }
            self.publish(fps);
            // A title at 30 fps need not have the window and the desktop drawn 60 times a second.
            let flips = self.emulator.stats().gpu.flips;
            if flips != painted_flips || painted_at.elapsed() >= MAX_REPAINT_INTERVAL {
                (painted_flips, painted_at) = (flips, Instant::now());
                repaint();
            }
        }
    }
}
