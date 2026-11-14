//! The title's pictures: the screens, the GamePad's own window, full screen and the log.
use super::*;

impl ShocoraApp {
    pub(super) fn log_panel(&self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(
            ui,
            |ui| {
                for line in &self.log {
                    ui.monospace(line);
                }
            },
        );
    }

    /// TV and GamePad side by side, scaled to fit while keeping aspect ratio.
    pub(super) fn screens(&mut self, ui: &mut egui::Ui) {
        let views = Screen::ALL.map(|screen| self.presenter.view(screen));
        let shown = [true, !self.config.gamepad_window];
        let input = show_screens(ui, views, shown);
        self.touch = input.touch;
        self.motion.tilt = input.tilt;
        if input.double_clicked {
            toggle_fullscreen(ui.ctx());
        }
    }

    /// The GamePad's screen in a window of its own, while a title runs and the user wants it.
    pub(super) fn gamepad_window(&mut self, ctx: &egui::Context, in_menu: bool) {
        self.window_touch = None;
        self.motion.window_tilt = None;
        self.window_keys = GamePadState::default();
        if !self.config.gamepad_window || in_menu {
            return;
        }
        let view = match &mut self.netplay {
            Netplay::Guest(guest) => guest.views(ctx)[1],
            _ => self.presenter.view(Screen::Drc),
        };
        let (width, height) = Screen::Drc.default_size();
        #[allow(clippy::cast_precision_loss)]
        let builder = egui::ViewportBuilder::default()
            .with_title(tr("window.gamepad_title"))
            .with_inner_size([width as f32, height as f32])
            .with_min_inner_size([width as f32 / 4.0, height as f32 / 4.0]);
        let bindings = &self.config.key_bindings;
        let (input, keys, closed) = ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("shocora gamepad"),
            builder,
            |ui, _class| {
                let ctx = ui.ctx().clone();
                let mut input = ScreenInput::default();
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
                    .show(ui, |ui| {
                        input = show_screens(ui, [None, view], [false, true]);
                        if input.double_clicked {
                            toggle_fullscreen(&ctx);
                        }
                    });
                fullscreen_hotkeys(&ctx);
                // Keys pressed while this window has the focus play, too.
                let keys = ctx.input(|input| {
                    bindings.gamepad(|name| {
                        egui::Key::from_name(name).is_some_and(|key| input.key_down(key))
                    })
                });
                let closed = ctx.input(|input| input.viewport().close_requested());
                (input, keys, closed)
            },
        );
        self.window_touch = input.touch;
        self.motion.window_tilt = input.tilt;
        self.window_keys = keys;
        if closed {
            self.config.gamepad_window = false;
            self.save_config();
        }
    }
}

/// Whether the current window covers the screen.
pub(super) fn is_fullscreen(ctx: &egui::Context) -> bool {
    ctx.input(|input| input.viewport().fullscreen.unwrap_or(false))
}

pub(super) fn toggle_fullscreen(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!is_fullscreen(ctx)));
}

/// F11 and Alt+Enter switch the current window to full screen and back; Esc leaves it.
pub(super) fn fullscreen_hotkeys(ctx: &egui::Context) {
    if ctx.egui_wants_keyboard_input() {
        return;
    }
    let (toggle, escape) = ctx.input(|input| {
        let alt_enter = input.modifiers.alt && input.key_pressed(egui::Key::Enter);
        (input.key_pressed(egui::Key::F11) || alt_enter, input.key_pressed(egui::Key::Escape))
    });
    if toggle {
        toggle_fullscreen(ctx);
    } else if escape && is_fullscreen(ctx) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
    }
}

/// What the mouse did on the screens.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ScreenInput {
    /// Where the left button touches the GamePad's screen, in its pixels.
    pub touch: Option<(u16, u16)>,
    /// Where the right button holds the GamePad's screen, `0.0..=1.0` across it: the
    /// GamePad tilts that way (Cemu's mouse motion).
    pub tilt: Option<(f32, f32)>,
    /// The TV's screen was double-clicked.
    pub double_clicked: bool,
}

/// The screens marked in `shown` side by side, scaled to fit while keeping aspect ratio.
pub(super) fn show_screens(
    ui: &mut egui::Ui,
    views: [Option<ScreenView>; 2],
    shown: [bool; 2],
) -> ScreenInput {
    let available = ui.available_size();
    let sizes = [0, 1].map(|index| {
        if !shown[index] {
            return egui::Vec2::ZERO;
        }
        views[index].map_or_else(
            || {
                let (w, h) = Screen::ALL[index].default_size();
                #[allow(clippy::cast_precision_loss)]
                egui::vec2(w as f32, h as f32)
            },
            |view| view.size,
        )
    });
    let spacing = if shown == [true, true] { ui.spacing().item_spacing.x } else { 0.0 };
    let total_width = sizes.iter().map(|s| s.x).sum::<f32>().max(1.0);
    let max_height = sizes.iter().map(|s| s.y).fold(1.0, f32::max);
    let scale = ((available.x - spacing) / total_width).min(available.y / max_height).max(0.01);

    let mut input = ScreenInput::default();
    // Centered as a whole: `horizontal_centered` only centers vertically.
    let used_width = total_width.mul_add(scale, spacing);
    let left = ((available.x - used_width) / 2.0).max(0.0);
    ui.horizontal_centered(|ui| {
        ui.add_space(left);
        for (((screen, size), view), show) in
            Screen::ALL.into_iter().zip(sizes).zip(views).zip(shown)
        {
            if !show {
                continue;
            }
            let size = size * scale;
            match view {
                // The GamePad's screen is a touch screen: the mouse is the finger.
                Some(view) if screen == Screen::Drc => {
                    let response =
                        ui.add(egui::Image::new((view.texture, size)).sense(egui::Sense::drag()));
                    input.touch = held_at(&response, egui::PointerButton::Primary).map(|(x, y)| {
                        let (width, height) = Screen::Drc.default_size();
                        #[allow(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            clippy::cast_precision_loss
                        )]
                        let at =
                            ((x * (width - 1) as f32) as u16, (y * (height - 1) as f32) as u16);
                        at
                    });
                    input.tilt = held_at(&response, egui::PointerButton::Secondary);
                    if response.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                }
                Some(view) => {
                    let response =
                        ui.add(egui::Image::new((view.texture, size)).sense(egui::Sense::click()));
                    input.double_clicked |= response.double_clicked();
                }
                None => {
                    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                    ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
                    let label = match screen {
                        Screen::Tv => "TV",
                        Screen::Drc => "GamePad",
                    };
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        label,
                        egui::FontId::proportional(16.0),
                        egui::Color32::DARK_GRAY,
                    );
                }
            }
        }
    });
    input
}

/// Where `button` holds `response`, `0.0..=1.0` across it. A drag that leaves the picture
/// keeps holding its edge, like a finger at the rim of the screen.
fn held_at(response: &egui::Response, button: egui::PointerButton) -> Option<(f32, f32)> {
    let down = response.ctx.input(|input| input.pointer.button_down(button));
    let position =
        response.interact_pointer_pos().filter(|_| down && response.is_pointer_button_down_on())?;
    let rect = response.rect;
    Some((
        ((position.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
        ((position.y - rect.top()) / rect.height()).clamp(0.0, 1.0),
    ))
}
