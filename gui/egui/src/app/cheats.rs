//! The cheat window: a list of the title's codes with their switches, and below it an editor
//! for the chosen one (name, description, code lines). Only the chosen code's lines are laid
//! out, so a long cheat file costs nothing while the window is open.
//!
//! The window can be resized. Every part takes a share of the room the window has and asks
//! for no more: content that asked for "all the room there is" inside an auto-sized window
//! made it grow on every frame. Changes (switches, saved or deleted codes) stay in the window
//! until Apply or OK writes them; Cancel and the close button drop them.
use wiiu_emu::cheat::{self, Cheat, CheatParseError};

use super::*;

/// The window's size when it opens.
const DEFAULT_SIZE: egui::Vec2 = egui::vec2(420.0, 560.0);
/// The smallest size it can be made.
const MIN_SIZE: egui::Vec2 = egui::vec2(320.0, 420.0);
/// Share of the free height that goes to the list; the rest to description and code.
const LIST_SHARE: f32 = 0.4;

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
    /// The list differs from the file.
    pending: bool,
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
    Ok,
    Cancel,
    Apply,
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
        // A new code is on once added: that is why it was typed in.
        let enabled = self.selected.and_then(|i| self.cheats.get(i)).is_none_or(|c| c.enabled);
        let notes = self.notes.lines().map(str::to_owned).collect();
        Ok(Cheat { name: name.to_owned(), notes, enabled, writes })
    }

    /// Puts the editor's fields into the list: over the chosen code, or as a new one.
    fn save(&mut self) -> Result<(), String> {
        let edited = self.edited()?;
        if let Some(slot) = self.selected.and_then(|i| self.cheats.get_mut(i)) {
            *slot = edited;
        } else {
            self.cheats.push(edited);
            self.selected = Some(self.cheats.len() - 1);
        }
        self.dirty = false;
        self.pending = true;
        self.message.clear();
        Ok(())
    }

    /// Something typed in the editor that the list does not hold yet.
    fn unsaved(&self) -> bool {
        self.dirty || (self.selected.is_none() && !self.name.trim().is_empty())
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

    /// Saves what the editor holds, writes the list and has the emulator use it from the
    /// next frame on. False if something stopped it (the message says what).
    fn store_cheats(&mut self) -> bool {
        let Some(editor) = &mut self.cheats else { return false };
        if editor.unsaved()
            && let Err(message) = editor.save()
        {
            editor.message = message;
            return false;
        }
        let Some(path) = &self.cheats_path else { return false };
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(path, cheat::to_text(&editor.cheats)));
        match written {
            Ok(()) => {
                self.console.send(ConsoleCommand::ReloadCheats);
                let count = editor.cheats.iter().filter(|c| c.enabled).count();
                editor.pending = false;
                editor.message = trf("cheats.applied", &[("count", &count)]);
                true
            }
            Err(error) => {
                editor.message = format!("{}: {error}", path.display());
                false
            }
        }
    }

    fn cheat_action(&mut self, action: CheatAction) {
        let Some(editor) = &mut self.cheats else { return };
        match action {
            CheatAction::Select(index) => editor.load(index),
            CheatAction::Add => editor.clear(),
            CheatAction::Toggle(index) => {
                if let Some(chosen) = editor.cheats.get_mut(index) {
                    chosen.enabled = !chosen.enabled;
                    editor.pending = true;
                }
            }
            CheatAction::Save => {
                if let Err(message) = editor.save() {
                    editor.message = message;
                }
            }
            CheatAction::Delete => {
                let Some(index) = editor.selected else { return };
                editor.cheats.remove(index);
                editor.pending = true;
                editor.clear();
                if !editor.cheats.is_empty() {
                    editor.load(index.min(editor.cheats.len() - 1));
                }
            }
            CheatAction::Apply => {
                self.store_cheats();
            }
            CheatAction::Ok => {
                let read_only = editor.read_only;
                if read_only || self.store_cheats() {
                    self.cheats = None;
                }
            }
            CheatAction::Cancel => self.cheats = None,
        }
    }

    pub(super) fn cheat_window(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.cheats else { return };
        let mut open = true;
        let mut action = None;
        egui::Window::new(tr("cheats.title"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size(DEFAULT_SIZE)
            .min_size(MIN_SIZE)
            .show(ctx, |ui| cheat_contents(ui, editor, &mut action));
        if !open {
            action = Some(CheatAction::Cancel);
        }
        if let Some(action) = action {
            self.cheat_action(action);
        }
    }
}

fn button(ui: &mut egui::Ui, enabled: bool, text: &str) -> bool {
    ui.add_enabled(enabled, egui::Button::new(text)).clicked()
}

fn cheat_contents(ui: &mut egui::Ui, editor: &mut CheatEditor, action: &mut Option<CheatAction>) {
    let writable = !editor.read_only;
    let selected = editor.selected.is_some();
    ui.horizontal(|ui| {
        ui.label(tr("cheats.available"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if button(ui, writable && selected, tr("cheats.delete")) {
                *action = Some(CheatAction::Delete);
            }
            if button(ui, writable && editor.unsaved(), tr("cheats.save")) {
                *action = Some(CheatAction::Save);
            }
            if button(ui, writable, tr("cheats.add")) {
                *action = Some(CheatAction::Add);
            }
        });
    });
    // What is left once the fixed rows (buttons, name, labels, bottom bar) are counted.
    let row = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
    let free = row.mul_add(-7.0, ui.available_height()).max(120.0);
    let list_height = free * LIST_SHARE;
    let text_height = free * (1.0 - LIST_SHARE) / 2.0;
    egui::Frame::group(ui.style()).inner_margin(2.0).show(ui, |ui| {
        cheat_list(ui, editor, writable, list_height, action);
    });
    cheat_fields(ui, editor, text_height);
    ui.separator();
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let changes = editor.pending || editor.unsaved();
            if button(ui, writable && changes, tr("cheats.apply")) {
                *action = Some(CheatAction::Apply);
            }
            if button(ui, true, tr("cheats.cancel")) {
                *action = Some(CheatAction::Cancel);
            }
            if button(ui, true, tr("cheats.ok")) {
                *action = Some(CheatAction::Ok);
            }
            ui.add(egui::Label::new(&editor.message).truncate());
        });
    });
}

