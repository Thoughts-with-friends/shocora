//! Main window: menu, the two screens, status bar and guest log.
use std::path::{Path, PathBuf};

use eframe::egui;
use shocora_game_library::{GameLibrary, LEGACY_PATH, legacy_rom_dirs};
use shocora_gui_common::{
    config::AppConfig,
    console::{Console, ConsoleCommand},
    input::{ACTIONS, Controllers, KeyBindings, PadBindings, merge},
};
use shocora_netplay::{
    DEFAULT_PORT, FrameRate, HostConfig, HostState, HostStatus, LinkState, Refusal,
};
use shocora_render::{GraphicsApi, ResolutionScale, Screen, UpscaleMethod, upscale};
use wiiu_emu::{GamePadState, GameSpeed, Status, VpadButtons};

use crate::{
    lang::{self, tr, trf},
    launcher::Launcher,
    netplay::{self, Netplay},
    presenter::{Presenter, ScreenView},
};

mod cheats;
mod input;
mod library;
mod menu;
mod motion;
mod netplay_ui;
mod screens;
mod settings;
mod status_bar;
use cheats::*;
use motion::MotionInput;
use screens::*;
use settings::*;
use status_bar::*;
/// Lines of guest output kept in the log panel.
const LOG_LINES: usize = 500;

pub struct ShocoraApp {
    /// Comes first, to be dropped before the console it sends commands to.
    netplay: Netplay,
    /// What went wrong when starting to host.
    netplay_error: Option<String>,
    /// Asked for each time: passwords are not written to the settings file.
    netplay_password: String,
    /// The network being hosted, for the status bar.
    netplay_network: String,
    console: Console,
    presenter: Presenter,
    config: AppConfig,
    config_path: PathBuf,
    /// API eframe is actually running with (config may hold a pending change).
    active_api: GraphicsApi,
    /// GPUs that can draw to the window, by name.
    gpus: Vec<String>,
    /// The one drawing now (config may hold a pending change).
    active_gpu: String,
    status: Option<Status>,
    paused: bool,
    title: Option<String>,
    fps: f32,
    instructions: u64,
    log: Vec<String>,
    show_log: bool,
    last_gamepad: GamePadState,
    /// The settings window's page, while it is open.
    settings_tab: Option<SettingsTab>,
    /// The binding waiting for a key or a controller button, in the input settings.
    binding_capture: Option<(&'static str, BindingDevice)>,
    /// How fast the title runs. Back to normal at the next start of the program.
    speed: GameSpeed,
    /// The controller's speed button was down at the last look: a press changes it once.
    speed_button_down: bool,
    controllers: Controllers,
    /// Titles found in the directories listed in `shocora.toml`.
    library: GameLibrary,
    library_error: Option<String>,
    show_library: bool,
    /// The start screen, shown while no title runs.
    launcher: Launcher,
    /// A boot was requested and the emulator has not answered yet.
    boot_pending: bool,
    /// Where the mouse holds the GamePad's screen, in its pixels.
    touch: Option<(u16, u16)>,
    /// What the GamePad's own window adds: its touch and the keys held while it has focus.
    window_touch: Option<(u16, u16)>,
    window_keys: GamePadState,
    /// Microphone and motion sensors.
    motion: MotionInput,
    /// The cheat editor's text, while it is open.
    cheats: Option<CheatEditor>,
    /// Sound volume, shared with whatever plays sound.
    volume: shocora_gui_common::audio::Volume,
    /// Where the running title's cheat codes are kept.
    cheats_path: Option<PathBuf>,
    /// What the backdrop picture's path field shows while being edited.
    picture_path: String,
}

impl ShocoraApp {
    /// # Errors
    /// Fails if no graphics context exists or the emulator cannot start.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        config: AppConfig,
        config_path: PathBuf,
        active_api: GraphicsApi,
        gpus: Vec<String>,
        boot: Option<PathBuf>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let presenter = Presenter::new(cc)?;
        let active_gpu = cc
            .wgpu_render_state
            .as_ref()
            .map_or_else(String::new, |state| state.adapter.get_info().name);
        presenter.set_upscale(config.upscale);
        let ctx = cc.egui_ctx.clone();

        crate::fonts::setup_custom_fonts(&ctx, None::<std::path::PathBuf>);
        let render = presenter.sender().ok_or("the render thread is not running")?;
        let volume = shocora_gui_common::audio::Volume::new(f32::from(config.volume) / 100.0);
        let console =
            Console::spawn(config.emulator_config(), volume.clone(), render, move || {
                ctx.request_repaint();
            })?;
        lang::set_language(&config.language);

        let picture_path = config
            .launcher_image
            .as_ref()
            .map_or_else(String::new, |path| path.display().to_string());

        presenter.set_frame_generation(config.frame_generation);
        presenter.set_background_shaders(config.background_shaders);

        let mut app = Self {
            netplay: Netplay::Off,
            netplay_error: None,
            netplay_password: String::new(),
            netplay_network: String::new(),
            cheats: None,
            volume,
            cheats_path: None,
            picture_path,
            console,
            presenter,
            config,
            config_path,
            active_api,
            gpus,
            active_gpu,
            status: None,
            paused: false,
            title: None,
            fps: 0.0,
            instructions: 0,
            log: Vec::new(),
            show_log: true,
            last_gamepad: GamePadState::default(),
            settings_tab: None,
            binding_capture: None,
            speed: GameSpeed::Normal,
            speed_button_down: false,
            controllers: Controllers::new(),
            library: GameLibrary::default(),
            library_error: None,
            show_library: false,
            launcher: Launcher::default(),
            boot_pending: false,
            touch: None,
            window_touch: None,
            window_keys: GamePadState::default(),
            motion: MotionInput::default(),
        };

