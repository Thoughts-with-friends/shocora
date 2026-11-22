//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//! ▲▲▲▲▲▲▲▲▲▲    ▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ emulator/load_tests.rs   │   │ pass / fail        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tests of load.rs         │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for load.rs (booting a game). They run with `cargo test` only and are not part of
//! the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;
use crate::Config;

#[test]
fn rejects_garbage_rpx() {
    let mut emu = Emulator::new(Config::default()).unwrap();
    assert!(emu.load_rpx("bad.rpx", b"not an elf").is_err());
    assert_eq!(*emu.status(), Status::Idle);
}
