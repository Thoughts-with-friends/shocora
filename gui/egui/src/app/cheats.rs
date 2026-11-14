//! The cheat editor.
use wiiu_emu::cheat::CheatParseError;

use super::*;

/// The cheat editor window.
#[derive(Debug)]
pub(super) struct CheatEditor {
    text: String,
    /// What is wrong with the text, or what saving it did.
    message: String,
}

impl ShocoraApp {
    /// Opens the cheat editor on the running title's file.
    pub(super) fn open_cheats(&mut self) {
        let text = self
            .cheats_path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        self.cheats = Some(CheatEditor { text, message: String::new() });
    }

    /// Checks the editor's text; if it is fine, writes it and has the emulator use it.
    fn save_cheats(&mut self) {
        let Some(editor) = &mut self.cheats else { return };
        let count = match wiiu_emu::cheat::parse(&editor.text) {
            Ok(cheats) => cheats.iter().filter(|cheat| cheat.enabled).count(),
            Err(error) => {
                editor.message = match &error {
                    CheatParseError::InvalidFormat(line) => {
                        trf("cheats.invalid_format", &[("line", line)])
                    }
                    CheatParseError::InvalidHex(line, token) => {
                        trf("cheats.invalid_hex", &[("line", line), ("token", token)])
                    }
                };
                return;
            }
        };
        let Some(path) = &self.cheats_path else { return };
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(path, &editor.text));
        editor.message = match written {
            Ok(()) => {
                self.console.send(ConsoleCommand::ReloadCheats);
                trf("cheats.applied", &[("count", &count)])
            }
            Err(error) => format!("{}: {error}", path.display()),
        };
    }

    pub(super) fn cheat_window(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.cheats else { return };
        let (mut open, mut save) = (true, false);
        egui::Window::new(tr("cheats.title")).open(&mut open).default_size([460.0, 380.0]).show(
            ctx,
            |ui| {
                ui.label(tr("cheats.format"));
                ui.label(tr("cheats.comments"));
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut editor.text)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .hint_text(tr("cheats.hint")),
                    );
                });
                ui.horizontal(|ui| {
                    save = ui.button(tr("cheats.save")).clicked();
                    ui.label(&editor.message);
                });
            },
        );
        if save {
            self.save_cheats();
        }
        if !open {
            self.cheats = None;
        }
    }
}
