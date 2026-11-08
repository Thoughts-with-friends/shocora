//! Controller state as the OS input libraries (VPAD, KPAD) report it.

bitflags::bitflags! {
    /// `VPAD_BUTTON_*` bits of the Wii U GamePad.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    pub struct VpadButtons: u32 {
        const SYNC = 0x0000_0001;
        const HOME = 0x0000_0002;
        const MINUS = 0x0000_0004;
        const PLUS = 0x0000_0008;
        const R = 0x0000_0010;
        const L = 0x0000_0020;
        const ZR = 0x0000_0040;
        const ZL = 0x0000_0080;
        const DOWN = 0x0000_0100;
        const UP = 0x0000_0200;
        const RIGHT = 0x0000_0400;
        const LEFT = 0x0000_0800;
        const Y = 0x0000_1000;
        const X = 0x0000_2000;
        const B = 0x0000_4000;
        const A = 0x0000_8000;
        const TV = 0x0001_0000;
        const STICK_R = 0x0002_0000;
        const STICK_L = 0x0004_0000;
        const STICK_R_DOWN = 0x0080_0000;
        const STICK_R_UP = 0x0100_0000;
        const STICK_R_RIGHT = 0x0200_0000;
        const STICK_R_LEFT = 0x0400_0000;
        const STICK_L_DOWN = 0x0800_0000;
        const STICK_L_UP = 0x1000_0000;
        const STICK_L_RIGHT = 0x2000_0000;
        const STICK_L_LEFT = 0x4000_0000;
    }
}

/// A stick position, each axis in `-1.0..=1.0`.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Stick {
    pub x: f32,
    pub y: f32,
}

/// Stick travel at which the OS reports a direction as pressed.
const STICK_DIRECTION_THRESHOLD: f32 = 0.5;

impl GamePadState {
    /// `held` plus the button bits the OS derives from the sticks, which is how
    /// titles usually read a stick in their menus.
    pub fn held_with_stick_directions(&self) -> VpadButtons {
        let directions = |stick: Stick, [left, right, up, down]: [VpadButtons; 4]| {
            let mut held = VpadButtons::empty();
            held.set(left, stick.x <= -STICK_DIRECTION_THRESHOLD);
            held.set(right, stick.x >= STICK_DIRECTION_THRESHOLD);
            held.set(up, stick.y >= STICK_DIRECTION_THRESHOLD);
            held.set(down, stick.y <= -STICK_DIRECTION_THRESHOLD);
            held
        };
        self.held
            | directions(
                self.left_stick,
                [
                    VpadButtons::STICK_L_LEFT,
                    VpadButtons::STICK_L_RIGHT,
                    VpadButtons::STICK_L_UP,
                    VpadButtons::STICK_L_DOWN,
                ],
            )
            | directions(
                self.right_stick,
                [
                    VpadButtons::STICK_R_LEFT,
                    VpadButtons::STICK_R_RIGHT,
                    VpadButtons::STICK_R_UP,
                    VpadButtons::STICK_R_DOWN,
                ],
            )
    }

    /// A sample with only VPAD_BUTTON_* bits set (scripts, tests).
    pub const fn from_button_bits(bits: u32) -> Self {
        Self {
            held: VpadButtons::from_bits_truncate(bits),
            left_stick: Stick { x: 0.0, y: 0.0 },
            right_stick: Stick { x: 0.0, y: 0.0 },
            touch: None,
        }
    }
}

/// One GamePad sample.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GamePadState {
    pub held: VpadButtons,
    pub left_stick: Stick,
    pub right_stick: Stick,
    /// Touch position in GamePad screen pixels (854x480), if touched.
    pub touch: Option<(u16, u16)>,
}

/// Tracks edges between samples (`trigger` / `release` in `VPADStatus`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ButtonEdges {
    previous: VpadButtons,
}

impl ButtonEdges {
    /// Returns `(trigger, release)` for the new `held` set.
    pub fn update(&mut self, held: VpadButtons) -> (VpadButtons, VpadButtons) {
        let trigger = held & !self.previous;
        let release = self.previous & !held;
        self.previous = held;
        (trigger, release)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushed_sticks_count_as_buttons() {
        let pad = GamePadState {
            held: VpadButtons::A,
            left_stick: Stick { x: -0.8, y: 0.2 },
            right_stick: Stick { x: 0.0, y: -1.0 },
            touch: None,
        };
        assert_eq!(
            pad.held_with_stick_directions(),
            VpadButtons::A | VpadButtons::STICK_L_LEFT | VpadButtons::STICK_R_DOWN
        );
    }

    #[test]
    fn edges() {
        let mut edges = ButtonEdges::default();
        assert_eq!(edges.update(VpadButtons::A), (VpadButtons::A, VpadButtons::empty()));
        assert_eq!(
            edges.update(VpadButtons::A | VpadButtons::B),
            (VpadButtons::B, VpadButtons::empty())
        );
        assert_eq!(edges.update(VpadButtons::B), (VpadButtons::empty(), VpadButtons::A));
    }
}
