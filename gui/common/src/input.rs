//! Keyboard and controller → GamePad mapping. Keys are identified by name (`"X"`,
//! `"ArrowUp"`, ...) so this crate stays independent of the GUI toolkit.
use std::collections::BTreeMap;

use gilrs::{Axis, Button, EventType, Gilrs};
use wiiu_peripherals::{GamePadState, VpadButtons, controller::Stick};

/// The action that holds the microphone open; it has no VPAD bit.
const MIC: &str = "mic";

/// Directions of the left stick.
const STICK_KEYS: [&str; 4] = ["stick_up", "stick_down", "stick_left", "stick_right"];

/// Action name → key name.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct KeyBindings(pub BTreeMap<String, String>);

impl Default for KeyBindings {
    fn default() -> Self {
        let pairs = [
            ("a", "X"),
            ("b", "Z"),
            ("x", "S"),
            ("y", "A"),
            ("l", "Q"),
            ("r", "W"),
            ("zl", "1"),
            ("zr", "2"),
            ("plus", "Enter"),
            ("minus", "Backspace"),
            ("home", "H"),
            ("up", "ArrowUp"),
            ("down", "ArrowDown"),
            ("left", "ArrowLeft"),
            ("right", "ArrowRight"),
            ("stick_up", "I"),
            ("stick_down", "K"),
            ("stick_left", "J"),
            ("stick_right", "L"),
            ("mic", "M"),
        ];
        Self(pairs.into_iter().map(|(a, k)| (a.to_owned(), k.to_owned())).collect())
    }
}

/// Every action a key or a controller button can be bound to, in the order settings list them.
pub const ACTIONS: [&str; 26] = [
    "a",
    "b",
    "x",
    "y",
    "l",
    "r",
    "zl",
    "zr",
    "plus",
    "minus",
    "home",
    "up",
    "down",
    "left",
    "right",
    "stick_l",
    "stick_r",
    "stick_up",
    "stick_down",
    "stick_left",
    "stick_right",
    "mic",
    "tilt_up",
    "tilt_down",
    "tilt_left",
    "tilt_right",
];

/// Actions that tip the GamePad: forward (top edge down), back, left and right.
const TILT_ACTIONS: [&str; 4] = ["tilt_up", "tilt_down", "tilt_left", "tilt_right"];

/// The tilt the held actions ask for: `x` leans right, `y` tips the top edge up.
fn tilt_from(down: impl Fn(&str) -> bool) -> Stick {
    let [up, dn, lf, rt] = TILT_ACTIONS.map(down);
    let axis = |neg: bool, pos: bool| f32::from(i8::from(pos) - i8::from(neg));
    Stick { x: axis(lf, rt), y: axis(dn, up) }
}

/// Controller buttons that can be bound, by their names in [`PadBindings`].
const BUTTON_NAMES: [&str; 19] = [
    "South",
    "East",
    "North",
    "West",
    "C",
    "Z",
    "LeftTrigger",
    "LeftTrigger2",
    "RightTrigger",
    "RightTrigger2",
    "Select",
    "Start",
    "Mode",
    "LeftThumb",
    "RightThumb",
    "DPadUp",
    "DPadDown",
    "DPadLeft",
    "DPadRight",
];

/// Button action names and their VPAD bits.
const BUTTONS: [(&str, VpadButtons); 17] = [
    ("a", VpadButtons::A),
    ("b", VpadButtons::B),
    ("x", VpadButtons::X),
    ("y", VpadButtons::Y),
    ("l", VpadButtons::L),
    ("r", VpadButtons::R),
    ("zl", VpadButtons::ZL),
    ("zr", VpadButtons::ZR),
    ("plus", VpadButtons::PLUS),
    ("minus", VpadButtons::MINUS),
    ("home", VpadButtons::HOME),
    ("up", VpadButtons::UP),
    ("down", VpadButtons::DOWN),
    ("left", VpadButtons::LEFT),
    ("right", VpadButtons::RIGHT),
    ("stick_l", VpadButtons::STICK_L),
    ("stick_r", VpadButtons::STICK_R),
];

impl KeyBindings {
    /// The tilt asked for by the held keys.
    pub fn tilt(&self, is_down: impl Fn(&str) -> bool) -> Stick {
        tilt_from(|action| self.0.get(action).is_some_and(|key| is_down(key)))
    }

    /// Builds a GamePad sample from `is_down(key_name)`.
    pub fn gamepad(&self, is_down: impl Fn(&str) -> bool) -> GamePadState {
        let down = |action: &str| self.0.get(action).is_some_and(|key| is_down(key));
        let held = BUTTONS
            .iter()
            .filter(|(action, _)| down(action))
            .fold(VpadButtons::empty(), |acc, (_, b)| acc | *b);
        let [up, dn, lf, rt] = STICK_KEYS.map(down);
        let axis = |neg: bool, pos: bool| f32::from(i8::from(pos) - i8::from(neg));
        GamePadState {
            held,
            left_stick: Stick { x: axis(lf, rt), y: axis(dn, up) },
            mic: f32::from(u8::from(down(MIC))),
            ..GamePadState::default()
        }
    }
}

