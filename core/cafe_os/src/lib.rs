//! Cafe OS HLE.
//!
//! ```text
//!  guest `bl OSReport` ──► stub [hle_call N; blr] ──► StepEvent::HleCall(N)
//!                                                          │ (wiiu_emu)
//!                                                          ▼
//!                       hle::dispatch ──► registry entry N ──► coreinit::report::os_report
//! ```
//!
//! * [`hle`] — calling convention glue (`FromArg`/`IntoReturn`, [`HleContext`]).
//! * [`registry`] — export tables, stub generation, import resolution.
//! * [`coreinit`], [`gx2`], [`vpad`] — one module per system RPL.
//! * [`process`] — boot: load the RPX, create the default heap and main stack.
//!
//! Only the main thread runs for now; thread-related exports behave as if no
//! other thread exists.
pub mod aoc;
pub mod coreinit;
pub mod fast;
pub mod gx2;
pub mod h264;
pub mod hle;
pub mod mic;
pub mod nn;
pub mod patches;
pub mod proc_ui;
pub mod process;
pub mod registry;
pub mod sndcore2;
pub mod vpad;
pub mod zlib125;

pub use hle::{HleContext, HleError, dispatch};
pub use process::{BootError, BootInfo};
pub use registry::HleRegistry;
use wiiu_peripherals::{GamePadState, TimeBase, controller::ButtonEdges};

/// All Cafe OS state that lives outside guest memory.
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct CafeOs {
    /// Function pointers: taken over from the running title when a state is loaded.
    #[serde(skip)]
    pub registry: HleRegistry,
    pub timebase: TimeBase,
    pub coreinit: coreinit::CoreinitState,
    pub gx2: gx2::Gx2State,
    pub proc_ui: proc_ui::ProcUiState,
    pub zlib: zlib125::ZlibState,
    pub ax: sndcore2::AxState,
    /// Entry point to run once `__preinit_user` returns.
    pub pending_main: Option<process::MainEntry>,
    /// Guest callbacks waiting to run / running.
    pub callbacks: hle::callback::Callbacks,
    /// Region and language reported to the title.
    pub settings: coreinit::settings::SystemSettings,
    /// Latest GamePad sample from the front end.
    pub gamepad: GamePadState,
    pub(crate) gamepad_edges: ButtonEdges,
    /// Wii U Pro Controllers by channel, as `WPAD`/`KPAD` see them.
    pub pro_controllers: [Option<GamePadState>; nn::PRO_CHANNELS],
    pub(crate) pro_edges: [ButtonEdges; nn::PRO_CHANNELS],
    /// When each channel was last read: reading again too soon finds no new sample.
    #[serde(skip)]
    pub(crate) pro_last_read: [Option<std::time::Instant>; nn::PRO_CHANNELS],
    /// Raw panel coordinates of the last touch of the GamePad.
    pub(crate) last_touch: (u16, u16),
    /// Lines printed by `OSReport` and friends, drained by the front end.
    #[serde(skip)]
    pub console: Vec<String>,
    /// Set by `exit`/`_Exit`.
    pub exit_code: Option<i32>,
    /// Set when the guest waits for vsync: the current frame should end.
    pub frame_done: bool,
    /// Downloadable content installed for the title, as `nn_aoc` lists it.
    pub aoc: Vec<aoc::AocTitle>,
    /// Movie decoders waiting for their output callback to return.
    #[serde(default)]
    pub h264: h264::H264State,
    /// The GamePad's microphone.
    #[serde(default)]
    pub mic: mic::MicState,
}

