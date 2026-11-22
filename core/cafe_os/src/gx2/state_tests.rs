//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ gx2/state_tests.rs       │   │ pass / fail        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tests of state.rs        │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for state.rs (fixed-function state). They run with `cargo test` only and are not
//! part of the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;

#[test]
fn register_encodings_match_cemu() {
    // Depth test on, write on, LESS.
    assert_eq!(depth_stencil(1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0), 0b1_0110);
    assert_eq!(channel_masks([0xF, 0, 0, 0, 0, 0, 0, 0xA]), 0xA000_000F);
    assert_eq!(blend_control(4, 5, 0, 0, 1, 0, 0) & 0x1F, 4);
    assert_eq!(scissor(1, 2, 3, 4), [1 | (2 << 16) | (1 << 31), 4 | (6 << 16)]);
}
