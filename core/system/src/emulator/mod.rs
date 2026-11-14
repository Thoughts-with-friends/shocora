//! The [`Emulator`] aggregate and its public API.
mod bus;
pub mod emu_config;
mod load;
mod multi;
mod runner;
pub mod state;

use wiiu_cafe_os::CafeOs;
use wiiu_cpu::{CpuState, Jit};
use wiiu_gpu::{Gpu, GpuStats, RenderCommand};
use wiiu_iosu::Iosu;
use wiiu_memory::Memory;
use wiiu_peripherals::{GamePadState, GameSpeed};
use wiiu_sound::speed::SpeedResampler;
use wiiu_title::TitleInfo;

use self::emu_config::Config;
use crate::EmuError;

/// Number of Espresso cores.
pub const CORE_COUNT: usize = 3;

/// The core running the main thread.
pub const MAIN_CORE: usize = 1;

/// Lifecycle of the emulated process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Nothing loaded.
    Idle,
    Running,
    /// The process exited with this status.
    Exited(i32),
    /// Emulation stopped on an error.
    Crashed(String),
}

/// Counters for the front end.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmuStats {
    pub frames: u64,
    pub instructions: u64,
    pub hle_calls: u64,
    /// Host time in translated guest code, when profiling.
    pub guest_nanos: u64,
    /// Host time in HLE functions, when profiling.
    pub hle_nanos: u64,
    /// Host time cores side by side had nothing to run, summed over the cores (profiling).
    pub idle_nanos: u64,
    /// Host time cores side by side waited for each other's HLE calls (profiling).
    pub lock_nanos: u64,
    /// Host time cores side by side spent in lock-free HLE calls (profiling).
    pub fast_nanos: u64,
    /// Host time of the cores side by side from their start to the vertical blank, summed
    /// over the cores (profiling): what guest, HLE, idle and fast calls leave is overhead.
    pub core_nanos: u64,
    /// Host time in translated guest code per core side by side (profiling).
    pub core_guest_nanos: [u64; CORE_COUNT],
    /// Host time translating guest code.
    pub compile_nanos: u64,
    /// Blocks of guest code translated.
    pub compiled_blocks: u64,
    /// Guest instructions the interpreter ran while their code waited for translation.
    pub interpreted: u64,
    pub gpu: GpuStats,
}

/// What a reset boots again.
#[derive(Debug, Clone)]
enum BootSource {
    Title(std::path::PathBuf),
    Rpx(String, Vec<u8>),
}

/// A Wii U.
pub struct Emulator {
    boot_source: Option<BootSource>,
    /// Applied at every frame.
    cheats: Vec<crate::cheat::Cheat>,
    pub config: Config,
    memory: Memory,
    cores: Vec<CpuState>,
    jit: Jit,
    os: CafeOs,
    iosu: Iosu,
    gpu: Gpu,
    title: Option<TitleInfo>,
    status: Status,
    stats: EmuStats,
    /// How fast the guest's time runs.
    speed: GameSpeed,
    /// Makes the sound of faster or slower time fit real time.
    audio_speed: SpeedResampler,
    /// When the next vertical blank is due.
    next_vsync: Option<std::time::Instant>,
    /// Whether the cores run side by side now (see [`Config::multi_core`]).
    multi_core_running: bool,
    /// Where time slices ran out, when profiling: a guest spinning on something
    /// shows up as one address taking many.
    slice_ends: std::collections::HashMap<u32, u64>,
}

impl std::fmt::Debug for Emulator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Emulator")
            .field("status", &self.status)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

/// Asks Windows for 1 ms timers, as Cemu does: by default a wait with a timeout (an idle core
/// looking for sleeps and alarms that are due) lasts up to 15.6 ms, which late-wakes guest
/// threads by whole frames. Once per process; elsewhere timers are fine already.
fn fine_timers() {
    #[cfg(windows)]
    {
        #[link(name = "winmm")]
        unsafe extern "system" {
            fn timeBeginPeriod(period: u32) -> u32;
        }
        static ONCE: std::sync::Once = std::sync::Once::new();
        // SAFETY: a plain Win32 call without pointers; the period is never ended, which the
        // system does itself when the process exits.
        ONCE.call_once(|| unsafe {
            timeBeginPeriod(1);
        });
    }
}

