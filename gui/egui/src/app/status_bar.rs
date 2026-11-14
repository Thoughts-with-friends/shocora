//! The status bar: state, speed and picture settings at a glance.
use super::*;

impl ShocoraApp {
    pub(super) fn status_bar(&mut self, ui: &mut egui::Ui) {
        // Parts of the bar that change a setting look like plain text but respond to clicks.
        fn setting(ui: &mut egui::Ui, text: String, hint: &str) -> bool {
            ui.add(egui::Label::new(text).sense(egui::Sense::click()))
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(hint)
                .clicked()
        }
        if matches!(self.netplay, Netplay::Guest(_)) {
            self.guest_status_bar(ui);
            return;
        }
        ui.horizontal(|ui| {
            let running = self.status == Some(Status::Running);
            let state = state_text(self.host_status().state);
            if running {
                if setting(ui, state, tr("status.pause_hint")) {
                    self.console.send(if self.paused {
                        ConsoleCommand::Resume
                    } else {
                        ConsoleCommand::Pause
                    });
                }
            } else {
                ui.label(state);
            }
            ui.separator();
            let fps = fps_text(self.fps, self.frame_rate());
            if setting(ui, fps, tr("status.rate_hint")) {
                self.toggle_frame_rate();
            }
            ui.separator();
            if !matches!(self.netplay, Netplay::Guest(_))
                && setting(
                    ui,
                    trf("status.speed", &[("speed", &self.speed.label())]),
                    tr("status.speed_hint"),
                )
            {
                self.next_speed();
            }
            ui.separator();
            let generation = if self.config.frame_generation {
                tr("status.framegen_on")
            } else {
                tr("status.framegen_off")
            };
            if setting(ui, generation.to_owned(), tr("status.framegen_hint")) {
                self.set_frame_generation(!self.config.frame_generation);
            }
            ui.separator();
            let scale = match self.config.upscale.method {
                UpscaleMethod::None => tr("status.xbrz_off").to_owned(),
                UpscaleMethod::Xbrz => format!("xBRZ {}x", self.config.upscale.factor),
            };
            if upscale::available() && setting(ui, scale, tr("status.xbrz_hint")) {
                self.cycle_upscale();
            }
            ui.separator();
            self.mic_controls(ui);
            ui.separator();
            ui.label(self.title.as_deref().unwrap_or_else(|| tr("status.no_title")));
            ui.separator();
            #[allow(clippy::cast_precision_loss)]
            let millions = format!("{:.1}", self.instructions as f64 / 1e6);
            ui.label(trf("status.instructions", &[("millions", &millions)]));
            if let Some(status) = self.netplay_status() {
                ui.separator();
                ui.label(status);
            }
            if matches!(self.netplay, Netplay::Host(_)) {
                self.netplay_controls(ui);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let api = format!("{} ({})", self.active_api.label(), Presenter::backend_name());
                if setting(ui, api, tr("status.api_hint")) {
                    self.cycle_graphics_api();
                }
                if self.config.graphics_api != self.active_api {
                    ui.label(trf(
                        "status.after_restart",
                        &[("api", &self.config.graphics_api.label())],
                    ));
                }
            });
        });
    }
}

pub(super) fn state_text(state: HostState) -> String {
    match state {
        HostState::Paused => tr("status.paused").to_owned(),
        HostState::Running => tr("status.running").to_owned(),
        HostState::Exited(code) => trf("status.exited", &[("code", &code)]),
        HostState::Crashed => tr("status.crashed").to_owned(),
        HostState::Idle => tr("status.idle").to_owned(),
    }
}

pub(super) fn fps_text(fps: f32, rate: FrameRate) -> String {
    let rate = match rate {
        FrameRate::Title => tr("status.rate_title"),
        FrameRate::Max30 => tr("status.rate_max30"),
        FrameRate::Force60 => tr("status.rate_force60"),
    };
    format!("{fps:.1} fps ({rate})")
}
