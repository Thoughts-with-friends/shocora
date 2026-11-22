//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                                        ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ addrlib/tests.rs         │   │ pass / fail        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tests of mod.rs          │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for mod.rs (address library). They run with `cargo test` only and are not part of
//! the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;

#[test]
fn tv_color_buffer_layout() {
    // 1280x720 RGBA8, 2D tiled thin1: pitch 1280, macro-tile aligned height.
    let info = gx2_surface_info(0x1A, 1280, 720, 1, dim::D2, TM_2D_TILED_THIN1, 0, 0, false, false);
    assert_eq!(info.hw_tile_mode, TM_2D_TILED_THIN1);
    assert_eq!(info.pitch, 1280);
    assert_eq!(info.height, 720);
    assert_eq!(info.bpp, 32);
    assert_eq!(info.surf_size, 1280 * 720 * 4);
    assert_eq!(info.base_align, 0x800);
}

#[test]
fn small_mips_fall_back_to_micro_tiling() {
    let info = gx2_surface_info(0x1A, 256, 256, 1, dim::D2, TM_2D_TILED_THIN1, 0, 5, false, false);
    assert_eq!(info.hw_tile_mode, TM_1D_TILED_THIN1);
    assert_eq!((info.pitch, info.height), (8, 8));
}

#[test]
fn bc1_uses_4x4_blocks() {
    let info = gx2_surface_info(0x31, 128, 128, 1, dim::D2, TM_1D_TILED_THIN1, 0, 0, false, false);
    assert_eq!((info.pitch, info.height, info.bpp), (32, 32, 64));
    assert_eq!(info.surf_size, 32 * 32 * 8);
    assert_eq!((info.pixel_pitch, info.pixel_height), (128, 128));
}

#[test]
fn linear_aligned_pitch() {
    let info = gx2_surface_info(0x1A, 100, 10, 1, dim::D2, TM_LINEAR_ALIGNED, 0, 0, false, false);
    assert_eq!(info.pitch, 128);
    assert_eq!(info.base_align, 256);
}
