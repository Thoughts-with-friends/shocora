//! The start screen and the game library: finding titles and booting them.
use super::*;

impl ShocoraApp {
    /// The directories of a `roms.json` from before `shocora.toml` listed them.
    pub(super) fn adopt_old_rom_list(&mut self) {
        if self.config.rom_dirs.is_empty() {
            if let Some(dirs) = legacy_rom_dirs(Path::new(LEGACY_PATH)) {
                self.config.rom_dirs = dirs;
                self.save_config();
            }
        }
    }

    /// Re-reads the directories from `shocora.toml`, which may have been edited, and rescans.
    pub(super) fn reload_library(&mut self) {
        match AppConfig::read(&self.config_path) {
            Ok(saved) => {
                // Nothing saved yet: what is known so far must not get lost.
                if self.config_path.exists() {
                    self.config.rom_dirs = saved.rom_dirs;
                }
                self.library = GameLibrary::scan(&self.config.rom_dirs);
                self.library_error = None;
                self.log.extend(self.library.problems.iter().cloned());
                self.log.push(trf("library.count", &[("count", &self.library.games.len())]));
            }
            Err(error) => {
                self.library = GameLibrary::default();
                self.library_error = Some(error);
            }
        }
    }

    /// Asks for a directory of titles, adds it to `shocora.toml` and rescans.
    pub(super) fn pick_rom_folder(&mut self) {
        let Some(dir) = rfd::FileDialog::new().set_title(tr("dialog.rom_folder")).pick_folder()
        else {
            return;
        };

        // Never overwrite a file that only failed to parse: it holds the user's list.
        if let Err(error) = AppConfig::read(&self.config_path) {
            self.log.push(error.clone());
            self.library_error = Some(error);
            return;
        }

        if !self.config.rom_dirs.contains(&dir) {
            self.config.rom_dirs.push(dir);
        }

        self.save_config();
        self.reload_library();
    }
}

impl ShocoraApp {
    pub(super) fn pick_and_boot(&mut self, folder: bool) {
        let dialog = rfd::FileDialog::new();
        let picked = if folder {
            dialog.set_title(tr("dialog.title_folder")).pick_folder()
        } else {
            dialog
                .set_title(tr("dialog.rpx"))
                .add_filter(tr("dialog.rpx_filter"), &["rpx"])
                .pick_file()
        };
        if let Some(path) = picked {
            self.boot(path);
        }
    }
}