impl CafeOs {
    pub fn new() -> Self {
        let mut registry = HleRegistry::new();

        registry.register_functions(coreinit::EXPORTS);
        registry.register_functions(coreinit::thread::EXPORTS);
        registry.register_data(coreinit::DATA_EXPORTS);
        registry.register_functions(gx2::EXPORTS);
        registry.register_functions(gx2::command::EXPORTS);
        registry.register_functions(gx2::draw::EXPORTS);
        registry.register_functions(gx2::state::EXPORTS);
        registry.register_functions(vpad::EXPORTS);
        registry.register_functions(proc_ui::EXPORTS);
        registry.register_functions(zlib125::EXPORTS);
        registry.register_functions(process::EXPORTS);
        registry.register_functions(coreinit::fs::mutate::EXPORTS);
        registry.register_functions(nn::EXPORTS);
        registry.register_functions(aoc::EXPORTS);
        registry.register_functions(sndcore2::EXPORTS);
        registry.register_functions(h264::EXPORTS);
        registry.register_functions(mic::EXPORTS);
        registry.register_functions(patches::EXPORTS);

        Self {
            registry,
            timebase: TimeBase::new(),
            coreinit: coreinit::CoreinitState::default(),
            gx2: gx2::Gx2State::default(),
            proc_ui: proc_ui::ProcUiState::default(),
            zlib: zlib125::ZlibState::default(),
            ax: sndcore2::AxState::default(),
            pending_main: None,
            callbacks: hle::callback::Callbacks::default(),
            settings: coreinit::settings::SystemSettings::default(),
            gamepad: GamePadState::default(),
            gamepad_edges: ButtonEdges::default(),
            pro_controllers: [None; nn::PRO_CHANNELS],
            pro_edges: [ButtonEdges::default(); nn::PRO_CHANNELS],
            pro_last_read: [None; nn::PRO_CHANNELS],
            last_touch: (0, 0),
            console: Vec::new(),
            exit_code: None,
            frame_done: false,
            aoc: Vec::new(),
            h264: h264::H264State::default(),
            mic: mic::MicState::default(),
        }
    }

    /// Appends a line to the guest console and the log.
    pub fn print(&mut self, line: impl Into<String>) {
        let line = line.into();
        log::info!(target: "guest", "{}", line.trim_end());
        self.console.push(line);
    }
}

impl Default for CafeOs {
    fn default() -> Self {
        Self::new()
    }
}

impl CafeOs {
    /// Runs the scheduler on `cpu` (the main core's live registers).
    ///
    /// When no thread can run, the frame ends so time can pass.
    pub fn schedule(
        &mut self,
        cpu: &mut wiiu_cpu::CpuState,
        memory: &wiiu_memory::Memory,
        mode: coreinit::thread::Reschedule,
    ) -> coreinit::thread::Outcome {
        let now = self.timebase.ticks_since_boot();
        let outcome = self.coreinit.threads.schedule(cpu, memory, now, mode);
        if outcome == coreinit::thread::Outcome::Idle {
            self.frame_done = true;
        }
        outcome
    }

    /// `true` while the running thread is blocked (nothing to execute).
    #[inline]
    pub fn current_thread_blocked(&self) -> bool {
        self.coreinit.threads.current_is_blocked()
    }

    /// Whether a guest callback borrows `core`. Side by side, callbacks run on the main
    /// core only; otherwise the one core there is runs them.
    #[inline]
    pub const fn callback_active_on(&self, core: u32) -> bool {
        self.callbacks.active() && (!self.coreinit.threads.multi_core || core == CALLBACK_CORE)
    }

    /// Whether callbacks may start on `core`.
    #[inline]
    pub const fn runs_callbacks(&self, core: u32) -> bool {
        !self.coreinit.threads.multi_core || core == CALLBACK_CORE
    }
}

/// The core running guest callbacks (alarms, completions) when the cores run side by side.
pub const CALLBACK_CORE: u32 = 1;

impl CafeOs {
    /// Starts the next queued guest callback on `cpu`, if the core is free.
    ///
    /// # Errors
    /// Fails if the callback return stub cannot be written.
    pub fn start_callback(
        &mut self,
        cpu: &mut wiiu_cpu::CpuState,
        memory: &mut wiiu_memory::Memory,
    ) -> Result<bool, wiiu_memory::MemoryError> {
        if self.callbacks.active()
            || !self.callbacks.has_pending()
            || !self.runs_callbacks(cpu.core_id())
        {
            return Ok(false);
        }
        let stub =
            self.registry.function_stub(process::INTERNAL_LIBRARY, process::CALLBACK_RETURN.name);
        self.registry.commit(memory)?;
        Ok(self.callbacks.begin(cpu, stub))
    }

