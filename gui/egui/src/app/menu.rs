//! The menu bar and the picture settings it switches.
use super::*;

impl ShocoraApp {
    pub(super) fn menu(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button(tr("menu.file"), |ui| {
                if ui.button(tr("menu.open_rpx")).clicked() {
                    ui.close();
                    self.pick_and_boot(false);
                }
                if ui.button(tr("menu.open_title")).clicked() {
                    ui.close();
                    self.pick_and_boot(true);
                }
                if let Some(last) = self.config.last_title.clone() {
                    if ui
                        .button(trf("menu.reload_title", &[("title", &short_name(&last))]))
                        .clicked()
                    {
                        ui.close();
                        self.boot(last);
                    }
                }
                if ui.button(tr("menu.select_rom_folder")).clicked() {
                    ui.close();
                    self.pick_rom_folder();
                }
                if ui.button(tr("menu.reload_library")).clicked() {
                    ui.close();
                    self.reload_library();
                }
                ui.separator();
                if ui.button(tr("menu.exit")).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button(tr("menu.emulation"), |ui| {
                let running = self.status == Some(Status::Running);
                let label = if self.paused { tr("menu.resume") } else { tr("menu.pause") };
                if ui.add_enabled(running, egui::Button::new(label)).clicked() {
                    self.console.send(if self.paused {
                        ConsoleCommand::Resume
                    } else {
                        ConsoleCommand::Pause
                    });
                    ui.close();
                }
                if ui
                    .add_enabled(self.cheats_path.is_some(), egui::Button::new(tr("menu.cheats")))
                    .clicked()
                {
                    self.open_cheats();
                    ui.close();
                }
                let running = self.title.is_some();
                ui.add_enabled_ui(running, |ui| {
                    ui.menu_button(tr("menu.save_state"), |ui| {
                        for slot in 0..wiiu_emu::STATE_SLOTS {
                            if ui.button(trf("menu.state_slot", &[("slot", &slot)])).clicked() {
                                self.console.send(ConsoleCommand::SaveState(slot));
                                ui.close();
                            }
                        }
                    });
                    ui.menu_button(tr("menu.load_state"), |ui| {
                        for slot in 0..wiiu_emu::STATE_SLOTS {
                            if ui.button(trf("menu.state_slot", &[("slot", &slot)])).clicked() {
                                self.console.send(ConsoleCommand::LoadState(slot));
                                ui.close();
                            }
                        }
                    });
                });
                if ui
                    .add_enabled(self.title.is_some(), egui::Button::new(tr("menu.reset")))
                    .clicked()
                {
                    self.log.push(tr("log.reset").to_owned());
                    self.console.send(ConsoleCommand::Reset);
                    ui.close();
                }
                if ui
                    .add_enabled(self.title.is_some(), egui::Button::new(tr("menu.stop")))
                    .clicked()
                {
                    self.console.send(ConsoleCommand::Stop);
                    ui.close();
                }
            });
            ui.menu_button(tr("menu.settings"), |ui| {
                for tab in SettingsTab::ALL {
                    if ui.button(tab.menu_label()).clicked() {
                        ui.close();
                        self.settings_tab = Some(tab);
                    }
                }
            });
            ui.menu_button(tr("menu.view"), |ui| {
                ui.checkbox(&mut self.show_library, tr("menu.game_library"));
                ui.checkbox(&mut self.show_log, tr("menu.guest_log"));
                ui.separator();
                if ui.checkbox(&mut self.config.gamepad_window, tr("menu.gamepad_window")).changed()
                {
                    self.save_config();
                }
                let mut fullscreen = is_fullscreen(ui.ctx());
                if ui
                    .checkbox(&mut fullscreen, tr("menu.fullscreen"))
                    .on_hover_text(tr("menu.fullscreen_hint"))
                    .changed()
                {
                    ui.close();
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(fullscreen));
                }
            });
        });
    }
}

