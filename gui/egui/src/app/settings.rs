//! The settings window.
use wiiu_title::update::AddOnKind;

use super::*;

/// `width` x `height` drawn `factor` times larger, as far as the limit allows.
fn scaled_size(factor: u32, width: u32, height: u32) -> String {
    let factor = ResolutionScale::for_tv(factor, width).factor;
    format!("{} x {}", width * factor, height * factor)
}

/// The pages of the settings window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingsTab {
    General,
    Graphics,
    Input,
    Netplay,
}

impl SettingsTab {
    pub(super) const ALL: [Self; 4] = [Self::General, Self::Graphics, Self::Input, Self::Netplay];

    fn title(self) -> &'static str {
        match self {
            Self::General => tr("settings.general"),
            Self::Graphics => tr("settings.graphics"),
            Self::Input => tr("settings.input"),
            Self::Netplay => tr("settings.netplay"),
        }
    }

    pub(super) fn menu_label(self) -> &'static str {
        match self {
            Self::General => tr("settings.general_menu"),
            Self::Graphics => tr("settings.graphics_menu"),
            Self::Input => tr("settings.input_menu"),
            Self::Netplay => tr("settings.netplay_menu"),
        }
    }
}

impl ShocoraApp {
    pub(super) fn set_resolution_factor(&mut self, factor: u32) {
        self.config.resolution_factor = factor;
        self.console.send(ConsoleCommand::ResolutionFactor(factor));
        self.save_config();
    }

    fn resolution_settings(&mut self, ui: &mut egui::Ui) {
        ui.label(tr("graphics.resolution")).on_hover_text(tr("graphics.resolution_hint"));
        let current = self.config.resolution_factor;
        for factor in ResolutionScale::FACTORS {
            let label = if factor == 1 {
                tr("graphics.native").to_owned()
            } else {
                trf(
                    "graphics.factor",
                    &[
                        ("factor", &factor),
                        ("at720", &scaled_size(factor, 1280, 720)),
                        ("at1080", &scaled_size(factor, 1920, 1080)),
                    ],
                )
            };
            if ui.radio(current == factor, label).clicked() && current != factor {
                self.set_resolution_factor(factor);
            }
        }
    }

    fn graphics_settings(&mut self, ui: &mut egui::Ui) {
        self.resolution_settings(ui);
        ui.separator();
        self.upscale_menu(ui);
        ui.separator();
        self.frame_rate_menu(ui);
        ui.separator();
        let mut generate = self.config.frame_generation;
        if ui
            .checkbox(&mut generate, tr("graphics.frame_generation"))
            .on_hover_text(tr("graphics.frame_generation_hint"))
            .changed()
        {
            self.set_frame_generation(generate);
        }
        let mut background = self.config.background_shaders;
        if ui
            .checkbox(&mut background, tr("graphics.background_shaders"))
            .on_hover_text(tr("graphics.background_shaders_hint"))
            .changed()
        {
            self.config.background_shaders = background;
            self.presenter.set_background_shaders(background);
            self.save_config();
        }
        if ui
            .checkbox(&mut self.config.gpu_thread, tr("graphics.gpu_thread"))
            .on_hover_text(tr("graphics.gpu_thread_hint"))
            .changed()
        {
            self.save_config();
        }
        if ui
            .checkbox(&mut self.config.multi_core, tr("graphics.multi_core"))
            .on_hover_text(tr("graphics.multi_core_hint"))
            .changed()
        {
            self.save_config();
        }
        ui.separator();
        ui.label(tr("graphics.api"));
        for api in GraphicsApi::ALL {
            if ui.radio(self.config.graphics_api == api, api.label()).clicked() {
                self.config.graphics_api = api;
                self.save_config();
            }
        }
        ui.separator();
        ui.label(tr("graphics.gpu")).on_hover_text(tr("graphics.gpu_hint"));
        let mut chosen = self.config.gpu.clone();
        ui.radio_value(&mut chosen, None, tr("graphics.gpu_auto"));
        for name in &self.gpus {
            let label = if *name == self.active_gpu {
                trf("graphics.gpu_active", &[("gpu", name)])
            } else {
                name.clone()
            };
            ui.radio_value(&mut chosen, Some(name.clone()), label);
        }
        if chosen != self.config.gpu {
            self.config.gpu = chosen;
            self.save_config();
        }
    }

