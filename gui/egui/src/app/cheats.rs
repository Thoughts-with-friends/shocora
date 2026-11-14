//! The cheat window: a list of the title's codes with their switches, and an editor for the
//! chosen one. Only the chosen code's lines are laid out, so a long cheat file costs nothing
//! while the window is open.
use wiiu_emu::cheat::{self, Cheat, CheatParseError};

use super::*;

/// The cheat window's state.
#[derive(Debug, Default)]
pub(super) struct CheatEditor {
    cheats: Vec<Cheat>,
    /// The code in the editor; `None` while a new one is typed.
    selected: Option<usize>,
    name: String,
    notes: String,
    code: String,
    /// The editor holds changes not saved yet.
    dirty: bool,
    /// The file could not be read: nothing is written over it.
    read_only: bool,
    /// What is wrong, or what the last action did.
    message: String,
}

/// What the user asked for this frame.
enum CheatAction {
    Toggle(usize),
    Select(usize),
    Add,
    Save,
    Delete,
}

fn parse_error_text(error: &CheatParseError) -> String {
    match error {
        CheatParseError::InvalidFormat(line) => trf("cheats.invalid_format", &[("line", line)]),
        CheatParseError::InvalidHex(line, token) => {
            trf("cheats.invalid_hex", &[("line", line), ("token", token)])
        }
    }
}

impl CheatEditor {
    fn load(&mut self, index: usize) {
        let Some(chosen) = self.cheats.get(index) else { return };
        self.selected = Some(index);
        self.name.clone_from(&chosen.name);
        self.notes = chosen.notes.join("\n");
        self.code = cheat::format_writes(&chosen.writes);
        self.dirty = false;
    }

    fn clear(&mut self) {
        self.selected = None;
        self.name.clear();
        self.notes.clear();
        self.code.clear();
        self.dirty = false;
    }

    /// The editor's fields as a code, or why they are not one.
    fn edited(&self) -> Result<Cheat, String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(tr("cheats.name_required").to_owned());
        }
        let writes = cheat::parse_writes(&self.code).map_err(|error| parse_error_text(&error))?;
        let enabled = self.selected.and_then(|i| self.cheats.get(i)).is_some_and(|c| c.enabled);
        let notes = self.notes.lines().map(str::to_owned).collect();
        Ok(Cheat { name: name.to_owned(), notes, enabled, writes })
    }
}

impl ShocoraApp {
    /// Opens the cheat window on the running title's file.
    pub(super) fn open_cheats(&mut self) {
        let text = self
            .cheats_path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        let mut editor = CheatEditor::default();
        match cheat::parse(&text) {
            Ok(cheats) => editor.cheats = cheats,
            Err(error) => {
                editor.message = parse_error_text(&error);
                editor.read_only = true;
            }
        }
        if !editor.cheats.is_empty() {
            editor.load(0);
        }
        self.cheats = Some(editor);
    }