impl Emulator {
    /// Creates a powered-on console with nothing loaded.
    ///
    /// # Errors
    /// Fails if the guest address space cannot be mapped.
    pub fn new(config: Config) -> Result<Self, EmuError> {
        fine_timers();
        Ok(Self {
            boot_source: None,
            cheats: Vec::new(),
            config,
            memory: Memory::with_game_layout()?,
            cores: (0..CORE_COUNT as u32).map(CpuState::new).collect(),
            jit: Jit::new(),
            os: CafeOs::new(),
            iosu: Iosu::default(),
            gpu: Gpu::new(),
            title: None,
            status: Status::Idle,
            stats: EmuStats::default(),
            speed: GameSpeed::Normal,
            audio_speed: SpeedResampler::default(),
            next_vsync: None,
            multi_core_running: false,
            slice_ends: std::collections::HashMap::new(),
        })
    }

    #[inline]
    pub const fn status(&self) -> &Status {
        &self.status
    }

    #[inline]
    pub const fn title(&self) -> Option<&TitleInfo> {
        self.title.as_ref()
    }

    pub fn stats(&self) -> EmuStats {
        EmuStats {
            gpu: self.gpu.stats(),
            compile_nanos: self.jit.compile_nanos(),
            compiled_blocks: self.jit.compiled_blocks(),
            interpreted: self.jit.interpreted(),
            ..self.stats
        }
    }

    /// Blocks waiting for their first translation and for optimizing (profiling).
    #[inline]
    pub fn jit_queue_lengths(&self) -> (usize, usize) {
        self.jit.queue_lengths()
    }

    /// Bytes of translated code: quick translations, then optimized code.
    #[inline]
    pub fn jit_code_bytes(&self) -> (usize, usize) {
        self.jit.code_bytes()
    }

    /// Register state of core `index` (for the debugger).
    #[inline]
    pub fn core(&self, index: usize) -> Option<&CpuState> {
        self.cores.get(index)
    }

    /// Vertical blanks per flip from now on, whatever the title asks for.
    #[inline]
    pub fn set_swap_interval(&mut self, interval: Option<u32>) {
        self.config.swap_interval = interval;
        self.os.gx2.forced_swap_interval = interval;
    }

    /// Draws the title's pictures this many times larger from now on.
    #[inline]
    pub fn set_resolution_factor(&mut self, factor: u32) {
        self.config.resolution_factor = factor;
        self.gpu.set_resolution_factor(factor);
    }

    /// Time runs at `speed` from now on: the title, its timers and its sound.
    #[inline]
    pub fn set_speed(&mut self, speed: GameSpeed) {
        self.speed = speed;
        self.os.timebase.set_speed(speed);
        // The next blank is due at the new pace, not when the old pace said.
        self.next_vsync = None;
    }

    /// Least vertical blanks per flip from now on.
    #[inline]
    pub fn set_min_swap_interval(&mut self, interval: Option<u32>) {
        self.config.min_swap_interval = interval;
        self.os.gx2.min_swap_interval = interval.unwrap_or(0);
    }

    /// Where the cheat codes of the running title are kept.
    pub fn cheats_path(&self) -> Option<std::path::PathBuf> {
        let title_id = self.title.as_ref()?.meta.title_id?;
        Some(self.config.cheats_dir.join(format!("{title_id:016x}.txt")))
    }