    fn general_settings(&mut self, ui: &mut egui::Ui) {
        ui.label(tr("general.language"));
        ui.horizontal(|ui| {
            for (code, name) in lang::languages() {
                let chosen = self.config.language == code;
                if ui.radio(chosen, name).clicked() && !chosen {
                    lang::set_language(&code);
                    self.config.language = code;
                    self.save_config();
                }
            }
        });
        ui.separator();
        ui.label(tr("general.speed")).on_hover_text(tr("general.speed_hint"));
        ui.horizontal(|ui| {
            for speed in GameSpeed::ALL {
                if ui.radio(self.speed == speed, speed.label()).clicked() && self.speed != speed {
                    self.speed = speed;
                    self.console.send(ConsoleCommand::Speed(speed));
                }
            }
        });
        ui.separator();
        if ui.button(tr("general.fonts_folder")).clicked() {
            self.pick_fonts_dir();
        }
        if ui.button(tr("general.rom_folder")).clicked() {
            self.pick_rom_folder();
        }
        ui.separator();
        self.add_on_dirs(ui, AddOnKind::Update);
        ui.separator();
        self.add_on_dirs(ui, AddOnKind::Dlc);
    }

    /// The folders searched for updates or downloadable content: listed, added, removed.
    fn add_on_dirs(&mut self, ui: &mut egui::Ui, kind: AddOnKind) {
        let (label, hint) = match kind {
            AddOnKind::Update => (tr("general.update_folders"), tr("general.update_folders_hint")),
            AddOnKind::Dlc => (tr("general.dlc_folders"), tr("general.dlc_folders_hint")),
        };
        ui.label(label).on_hover_text(hint);
        let dirs = match kind {
            AddOnKind::Update => &self.config.update_dirs,
            AddOnKind::Dlc => &self.config.dlc_dirs,
        };
        let mut removed = None;
        for (index, dir) in dirs.iter().enumerate() {
            ui.horizontal(|ui| {
                if ui.small_button("✖").on_hover_text(tr("general.remove_folder")).clicked() {
                    removed = Some(index);
                }
                ui.label(dir.display().to_string());
            });
        }
        let added = ui
            .button(tr("general.add_folder"))
            .clicked()
            .then(|| rfd::FileDialog::new().set_title(label).pick_folder())
            .flatten();
        if removed.is_none() && added.is_none() {
            return;
        }
        let dirs = match kind {
            AddOnKind::Update => &mut self.config.update_dirs,
            AddOnKind::Dlc => &mut self.config.dlc_dirs,
        };
        if let Some(index) = removed {
            dirs.remove(index);
        }
        if let Some(dir) = added.filter(|dir| !dirs.contains(dir)) {
            dirs.push(dir);
        }
        let config = self.config.emulator_config();
        self.console.send(ConsoleCommand::AddOnDirs {
            update_dirs: config.update_dirs,
            dlc_dirs: config.dlc_dirs,
        });
        self.save_config();
        self.log.push(tr("log.add_on_folders").to_owned());
    }

    fn input_settings(&self, ui: &mut egui::Ui) {
        ui.label(tr("input.speed_keys"));
        ui.label(tr("input.edit_hint"));
        ui.separator();
        egui::Grid::new("bindings").striped(true).show(ui, |ui| {
            ui.strong(tr("input.action"));
            ui.strong(tr("input.keyboard"));
            ui.strong(tr("input.controller"));
            ui.end_row();
            for (action, key) in &self.config.key_bindings.0 {
                ui.label(action);
                ui.label(key);
                ui.label(self.config.pad_bindings.0.get(action).map_or("-", String::as_str));
                ui.end_row();
            }
        });
    }

    /// The settings window, with one tab per subject.
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(mut tab) = self.settings_tab else { return };
        let mut open = true;
        egui::Window::new(tr("menu.settings")).open(&mut open).resizable(true).show(ctx, |ui| {
            ui.horizontal(|ui| {
                for choice in SettingsTab::ALL {
                    ui.selectable_value(&mut tab, choice, choice.title());
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| match tab {
                SettingsTab::General => self.general_settings(ui),
                SettingsTab::Graphics => self.graphics_settings(ui),
                SettingsTab::Input => self.input_settings(ui),
                SettingsTab::Netplay => self.netplay_settings(ui),
            });
        });
        self.settings_tab = open.then_some(tab);
    }
}

impl ShocoraApp {
    /// Asks for the folder with `CafeStd.ttf`; titles booted from now on use it.
    fn pick_fonts_dir(&mut self) {
        let dialog = rfd::FileDialog::new().set_title(tr("dialog.fonts_folder"));
        if let Some(dir) = dialog.pick_folder() {
            self.log.push(trf("log.fonts", &[("path", &dir.display())]));
            self.console.send(ConsoleCommand::FontsDir(Some(dir.clone())));
            self.config.fonts_dir = Some(dir);
            self.save_config();
        }
    }
}