/// Stick movement below this is noise of a worn stick.
const DEAD_ZONE: f32 = 0.15;

/// Action name → controller button name (`gilrs::Button` variants).
///
/// The defaults go by position: `East` is the Wii U's A wherever a pad prints its
/// labels, as on Nintendo's own controllers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct PadBindings(pub BTreeMap<String, String>);

impl Default for PadBindings {
    fn default() -> Self {
        let pairs = [
            ("a", "East"),
            ("b", "South"),
            ("x", "North"),
            ("y", "West"),
            ("l", "LeftTrigger"),
            ("r", "RightTrigger"),
            ("zl", "LeftTrigger2"),
            ("zr", "RightTrigger2"),
            ("plus", "Start"),
            ("minus", "Select"),
            ("home", "Mode"),
            ("up", "DPadUp"),
            ("down", "DPadDown"),
            ("left", "DPadLeft"),
            ("right", "DPadRight"),
            ("stick_l", "LeftThumb"),
            ("stick_r", "RightThumb"),
        ];
        Self(pairs.into_iter().map(|(a, b)| (a.to_owned(), b.to_owned())).collect())
    }
}

fn button_from_name(name: &str) -> Option<Button> {
    Some(match name {
        "South" => Button::South,
        "East" => Button::East,
        "North" => Button::North,
        "West" => Button::West,
        "C" => Button::C,
        "Z" => Button::Z,
        "LeftTrigger" => Button::LeftTrigger,
        "LeftTrigger2" => Button::LeftTrigger2,
        "RightTrigger" => Button::RightTrigger,
        "RightTrigger2" => Button::RightTrigger2,
        "Select" => Button::Select,
        "Start" => Button::Start,
        "Mode" => Button::Mode,
        "LeftThumb" => Button::LeftThumb,
        "RightThumb" => Button::RightThumb,
        "DPadUp" => Button::DPadUp,
        "DPadDown" => Button::DPadDown,
        "DPadLeft" => Button::DPadLeft,
        "DPadRight" => Button::DPadRight,
        _ => return None,
    })
}

/// A stick position with the dead zone removed and the rest rescaled to full range.
fn apply_dead_zone(x: f32, y: f32) -> Stick {
    let length = x.hypot(y);
    if length <= DEAD_ZONE {
        return Stick::default();
    }
    let scale = ((length - DEAD_ZONE) / (1.0 - DEAD_ZONE)).min(1.0) / length;
    Stick { x: x * scale, y: y * scale }
}

impl PadBindings {
    /// Builds a GamePad sample from a controller's state.
    pub fn gamepad(
        &self,
        is_pressed: impl Fn(Button) -> bool,
        axis: impl Fn(Axis) -> f32,
    ) -> GamePadState {
        let down = |action: &str| {
            self.0.get(action).and_then(|name| button_from_name(name)).is_some_and(&is_pressed)
        };
        let held = BUTTONS
            .iter()
            .filter(|(action, _)| down(action))
            .fold(VpadButtons::empty(), |acc, (_, b)| acc | *b);
        GamePadState {
            held,
            left_stick: apply_dead_zone(axis(Axis::LeftStickX), axis(Axis::LeftStickY)),
            right_stick: apply_dead_zone(axis(Axis::RightStickX), axis(Axis::RightStickY)),
            mic: f32::from(u8::from(down(MIC))),
            ..GamePadState::default()
        }
    }
}

/// The stick pushed further of two sources.
fn stronger(a: Stick, b: Stick) -> Stick {
    if b.x.hypot(b.y) > a.x.hypot(a.y) { b } else { a }
}

/// Several input devices driving one GamePad: buttons add up, the stronger stick wins.
pub fn merge(a: GamePadState, b: GamePadState) -> GamePadState {
    GamePadState {
        held: a.held | b.held,
        left_stick: stronger(a.left_stick, b.left_stick),
        right_stick: stronger(a.right_stick, b.right_stick),
        touch: a.touch.or(b.touch),
        motion: a.motion.or(b.motion),
        mic: a.mic.max(b.mic),
    }
}

/// Physical controllers, read through gilrs.
pub struct Controllers {
    /// `None` when the platform's gamepad backend is unavailable.
    gilrs: Option<Gilrs>,
}

impl std::fmt::Debug for Controllers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Controllers").field("available", &self.gilrs.is_some()).finish()
    }
}

impl Controllers {
    pub fn new() -> Self {
        let gilrs = match Gilrs::new() {
            Ok(gilrs) => Some(gilrs),
            Err(error) => {
                log::warn!("controllers are unavailable: {error}");
                None
            }
        };
        Self { gilrs }
    }