        app.adopt_old_rom_list();
        app.reload_library();

        if let Some(path) = boot {
            app.boot(path);
        }

        Ok(app)
    }

    fn boot(&mut self, path: PathBuf) {
        self.log.push(trf("log.loading", &[("path", &path.display())]));
        self.boot_pending = true;
        self.console.send(ConsoleCommand::Load(path.clone()));
        self.config.last_title = Some(path);
        self.save_config();
    }

    fn save_config(&self) {
        if let Err(error) = self.config.save(&self.config_path) {
            log::warn!("{}: {error}", self.config_path.display());
        }
    }

    fn pull_emulator_state(&mut self) {
        let snapshot = self.console.drain();
        self.presenter.sync();
        self.log.extend(snapshot.console_lines.iter().map(|l| l.trim_end().to_owned()));
        if let Some(factor) = self.presenter.take_memory_shortage() {
            // Frames already on their way may report the old factor once more.
            let lower = ResolutionScale::lower_factor(factor);
            if lower < self.config.resolution_factor {
                self.log.push(trf(
                    "graphics.memory_ran_out",
                    &[("factor", &factor), ("lower", &lower)],
                ));
                self.set_resolution_factor(lower);
            }
        }
        if snapshot.title.is_some() {
            self.boot_pending = false;
        }
        if let Some(error) = snapshot.load_error {
            self.log.push(error);
            self.boot_pending = false;
        }
        if let Some(Status::Crashed(message)) = &snapshot.status {
            if self.status != snapshot.status {
                self.log.push(trf("log.stopped", &[("message", message)]));
            }
        }
        let overflow = self.log.len().saturating_sub(LOG_LINES);
        self.log.drain(..overflow);
        self.status = snapshot.status;
        self.cheats_path = snapshot.cheats_path;
        self.paused = snapshot.paused;
        self.title = snapshot.title;
        self.fps = snapshot.fps;
        self.instructions = snapshot.stats.instructions;
        if let Netplay::Host(hosting) = &self.netplay {
            hosting.publish(self.host_status());
        }
    }

    /// What this emulator is doing, in the words of the status bars of both players.
    fn host_status(&self) -> HostStatus {
        HostStatus {
            state: match (&self.status, self.paused) {
                (Some(Status::Running), true) => HostState::Paused,
                (Some(Status::Running), false) => HostState::Running,
                (Some(Status::Exited(code)), _) => HostState::Exited(*code),
                (Some(Status::Crashed(_)), _) => HostState::Crashed,
                (Some(Status::Idle) | None, _) => HostState::Idle,
            },
            fps: self.fps,
            rate: self.frame_rate(),
            speed: self.speed.label().to_string(),
            frame_generation: self.config.frame_generation,
            xbrz: match self.config.upscale.method {
                UpscaleMethod::None => 0,
                UpscaleMethod::Xbrz => self.config.upscale.factor,
            },
            title: self.title.clone().unwrap_or_default(),
            instructions: self.instructions,
            player1: false,
        }
    }
}

fn short_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

impl eframe::App for ShocoraApp {
    /// Nothing of its own behind the panels: a see-through backdrop shows the desktop.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pull_emulator_state();
        // A guest shows the host's title, whatever runs here.
        let in_menu = self.title.is_none()
            && !self.boot_pending
            && !matches!(self.netplay, Netplay::Guest(_));
        let ctx = ui.ctx().clone();
        // Before the input is read: its window adds touches and keys.
        self.gamepad_window(&ctx, in_menu);
        self.poll_input(&ctx);
        fullscreen_hotkeys(&ctx);
        // Full screen while a title runs: nothing but its pictures.
        if is_fullscreen(&ctx) && !in_menu {
            let screen = egui::Frame::NONE.fill(egui::Color32::BLACK);
            if matches!(self.netplay, Netplay::Guest(_)) {
                egui::CentralPanel::default().frame(screen).show(ui, |ui| self.guest_screen(ui));
            } else {
                egui::CentralPanel::default().frame(screen).show(ui, |ui| self.screens(ui));
            }
            self.cheat_window(&ctx);
            self.settings_window(&ctx);
            return;
        }

        egui::Panel::top("menu").show(ui, |ui| self.menu(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.show_library {
            egui::Panel::left("library")
                .resizable(true)
                .default_size(260.0)
                .show(ui, |ui| self.library_panel(ui));
        }
        if self.show_log && !in_menu {
            egui::Panel::bottom("log")
                .resizable(true)
                .default_size(140.0)
                .show(ui, |ui| self.log_panel(ui));
        }
        if matches!(self.netplay, Netplay::Guest(_)) {
            egui::CentralPanel::default().show(ui, |ui| self.guest_screen(ui));
        } else if in_menu {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| self.menu_screen(ui));
        } else {
            egui::CentralPanel::default().show(ui, |ui| self.screens(ui));
        }
        self.cheat_window(ui.ctx());
        self.settings_window(ui.ctx());
        self.launcher.paint_transition(ui.ctx());
    }
}