/// The codes, one row each: switch, name, type. A grid lays the rows out one under the other.
fn cheat_list(
    ui: &mut egui::Ui,
    editor: &CheatEditor,
    writable: bool,
    height: f32,
    action: &mut Option<CheatAction>,
) {
    // The default check mark is dark on a dark box; a white one is easy to see.
    let widgets = &mut ui.visuals_mut().widgets;
    for widget in [&mut widgets.inactive, &mut widgets.hovered, &mut widgets.active] {
        widget.fg_stroke.color = egui::Color32::WHITE;
    }
    let name_width = (ui.available_width() - 140.0).max(80.0);
    egui::ScrollArea::vertical()
        .id_salt("cheat_list")
        .auto_shrink(false)
        .max_height(height)
        .min_scrolled_height(height)
        .show(ui, |ui| {
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
                    let name = egui::Button::selectable(chosen, &code.name).truncate();
                    let size = [name_width, ui.spacing().interact_size.y];
                    if ui.add_sized(size, name).clicked() && !chosen {
                        *action = Some(CheatAction::Select(index));
                    }
                    ui.label(tr("cheats.type_ram"));
                    ui.end_row();
                }
            });
        });
}

/// Name, description and code lines of the code in the editor, one under the other.
fn cheat_fields(ui: &mut egui::Ui, editor: &mut CheatEditor, text_height: f32) {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(tr("cheats.name"));
        let rest = ui.available_width();
        changed |=
            ui.add(egui::TextEdit::singleline(&mut editor.name).desired_width(rest)).changed();
    });
    ui.label(tr("cheats.notes"));
    changed |= text_box(ui, "cheat_notes", &mut editor.notes, text_height, None);
    ui.label(tr("cheats.code")).on_hover_text(tr("cheats.format"));
    changed |= text_box(ui, "cheat_code", &mut editor.code, text_height, Some(tr("cheats.hint")));
    editor.dirty |= changed;
}

/// A multi-line field of the given height that scrolls what does not fit.
fn text_box(
    ui: &mut egui::Ui,
    id: &str,
    text: &mut String,
    height: f32,
    hint: Option<&str>,
) -> bool {
    let mut changed = false;
    egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink(false)
        .max_height(height)
        .min_scrolled_height(height)
        .show(ui, |ui| {
            let line = ui.text_style_height(&egui::TextStyle::Monospace);
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let rows = ((height - 8.0) / line).floor().max(1.0) as usize;
            let mut edit = egui::TextEdit::multiline(text)
                .code_editor()
                .desired_rows(rows)
                .desired_width(ui.available_width());
            if let Some(hint) = hint {
                edit = edit.hint_text(hint);
            }
            changed = ui.add(edit).changed();
        });
    changed
}