    /// Takes in new input. Returns a line per controller that came or went.
    pub fn poll(&mut self) -> Vec<String> {
        let Some(gilrs) = &mut self.gilrs else { return Vec::new() };
        let mut changes = Vec::new();
        while let Some(event) = gilrs.next_event() {
            let name = gilrs.gamepad(event.id).name().to_owned();
            match event.event {
                EventType::Connected => changes.push(format!("controller connected: {name}")),
                EventType::Disconnected => {
                    changes.push(format!("controller disconnected: {name}"));
                }
                _ => {}
            }
        }
        changes
    }

    /// The name of a button held on any controller, for binding it to an action.
    pub fn pressed_button(&self) -> Option<&'static str> {
        let gilrs = self.gilrs.as_ref()?;
        gilrs.gamepads().find_map(|(_, pad)| {
            BUTTON_NAMES
                .into_iter()
                .find(|name| button_from_name(name).is_some_and(|button| pad.is_pressed(button)))
        })
    }

    /// The tilt asked for by buttons held on any controller.
    pub fn tilt(&self, bindings: &PadBindings) -> Stick {
        let Some(gilrs) = &self.gilrs else { return Stick::default() };
        gilrs.gamepads().fold(Stick::default(), |tilt, (_, pad)| {
            let held = tilt_from(|action| {
                let button = bindings.0.get(action).and_then(|name| button_from_name(name));
                button.is_some_and(|button| pad.is_pressed(button))
            });
            stronger(tilt, held)
        })
    }

    /// The combined state of every connected controller.
    pub fn gamepad(&self, bindings: &PadBindings) -> GamePadState {
        let Some(gilrs) = &self.gilrs else { return GamePadState::default() };
        gilrs.gamepads().fold(GamePadState::default(), |state, (_, pad)| {
            merge(state, bindings.gamepad(|button| pad.is_pressed(button), |axis| pad.value(axis)))
        })
    }
}

impl Default for Controllers {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_keys_to_buttons_and_stick() {
        let bindings = KeyBindings::default();
        let pad = bindings.gamepad(|key| matches!(key, "X" | "ArrowUp" | "L"));
        assert_eq!(pad.held, VpadButtons::A | VpadButtons::UP);
        assert_eq!((pad.left_stick.x, pad.left_stick.y), (1.0, 0.0));
    }

    #[test]
    fn maps_controller_buttons_by_position() {
        let pad = PadBindings::default().gamepad(
            |button| matches!(button, Button::East | Button::DPadUp | Button::LeftTrigger2),
            |_| 0.0,
        );
        assert_eq!(pad.held, VpadButtons::A | VpadButtons::UP | VpadButtons::ZL);
    }

    #[test]
    fn sticks_have_a_dead_zone_and_reach_full_range() {
        let pad = |x, y| {
            PadBindings::default().gamepad(
                |_| false,
                move |axis| match axis {
                    Axis::LeftStickX => x,
                    Axis::LeftStickY => y,
                    _ => 0.0,
                },
            )
        };
        assert_eq!(pad(0.1, -0.1).left_stick, Stick::default());
        assert_eq!(pad(0.0, 1.0).left_stick, Stick { x: 0.0, y: 1.0 });
        assert!(pad(1.0, 0.0).left_stick.x <= 1.0);
    }

    #[test]
    fn every_bindable_name_is_known() {
        assert!(BUTTON_NAMES.iter().all(|name| button_from_name(name).is_some()));
        let defaults = KeyBindings::default();
        assert!(defaults.0.keys().all(|action| ACTIONS.contains(&action.as_str())));
        let defaults = PadBindings::default();
        assert!(defaults.0.keys().all(|action| ACTIONS.contains(&action.as_str())));
    }

    #[test]
    fn keyboard_and_controller_add_up() {
        let keyboard = KeyBindings::default().gamepad(|key| key == "X");
        let controller = PadBindings::default().gamepad(
            |button| button == Button::South,
            |axis| if axis == Axis::RightStickX { 1.0 } else { 0.0 },
        );
        let both = merge(keyboard, controller);
        assert_eq!(both.held, VpadButtons::A | VpadButtons::B);
        assert_eq!(both.right_stick.x, 1.0);
    }

    /// Intent: tilt actions are read apart from the buttons, opposite ones cancel.
    /// Pass: a bound up key tips up; up and down together stay level.
    #[test]
    fn tilt_keys_tip_the_gamepad() {
        let mut bindings = KeyBindings::default();
        bindings.0.insert("tilt_up".to_owned(), "T".to_owned());
        bindings.0.insert("tilt_down".to_owned(), "G".to_owned());
        assert_eq!(bindings.tilt(|key| key == "T"), Stick { x: 0.0, y: 1.0 });
        assert_eq!(bindings.tilt(|key| key == "T" || key == "G"), Stick::default());
    }

    /// Intent: the MIC action is not a button: it opens the microphone instead.
    /// Pass: its key sets `mic` to 1 and no VPAD bit; merging keeps the louder source.
    #[test]
    fn the_mic_key_opens_the_microphone() {
        let pad = KeyBindings::default().gamepad(|key| key == "M");
        assert_eq!((pad.held, pad.mic), (VpadButtons::empty(), 1.0));
        assert_eq!(merge(GamePadState::default(), pad).mic, 1.0);
    }
}