    /// Reads the running title's cheat codes again; no file means none.
    ///
    /// # Errors
    /// Fails if the file cannot be read or understood. The codes in use stay then.
    pub fn reload_cheats(&mut self) -> Result<usize, String> {
        let Some(path) = self.cheats_path() else {
            self.cheats.clear();
            return Ok(0);
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        self.cheats =
            crate::cheat::parse(&text).map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(self.cheats.len())
    }

    /// Feeds the latest GamePad sample.
    #[inline]
    pub const fn set_gamepad(&mut self, state: GamePadState) {
        self.os.gamepad = state;
    }

    /// Plugs in, updates or (with `None`) unplugs the Pro Controller on `channel`.
    #[inline]
    pub fn set_pro_controller(&mut self, channel: usize, state: Option<GamePadState>) {
        if let Some(slot) = self.os.pro_controllers.get_mut(channel) {
            *slot = state;
        }
    }

    /// Drains render commands produced since the last call.
    #[inline]
    pub fn take_render_commands(&mut self) -> Vec<RenderCommand> {
        self.gpu.take_render_commands()
    }

    /// Disassembly of `count` instructions starting `before` instructions ahead of `address`.
    pub fn disassemble_around(&self, address: u32, before: u32, count: u32) -> Vec<String> {
        (0..count)
            .map(|i| address.wrapping_sub(4 * before).wrapping_add(4 * i))
            .map(|at| match self.memory.read_u32(at) {
                Ok(raw) => format!(
                    "{at:#010x}: {raw:08x}  {}",
                    wiiu_cpu::disassemble::disassemble(at, raw)
                ),
                Err(_) => format!("{at:#010x}: <unmapped>"),
            })
            .collect()
    }

    /// Reads a guest word (debugging).
    #[inline]
    pub fn peek_u32(&self, address: u32) -> Option<u32> {
        self.memory.read_u32(address).ok()
    }

    /// One line per guest thread (debugging).
    #[inline]
    pub fn thread_report(&self) -> Vec<String> {
        self.os.coreinit.threads.report()
    }

    /// The most recent HLE calls (for crash reports).
    #[inline]
    pub fn hle_trace(&self) -> Vec<String> {
        self.os.registry.recent_calls()
    }

    /// IOSU: its devices, open handles and file system.
    #[inline]
    pub const fn iosu(&self) -> &Iosu {
        &self.iosu
    }

    /// The title's volumes as the guest sees them (`/vol/code`, `/vol/content`, `/vol/save`…).
    #[inline]
    pub const fn vfs(&self) -> &wiiu_fs::Vfs {
        self.iosu.vfs()
    }

    /// The most frequently called HLE functions (for profiling).
    #[inline]
    pub fn hle_profile(&self, limit: usize) -> Vec<(String, u64)> {
        self.os.registry.hottest(limit)
    }

    /// Where time slices ran out most often, when profiling: `(address, slices)`.
    pub fn slice_end_profile(&self, limit: usize) -> Vec<(u32, u64)> {
        let mut ends: Vec<_> = self.slice_ends.iter().map(|(pc, n)| (*pc, *n)).collect();
        ends.sort_by_key(|(_, slices)| std::cmp::Reverse(*slices));
        ends.truncate(limit);
        ends
    }

    /// The HLE functions that took longest, when profiling.
    #[inline]
    pub fn hle_slowest(&self, limit: usize) -> Vec<(String, u64, u64)> {
        self.os.registry.slowest(limit)
    }

    /// Every guest thread: name, priority, state and where it stopped (profiling, hangs).
    pub fn thread_summary(&self) -> Vec<String> {
        let scheduler = &self.os.coreinit.threads;
        scheduler
            .threads
            .iter()
            .map(|(address, record)| {
                let name = if record.name == 0 {
                    String::new()
                } else {
                    self.memory.read_c_string(record.name, 64).unwrap_or_default()
                };
                let current = if *address == scheduler.current {
                    " (current)".to_owned()
                } else if record.state == wiiu_cafe_os::coreinit::thread::ThreadState::Moribund {
                    format!(" exit {}", record.exit_value)
                } else {
                    String::new()
                };
                format!(
                    "{address:#010x} {name:?} prio {} {:?}{current} pc {:#010x} lr {:#010x}",
                    record.priority, record.state, record.context.pc, record.context.lr
                )
            })
            .collect()
    }

    /// What the title's sound voices are doing (profiling).
    #[inline]
    pub fn sound_summary(&self) -> String {
        self.os.ax.summary()
    }

    /// Drains mixed audio: interleaved stereo at [`wiiu_cafe_os::sndcore2::OUTPUT_RATE`].
    pub fn take_audio(&mut self) -> Vec<i16> {
        let samples: Vec<i16> = self.os.ax.output.drain(..).collect();
        self.audio_speed.process(&samples, self.speed.halves())
    }

    /// Drains guest console output (`OSReport`, ...).
    #[inline]
    pub fn take_console(&mut self) -> Vec<String> {
        std::mem::take(&mut self.os.console)
    }

    /// Stops emulation and frees the process.
    pub fn stop(&mut self) {
        self.gpu.blank_screens();
        self.status = Status::Idle;
        self.title = None;
        self.boot_source = None;
    }
}
