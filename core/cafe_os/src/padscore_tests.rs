//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ padscore_tests.rs        │   │ pass / fail        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tests of padscore.rs     │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for padscore.rs (WPAD/KPAD libraries). They run with `cargo test` only and are
//! not part of the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use wiiu_peripherals::{GamePadState, VpadButtons};

use super::kpad;
use crate::hle::test_env::{SCRATCH, TestEnv};

/// Intent: `WPADProbe` tells titles which channels have a Pro Controller.
/// Pass: an empty channel returns -1 with device 0xFD; a connected one returns 0 with
/// device 31 (`WPAD_DEV_URCC`).
#[test]
fn probe_reports_connected_pro_controllers() {
    let mut env = TestEnv::new();
    assert_eq!(env.call("padscore", "WPADProbe", &[0, SCRATCH]) as i32, -1);
    assert_eq!(env.memory.read_u32(SCRATCH).unwrap(), 0xFD);
    env.os.pro_controllers[0] = Some(GamePadState::default());
    assert_eq!(env.call("padscore", "WPADProbe", &[0, SCRATCH]), 0);
    assert_eq!(env.memory.read_u32(SCRATCH).unwrap(), 31);
}

/// Intent: `KPADReadEx` gives a Pro Controller sample with the buttons in its own bit
/// layout (A is 0x10, not the GamePad's 0x8000) and edge bits for new presses.
/// Pass: one sample, no error, hold and trigger both 0x10, format 22 (`WPAD_FMT_URCC`).
#[test]
fn kpad_reads_pro_controller_buttons() {
    let mut env = TestEnv::new();
    let pad = GamePadState { held: VpadButtons::A, ..GamePadState::default() };
    env.os.pro_controllers[1] = Some(pad);
    let error = SCRATCH + 0x200;
    assert_eq!(env.call("padscore", "KPADReadEx", &[1, SCRATCH, 1, error]), 1);
    assert_eq!(env.memory.read_u32(error).unwrap(), 0);
    assert_eq!(env.memory.read_u32(SCRATCH + kpad::PRO_HOLD).unwrap(), 0x10);
    assert_eq!(env.memory.read_u32(SCRATCH + kpad::PRO_TRIGGER).unwrap(), 0x10);
    assert_eq!(env.memory.read_u8(SCRATCH + kpad::FORMAT).unwrap(), 22);
}

/// Intent: titles wait for `WPADGetStatus` to report the library ready (Xenoblade
/// Chronicles X polled it forever while it returned 0).
/// Pass: it returns 1.
#[test]
fn wpad_reports_ready() {
    let mut env = TestEnv::new();
    assert_eq!(env.call("padscore", "WPADGetStatus", &[]), 1);
}

/// Intent: a connect callback first hears "no controller" for every channel, then
/// "connected" once a Pro Controller is plugged in, as Cemu's 5 ms tick does.
/// Pass: the first tick queues (0, -1); after connecting, the next tick queues (0, 0).
#[test]
fn connect_callback_reports_disconnect_then_connect() {
    let mut env = TestEnv::new();
    env.call("padscore", "WPADSetConnectCallback", &[0, 0x0200_0000]);
    env.os.padscore_tick();
    assert_eq!(env.os.callbacks.pending(), 1);
    env.os.pro_controllers[0] = Some(GamePadState::default());
    env.os.padscore.next_tick = 0;
    env.os.padscore_tick();
    assert_eq!(env.os.callbacks.pending(), 2);
}