    /// Writes the list and has the emulator use it from the next frame on.
    fn store_cheats(&mut self) {
        let (Some(editor), Some(path)) = (&mut self.cheats, &self.cheats_path) else { return };
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(path, cheat::to_text(&editor.cheats)));
        editor.message = match written {
            Ok(()) => {
                self.console.send(ConsoleCommand::ReloadCheats);
                let count = editor.cheats.iter().filter(|c| c.enabled).count();
                trf("cheats.applied", &[("count", &count)])
            }
            Err(error) => format!("{}: {error}", path.display()),
        };
    }

    fn cheat_action(&mut self, action: CheatAction) {
        let Some(editor) = &mut self.cheats else { return };
        match action {
            CheatAction::Select(index) => {
                editor.load(index);
                return;
            }
            CheatAction::Add => {
                editor.clear();
                return;
            }
            CheatAction::Toggle(index) => {
                if let Some(chosen) = editor.cheats.get_mut(index) {
                    chosen.enabled = !chosen.enabled;
                }
            }
            CheatAction::Save => match editor.edited() {
                Ok(edited) => {
                    if let Some(slot) = editor.selected.and_then(|i| editor.cheats.get_mut(i)) {
                        *slot = edited;
                    } else {
                        editor.cheats.push(edited);
                        editor.selected = Some(editor.cheats.len() - 1);
                    }
                    editor.dirty = false;
                }
                Err(message) => {
                    editor.message = message;
                    return;
                }
            },
            CheatAction::Delete => {
                let Some(index) = editor.selected else { return };
                editor.cheats.remove(index);
                editor.clear();
                if !editor.cheats.is_empty() {
                    editor.load(index.min(editor.cheats.len() - 1));
                }
            }
        }
        self.store_cheats();
    }

    pub(super) fn cheat_window(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.cheats else { return };
        let mut open = true;
        let mut action = None;
        egui::Window::new(tr("cheats.title")).open(&mut open).default_size([760.0, 460.0]).show(
            ctx,
            |ui| {
                let writable = !editor.read_only;
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(writable, egui::Button::new(tr("cheats.add"))).clicked() {
                        action = Some(CheatAction::Add);
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let selected = editor.selected.is_some();
                    if ui
                        .add_enabled(writable && selected, egui::Button::new(tr("cheats.delete")))
                        .clicked()
                    {
                        action = Some(CheatAction::Delete);
                    }
                    let can_save = writable && (editor.dirty || !selected);
                    if ui.add_enabled(can_save, egui::Button::new(tr("cheats.save"))).clicked() {
                        action = Some(CheatAction::Save);
                    }
                });
                ui.columns(2, |columns| {
                    cheat_list(&mut columns[0], editor, writable, &mut action);
                    cheat_fields(&mut columns[1], editor);
                });
                ui.separator();
                ui.label(&editor.message);
            },
        );
        if let Some(action) = action {
            self.cheat_action(action);
        }
        if !open {
            self.cheats = None;
        }
    }
}

/// The codes, each with its switch, name and type.
fn cheat_list(
    ui: &mut egui::Ui,
    editor: &CheatEditor,
    writable: bool,
    action: &mut Option<CheatAction>,
) {
    ui.label(tr("cheats.available"));
    egui::Frame::group(ui.style()).show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt("cheat_list").max_height(320.0).show(ui, |ui| {
            egui::Grid::new("cheat_grid").striped(true).num_columns(3).show(ui, |ui| {
                ui.label("");
                ui.strong(tr("cheats.column_name"));
                ui.strong(tr("cheats.column_type"));
                ui.end_row();
                for (index, code) in editor.cheats.iter().enumerate() {
                    let mut enabled = code.enabled;
                    if ui
                        .add_enabled(writable, egui::Checkbox::without_text(&mut enabled))
                        .changed()
                    {
                        *action = Some(CheatAction::Toggle(index));
                    }
                    let chosen = editor.selected == Some(index);
                    if ui.selectable_label(chosen, &code.name).clicked() && !chosen {
                        *action = Some(CheatAction::Select(index));
                    }
                    ui.label(tr("cheats.type_ram"));
                    ui.end_row();
                }
            });
            ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
        });
    });
}

/// Name, notes and code lines of the code in the editor.
fn cheat_fields(ui: &mut egui::Ui, editor: &mut CheatEditor) {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(tr("cheats.name"));
        changed |= ui
            .add(egui::TextEdit::singleline(&mut editor.name).desired_width(f32::INFINITY))
            .changed();
    });
    ui.label(tr("cheats.notes"));
    changed |= ui
        .add(
            egui::TextEdit::multiline(&mut editor.notes)
                .desired_rows(5)
                .desired_width(f32::INFINITY),
        )
        .changed();
    ui.label(tr("cheats.code")).on_hover_text(tr("cheats.format"));
    egui::ScrollArea::vertical().id_salt("cheat_code").max_height(200.0).show(ui, |ui| {
        changed |= ui
            .add(
                egui::TextEdit::multiline(&mut editor.code)
                    .code_editor()
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .hint_text(tr("cheats.hint")),
            )
            .changed();
    });
    editor.dirty |= changed;
}