    /// Queues handlers of alarms that are due, and of GPU interrupts that came.
    pub fn fire_due_alarms(&mut self) {
        self.fire_gpu_interrupts();
        let now = self.timebase.ticks_since_epoch();
        for call in self.coreinit.alarms.fire_due(now) {
            self.callbacks.push(call);
        }
    }

    /// Start-of-frame housekeeping: alarms, callbacks, waking blocked threads.
    ///
    /// # Errors
    /// Fails if guest memory for stubs is unmapped.
    pub fn begin_frame(
        &mut self,
        cpu: &mut wiiu_cpu::CpuState,
        memory: &mut wiiu_memory::Memory,
    ) -> Result<(), wiiu_memory::MemoryError> {
        self.frame_done = false;
        self.sound_tick(memory);
        self.fire_due_alarms();
        if self.callbacks.active() || self.start_callback(cpu, memory)? {
            return Ok(());
        }
        // Blocked, or none at all after the threads were parked.
        if !self.coreinit.threads.current_is_running() {
            self.schedule(cpu, memory, coreinit::thread::Reschedule::Block);
        }
        Ok(())
    }

    /// End of a time slice: alarms, callbacks, then round-robin.
    ///
    /// # Errors
    /// Fails if guest memory for stubs is unmapped.
    pub fn end_time_slice(
        &mut self,
        cpu: &mut wiiu_cpu::CpuState,
        memory: &mut wiiu_memory::Memory,
    ) -> Result<(), wiiu_memory::MemoryError> {
        self.sound_tick(memory);
        self.fire_due_alarms();
        if self.callbacks.active() || self.start_callback(cpu, memory)? {
            return Ok(());
        }
        self.schedule(cpu, memory, coreinit::thread::Reschedule::Yield);
        Ok(())
    }
}

/// Tests against the developer's own title (`shocora.toml`); they pass without one.
#[cfg(test)]
mod rom_tests {
    use std::collections::BTreeMap;

    use wiiu_rpl::RplFile;
    use wiiu_test_roms::{report, require};

    use super::CafeOs;

    /// Intent: a fresh `CafeOs` registers the system functions (HLE) the title imports;
    /// what is missing returns 0 when called, which is where titles misbehave. The report
    /// lists the coverage per library, the to-do list for a title.
    /// Pass: at least 90 % of the `coreinit` functions the title imports are implemented
    /// (threads, memory, files: without them nothing boots).
    #[test]
    fn implements_the_title_imports() {
        let title = require!(wiiu_test_roms::title(), "no title");
        let file = RplFile::parse(&std::fs::read(wiiu_test_roms::rpx_of(&title).unwrap()).unwrap())
            .unwrap();
        let registry = CafeOs::new().registry;
        let mut libraries: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
        for (library, symbol, is_function) in file.imported_symbols().unwrap() {
            if !is_function || symbol.starts_with('.') {
                continue;
            }
            let entry = libraries.entry(library.clone()).or_default();
            entry.0 += 1;
            if registry.function_index(&library, &symbol).is_none() {
                entry.1.push(symbol);
            }
        }
        for (library, (total, missing)) in &libraries {
            let shown: Vec<&str> = missing.iter().take(6).map(String::as_str).collect();
            let more = if missing.len() > 6 { ", ..." } else { "" };
            report(
                library,
                format!(
                    "{}/{total} implemented; missing: {}{more}",
                    total - missing.len(),
                    shown.join(", ")
                ),
            );
        }
        let (total, missing) = &libraries["coreinit"];
        assert!(missing.len() * 10 <= *total, "coreinit: {} of {total} missing", missing.len());
    }
}
