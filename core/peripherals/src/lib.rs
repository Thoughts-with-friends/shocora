//! Small Wii U devices, each independent of the rest of the system.
pub mod controller;
pub mod motion;
pub mod rtc;
pub mod timebase;

pub use controller::{GamePadState, Stick, VpadButtons};
pub use motion::{Motion, Pose, TiltMotion};
pub use rtc::CalendarTime;
pub use timebase::{GameSpeed, TimeBase};

#[cfg(test)]
mod tests {
    use super::*;

    /// Intent: an idle GamePad (the default state) reports nothing held, and a stick
    /// pushed fully left also reads as the stick's left direction, as the OS does.
    /// Pass: no buttons by default; full left gives `STICK_L_LEFT`.
    #[test]
    fn sticks_also_report_directions() {
        let mut pad = GamePadState::default();
        assert!(pad.held_with_stick_directions().is_empty());
        pad.left_stick.x = -1.0;
        assert!(pad.held_with_stick_directions().contains(VpadButtons::STICK_L_LEFT));
    }
}
