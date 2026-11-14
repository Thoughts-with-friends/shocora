//! The GamePad's microphone and motion sensors, as the host stands in for them: a held MIC
//! button with a strength bar, and a GamePad tilted by a right-button drag on its screen or
//! by the Tilt actions bound to keys or controller buttons.
use std::time::Instant;

use wiiu_emu::{Pose, TiltMotion};

use super::*;

/// Tilt of a drag from the middle of the GamePad's screen to its edge, in degrees.
const DRAG_TILT: f32 = 90.0;
/// Tilt while a Tilt action is held, in degrees.
const HELD_TILT: f32 = 60.0;
/// How fast a held Tilt action tips the GamePad, and it levels out again, in degrees/s.
const TILT_SPEED: f32 = 240.0;
/// Turn per scrolled point of the mouse wheel during a drag, in degrees.
const WHEEL_TURN: f32 = 0.3;

/// What stands in for the GamePad's microphone and motion sensors.
#[derive(Debug)]
pub(super) struct MotionInput {
    /// Where the right button holds the GamePad's screen in the main window / its own.
    pub tilt: Option<(f32, f32)>,
    pub window_tilt: Option<(f32, f32)>,
    /// The on-screen MIC button is held.
    pub mic_button: bool,
    sensors: TiltMotion,
    /// Where the GamePad is held now.
    pose: Pose,
    last_sample: Instant,
}

impl Default for MotionInput {
    fn default() -> Self {
        Self {
            tilt: None,
            window_tilt: None,
            mic_button: false,
            sensors: TiltMotion::default(),
            pose: Pose::default(),
            last_sample: Instant::now(),
        }
    }
}

/// `value` moved toward `target` by at most `limit`.
fn approach(value: f32, target: f32, limit: f32) -> f32 {
    value + (target - value).clamp(-limit, limit)
}

impl ShocoraApp {
    /// Adds the microphone and motion to the GamePad sample.
    pub(super) fn add_mic_and_motion(&mut self, ctx: &egui::Context, pad: &mut GamePadState) {
        let strength = f32::from(self.config.mic_strength.min(100)) / 100.0;
        pad.mic = pad.mic.max(f32::from(u8::from(self.motion.mic_button))) * strength;

        let held = ctx.input(|input| {
            self.config
                .key_bindings
                .tilt(|name| egui::Key::from_name(name).is_some_and(|key| input.key_down(key)))
        });
        let buttons = self.controllers.tilt(&self.config.pad_bindings);
        let held = if buttons.x.hypot(buttons.y) > held.x.hypot(held.y) { buttons } else { held };

        let motion = &mut self.motion;
        let step = motion.last_sample.elapsed().as_secs_f32().min(0.1);
        motion.last_sample = Instant::now();
        if let Some((x, y)) = motion.tilt.or(motion.window_tilt) {
            let scrolled = ctx.input(|input| input.smooth_scroll_delta.y);
            motion.pose = Pose {
                pitch: (0.5 - y) * 2.0 * DRAG_TILT,
                roll: (x - 0.5) * 2.0 * DRAG_TILT,
                yaw: scrolled.mul_add(WHEEL_TURN, motion.pose.yaw),
            };
        } else {
            let limit = TILT_SPEED * step;
            motion.pose = Pose {
                pitch: approach(motion.pose.pitch, held.y * HELD_TILT, limit),
                roll: approach(motion.pose.roll, held.x * HELD_TILT, limit),
                yaw: approach(motion.pose.yaw, 0.0, limit),
            };
        }
        if motion.pose == Pose::default() && motion.tilt.or(motion.window_tilt).is_none() {
            motion.sensors.release();
            pad.motion = None;
        } else {
            pad.motion = Some(motion.sensors.update(motion.pose, step));
        }
    }

    /// The MIC button, held to speak into the GamePad, and how loud it hears.
    pub(super) fn mic_controls(&mut self, ui: &mut egui::Ui) {
        let held = self.last_gamepad.mic > 0.0;
        let button = egui::Button::new("🎤 MIC").selected(held);
        let response = ui.add(button.sense(egui::Sense::drag())).on_hover_text(tr("mic.hint"));
        self.motion.mic_button = response.is_pointer_button_down_on();
        let slider = egui::Slider::new(&mut self.config.mic_strength, 0..=100).suffix(" %");
        let response = ui.add(slider).on_hover_text(tr("mic.strength"));
        if response.drag_stopped() || (response.changed() && !response.dragged()) {
            self.save_config();
        }
    }

    /// How the GamePad is tilted from here.
    pub(super) fn motion_settings(ui: &mut egui::Ui) {
        ui.strong(tr("motion.title"));
        ui.label(tr("motion.mouse_hint"));
        ui.label(tr("motion.tilt_hint"));
    }
}
