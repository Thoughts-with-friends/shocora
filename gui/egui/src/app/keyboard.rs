//! The title's software keyboard, typed on the host keyboard (Cemu's `swkbd_render`).
//!
//! While the title waits for text, a window over its picture takes it, through the
//! host's input method too (Japanese and so on). Every change goes to the title at once,
//! as typing on the GamePad does; Enter or OK confirms it.
use super::*;

impl ShocoraApp {
    pub(super) fn keyboard_window(&mut self, ctx: &egui::Context) {
        let Some(title_text) = &self.keyboard_text else {
            self.keyboard_edit = None;
            return;
        };

        let started = self.keyboard_edit.is_none();
        let edit = self.keyboard_edit.get_or_insert_with(|| title_text.clone());
        let mut changed = false;
        let mut decide = false;

        egui::Window::new(tr("keyboard.title"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let field = ui.add(
                    egui::TextEdit::singleline(edit)
                        .font(egui::TextStyle::Heading)
                        .desired_width(480.0),
                );

                if started {
                    field.request_focus();
                }

                changed = field.changed();
                decide =
                    field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));

                ui.horizontal(|ui| {
                    if ui.button(tr("keyboard.ok")).clicked() {
                        decide = true;
                    }
                    ui.label(tr("keyboard.hint"));
                });
            });

        if changed || decide {
            self.console.send(ConsoleCommand::KeyboardText { text: edit.clone(), decide });
        }
    }
}
