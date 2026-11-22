//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ vpad_tests.rs            │   │ pass / fail        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tests of vpad.rs         │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for vpad.rs (GamePad library (VPAD)). They run with `cargo test` only and are not
//! part of the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use wiiu_peripherals::VpadButtons;

use super::offset;
use crate::hle::test_env::{SCRATCH, TestEnv};

#[test]
fn touches_reach_the_title_and_convert_back_to_the_screen() {
    let mut env = TestEnv::new();
    env.os.gamepad.touch = Some((427, 240));
    env.call("vpad", "VPADRead", &[0, SCRATCH, 1, 0]);
    let raw = SCRATCH + offset::TOUCH[0];
    let word = |env: &TestEnv, at: u32| env.memory.read_u16(at).unwrap();
    // Cemu's formula: x * 3883 + 92, 4095 - y * 3694 - 254.
    assert_eq!((word(&env, raw), word(&env, raw + 2)), (2033, 1994));
    assert_eq!((word(&env, raw + 4), word(&env, raw + 6)), (1, 0));

    let out = SCRATCH + 0x200;
    env.call("vpad", "VPADGetTPCalibratedPoint", &[0, out, raw]);
    let (x, y) = (word(&env, out), word(&env, out + 2));
    assert!(x.abs_diff(640) <= 1 && y.abs_diff(360) <= 1, "{x},{y}");

    // Released: not valid any more, the position stays.
    env.os.gamepad.touch = None;
    env.call("vpad", "VPADRead", &[0, SCRATCH, 1, 0]);
    assert_eq!(word(&env, raw), 2033);
    assert_eq!((word(&env, raw + 4), word(&env, raw + 6)), (0, 3));
}

/// Intent: without a motion source the GamePad reads as Cemu reports it (lying still,
/// axes the identity), and a source's sample reaches the title at wut's offsets.
/// Pass: direction X axis is (1,0,0) at rest; a sample's gyro and direction come back.
#[test]
fn motion_reaches_the_title() {
    let mut env = TestEnv::new();
    env.call("vpad", "VPADRead", &[0, SCRATCH, 1, 0]);
    let float = |env: &TestEnv, at: u32| env.memory.read_f32(SCRATCH + at).unwrap();
    assert_eq!(float(&env, offset::DIRECTION), 1.0);
    assert_eq!(float(&env, offset::DIRECTION + 0x10), 1.0);
    assert_eq!(float(&env, offset::ACC + 0x14), 1.0);

    let mut motion = wiiu_peripherals::Motion::REST;
    motion.gyro = [0.25, -0.5, 0.0];
    motion.direction[2] = [0.0, -1.0, 0.0];
    env.os.gamepad.motion = Some(motion);
    env.call("vpad", "VPADRead", &[0, SCRATCH, 1, 0]);
    let gyro = offset::ACC + 0x1C;
    assert_eq!((float(&env, gyro), float(&env, gyro + 4)), (0.25, -0.5));
    assert_eq!(float(&env, offset::DIRECTION + 0x1C), -1.0);
}

#[test]
fn reports_held_and_edges() {
    let mut env = TestEnv::new();
    env.os.gamepad.held = VpadButtons::A;
    assert_eq!(env.call("vpad", "VPADRead", &[0, SCRATCH, 1, SCRATCH + 0x100]), 1);
    assert_eq!(env.memory.read_u32(SCRATCH).unwrap(), VpadButtons::A.bits());
    assert_eq!(env.memory.read_u32(SCRATCH + 4).unwrap(), VpadButtons::A.bits());
    env.call("vpad", "VPADRead", &[0, SCRATCH, 1, 0]);
    assert_eq!(env.memory.read_u32(SCRATCH + 4).unwrap(), 0);
}

/// Intent: after `VPADSetBtnRepeat`, a held button sets `VPAD_REPEAT` once the delay
/// has passed and then once per pulse, as Cemu's `VPADController::VPADRead`.
/// Pass: no repeat right after pressing; repeat after the delay; none within the
/// pulse; the guest call stores Cemu's `seconds * 200`.
#[test]
fn held_buttons_repeat() {
    let mut state =
        super::VpadChannel { repeat_delay: 100, repeat_pulse: 20, ..Default::default() };
    let ms = super::TICKS_PER_MS;
    let mut hold = 0x8000;
    assert_eq!(super::repeat_and_edges(&mut state, &mut hold, 0), (0x8000, 0));
    assert_eq!(hold, 0x8000);
    let mut hold = 0x8000;
    assert_eq!(super::repeat_and_edges(&mut state, &mut hold, 101 * ms), (0, 0));
    assert_eq!(hold, 0x8000_8000);
    let mut hold = 0x8000;
    super::repeat_and_edges(&mut state, &mut hold, 110 * ms);
    assert_eq!(hold, 0x8000, "within the pulse");

    let mut env = TestEnv::new();
    env.cpu.fpr[1].ps0 = 0.5;
    env.cpu.fpr[2].ps0 = 0.1;
    env.call("vpad", "VPADSetBtnRepeat", &[0]);
    let c = &env.os.vpad.channels[0];
    assert_eq!((c.repeat_delay, c.repeat_pulse), (100, 20));
}

/// Intent: channel 1 has no GamePad: Cemu reports `VPAD_READ_ERR_NO_CONTROLLER`.
/// Pass: 0 samples, error -2.
#[test]
fn second_channel_has_no_controller() {
    let mut env = TestEnv::new();
    let err = SCRATCH + 0x100;
    assert_eq!(env.call("vpad", "VPADRead", &[1, SCRATCH, 1, err]), 0);
    assert_eq!(env.memory.read_u32(err).unwrap(), (-2_i32).cast_unsigned());
}