impl ShocoraApp {
    pub(super) fn frame_rate_menu(&mut self, ui: &mut egui::Ui) {
        ui.label(tr("graphics.frame_rate"));
        let current = self.frame_rate();
        let title = ui.radio(current == FrameRate::Title, tr("graphics.as_title"));
        let capped = ui
            .radio(current == FrameRate::Max30, tr("graphics.max30"))
            .on_hover_text(tr("graphics.max30_hint"));
        let forced = ui
            .radio(current == FrameRate::Force60, tr("graphics.force60"))
            .on_hover_text(tr("graphics.force60_hint"));
        let chosen = if title.clicked() {
            FrameRate::Title
        } else if capped.clicked() {
            FrameRate::Max30
        } else if forced.clicked() {
            FrameRate::Force60
        } else {
            current
        };
        if chosen != current {
            self.set_frame_rate(chosen);
        }
    }

    pub(super) fn frame_rate(&self) -> FrameRate {
        if self.config.swap_interval.is_some() {
            FrameRate::Force60
        } else if self.config.min_swap_interval.is_some() {
            FrameRate::Max30
        } else {
            FrameRate::Title
        }
    }

    fn set_frame_rate(&mut self, rate: FrameRate) {
        let (forced, least) = match rate {
            FrameRate::Title => (None, None),
            FrameRate::Max30 => (None, Some(2)),
            FrameRate::Force60 => (Some(1), None),
        };
        self.config.swap_interval = forced;
        self.config.min_swap_interval = least;
        self.console.send(ConsoleCommand::SwapInterval(forced));
        self.console.send(ConsoleCommand::MinSwapInterval(least));
        self.save_config();
    }

    pub(super) fn upscale_menu(&mut self, ui: &mut egui::Ui) {
        let usable = upscale::available() && self.config.resolution_factor == 1;
        ui.add_enabled_ui(usable, |ui| {
            ui.label(tr("graphics.xbrz")).on_disabled_hover_text(tr("graphics.xbrz_unavailable"));
            let mut changed = ui
                .radio(self.config.upscale.method == UpscaleMethod::None, tr("graphics.off"))
                .clicked();
            if changed {
                self.config.upscale.method = UpscaleMethod::None;
            }
            for factor in 2..=upscale::MAX_FACTOR {
                let selected = self.config.upscale.method == UpscaleMethod::Xbrz
                    && self.config.upscale.factor == factor;
                if ui.radio(selected, format!("{factor}x")).clicked() {
                    self.config.upscale.method = UpscaleMethod::Xbrz;
                    self.config.upscale.factor = factor;
                    changed = true;
                }
            }
            if changed {
                self.presenter.set_upscale(self.config.upscale);
                self.save_config();
            }
        });
    }

    pub(super) fn set_frame_generation(&mut self, enabled: bool) {
        self.config.frame_generation = enabled;
        self.presenter.set_frame_generation(enabled);
        self.save_config();
    }
}

impl ShocoraApp {
    /// Follows the title's frame rate or forces 60 fps, at once.
    pub(super) fn toggle_frame_rate(&mut self) {
        self.set_frame_rate(match self.frame_rate() {
            FrameRate::Title => FrameRate::Max30,
            FrameRate::Max30 => FrameRate::Force60,
            FrameRate::Force60 => FrameRate::Title,
        });
    }

    /// Off, then every factor of xBRZ in turn.
    pub(super) fn cycle_upscale(&mut self) {
        let upscale = &mut self.config.upscale;
        if upscale.method == UpscaleMethod::None {
            upscale.method = UpscaleMethod::Xbrz;
            upscale.factor = 2;
        } else if upscale.factor < upscale::MAX_FACTOR {
            upscale.factor += 1;
        } else {
            upscale.method = UpscaleMethod::None;
        }
        self.presenter.set_upscale(self.config.upscale);
        self.save_config();
    }

    /// The next graphics API, used from the next start.
    pub(super) fn cycle_graphics_api(&mut self) {
        let position = GraphicsApi::ALL.iter().position(|api| *api == self.config.graphics_api);
        let next = position.map_or(0, |index| (index + 1) % GraphicsApi::ALL.len());
        self.config.graphics_api = GraphicsApi::ALL[next];
        self.save_config();
    }
}