impl ShocoraApp {
    /// Titles from `shocora.toml`'s directories; double-click or tr("library.boot") starts one.
    pub(super) fn library_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(tr("library.games"));
            if ui.small_button("⟳").on_hover_text(tr("library.rescan")).clicked() {
                self.reload_library();
            }
        });
        ui.separator();

        if let Some(error) = &self.library_error {
            ui.colored_label(egui::Color32::YELLOW, error);
            return;
        }

        if self.library.games.is_empty() {
            ui.label(tr("library.none"));
            return;
        }

        let mut boot = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for game in &self.library.games {
                let selected = self.title.as_deref() == Some(game.name.as_str());
                let response = ui.selectable_label(selected, &game.name);
                let response = match game.title_id {
                    Some(id) => {
                        response.on_hover_text(format!("{id:016X}\n{}", game.rpx_path.display()))
                    }
                    None => response.on_hover_text(game.rpx_path.display().to_string()),
                };

                if response.double_clicked() {
                    boot = Some(game.path.clone());
                }

                response.context_menu(|ui| {
                    if ui.button(tr("library.boot")).clicked() {
                        boot = Some(game.path.clone());
                        ui.close();
                    }
                });
            }
        });

        if let Some(path) = boot {
            self.boot(path);
        }
    }

    /// The console-like start screen; tells what is wrong if it has no titles to offer.
    pub(super) fn menu_screen(&mut self, ui: &mut egui::Ui) {
        let area = ui.available_rect_before_wrap();
        let [r, g, b, a] = self.config.launcher_background;
        let background = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
        let picture = self.config.launcher_image.clone();

        if let Some(path) =
            self.launcher.show(ui, &self.library.games, background, picture.as_deref())
        {
            self.boot(path);
        }

        let notice = self
            .library_error
            .clone()
            .or_else(|| self.library.games.is_empty().then(|| tr("library.none").to_owned()));

        let button = egui::Rect::from_min_size(
            area.left_top() + egui::vec2(16.0, 16.0),
            egui::vec2(150.0, 32.0),
        );

        if ui.put(button, egui::Button::new(tr("launcher.select_rom"))).clicked() {
            self.pick_rom_folder();
        }

        let picture_rect = egui::Rect::from_min_size(
            button.left_bottom() + egui::vec2(0.0, 8.0 + 32.0 + 8.0),
            egui::vec2(230.0, 26.0),
        );

        let picker =
            egui::Rect::from_min_size(button.left_bottom() + egui::vec2(0.0, 8.0), button.size());

        let mut color = background;
        let changed = ui
            .scope_builder(egui::UiBuilder::new().max_rect(picker), |ui| {
                ui.horizontal(|ui| {
                    let response = egui::color_picker::color_edit_button_srgba(
                        ui,
                        &mut color,
                        egui::color_picker::Alpha::OnlyBlend,
                    )
                    .on_hover_text(tr("launcher.background_color_hint"));

                    egui::Frame::new()
                        .fill(egui::Color32::from_black_alpha(170))
                        .corner_radius(4.0)
                        .inner_margin(egui::vec2(6.0, 2.0))
                        .show(ui, |ui| {
                            ui.colored_label(egui::Color32::WHITE, tr("launcher.background_color"))
                        });

                    response.changed()
                })
                .inner
            })
            .inner;

        if changed {
            // Not `r()` and so on: those are premultiplied, and would darken at every change.
            self.config.launcher_background = color.to_srgba_unmultiplied();
            self.save_config();
        }
        self.picture_controls(ui, picture_rect);

        if let Some(error) = self.launcher.picture_error() {
            ui.painter().text(
                picture_rect.left_bottom() + egui::vec2(0.0, 6.0),
                egui::Align2::LEFT_TOP,
                error,
                egui::FontId::proportional(12.0),
                egui::Color32::from_rgb(200, 60, 40),
            );
        }

        if let Some(notice) = notice {
            ui.painter().text(
                area.center_top() + egui::vec2(0.0, 12.0),
                egui::Align2::CENTER_TOP,
                notice,
                egui::FontId::proportional(14.0),
                egui::Color32::from_rgb(170, 90, 30),
            );
        }
    }

    /// Path of the backdrop picture: typed in, or chosen with the button next to it.
    fn picture_controls(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let mut chosen = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.horizontal(|ui| {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.picture_path)
                        .hint_text(tr("launcher.picture_path"))
                        .desired_width(rect.width() - 36.0),
                );

                if field.lost_focus() {
                    chosen = Some(self.picture_path.clone());
                }

                if ui.button("…").on_hover_text(tr("launcher.pick_picture")).clicked() {
                    let dialog =
                        rfd::FileDialog::new().set_title(tr("launcher.picture")).add_filter(
                            tr("launcher.pictures"),
                            &["png", "jpg", "jpeg", "webp", "bmp", "gif", "tga"],
                        );
                    if let Some(path) = dialog.pick_file() {
                        chosen = Some(path.display().to_string());
                    }
                }
            });
        });

        if let Some(text) = chosen {
            self.set_picture(&text);
        }
    }

    /// Applies a backdrop picture path; empty text removes the picture.
    fn set_picture(&mut self, text: &str) {
        // Explorer's "copy as path" adds quotes.
        let text = text.trim().trim_matches('"');
        self.picture_path = text.to_owned();

        let path = (!text.is_empty()).then(|| PathBuf::from(text));
        if path == self.config.launcher_image {
            return;
        }

        // An opaque color would hide the picture just chosen.
        if path.is_some() && self.config.launcher_background[3] == 255 {
            self.config.launcher_background[3] = 0;
        }

        self.config.launcher_image = path;
        self.save_config();
    }
}
