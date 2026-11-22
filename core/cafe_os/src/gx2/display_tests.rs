//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ gx2/display_tests.rs     │   │ pass / fail        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tests of display.rs      │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for display.rs (display & surface copies). They run with `cargo test` only and
//! are not part of the emulator you play with. Terms such as HLE, JIT or shader are explained
//! in `docs/guide/wiiu_term.md`.

use crate::hle::test_env::{SCRATCH, TestEnv};

/// Intent: titles allocate their TV scan buffers from `GX2CalcTVSize`: width x height
/// x 4 bytes per buffer.
/// Pass: 720p (mode 3) double-buffered is 1280 x 720 x 4 x 2 bytes, and no scaling is
/// reported; 1080p triple-buffered is 1920 x 1080 x 4 x 3.
#[test]
fn tv_buffer_sizes_follow_mode_and_buffering() {
    let mut env = TestEnv::new();
    env.call("gx2", "GX2CalcTVSize", &[3, 0x1A, 2, SCRATCH, SCRATCH + 4]);
    assert_eq!(env.memory.read_u32(SCRATCH).unwrap(), 1280 * 720 * 4 * 2);
    assert_eq!(env.memory.read_u32(SCRATCH + 4).unwrap(), 0);
    env.call("gx2", "GX2CalcTVSize", &[5, 0x1A, 4, SCRATCH, 0]);
    assert_eq!(env.memory.read_u32(SCRATCH).unwrap(), 1920 * 1080 * 4 * 3);
}

/// Intent: `GX2Init` remembers the core that owns the GPU (`GX2GetMainCoreId`).
/// Pass: after the call GX2 is initialised and its main core is the caller's (1).
#[test]
fn init_remembers_the_main_core() {
    let mut env = TestEnv::new();
    env.call("gx2", "GX2Init", &[0]);
    assert!(env.os.gx2.initialized);
    assert_eq!(env.call("gx2", "GX2GetMainCoreId", &[]), 1);
}

/// Intent: a copy from a `LINEAR_SPECIAL` surface is done in memory, tiling the
/// pixels into the target's layout (GX2 does it on the CPU; BotW's wind texture).
/// Pass: every pixel of a 64 x 16 R16F linear surface is found in the 2D-tiled target
/// where the target's tiling puts it.
#[test]
fn linear_special_copies_tile_into_memory() {
    let mut env = TestEnv::new();
    let (source_image, target_image) = (SCRATCH + 0x1_0000, SCRATCH + 0x2_0000);
    let surface = |image, tile_mode| super::Gx2Surface {
        dim: 1,
        width: 64,
        height: 16,
        depth: 1,
        num_levels: 1,
        format: 0x806,
        image,
        tile_mode,
        ..super::Gx2Surface::default()
    };
    let (source, target) = (surface(source_image, 16), surface(target_image, 4));
    source.write(&mut env.memory, SCRATCH).unwrap();
    target.write(&mut env.memory, SCRATCH + 0x100).unwrap();
    for i in 0..64 * 16 {
        env.memory.write_u16(source_image + 2 * i, i as u16 + 1).unwrap();
    }
    env.call("gx2", "GX2CopySurface", &[SCRATCH, 0, 0, SCRATCH + 0x100, 0, 0]);
    let info = target.info(0);
    let layout = super::Layout {
        tile_mode: info.hw_tile_mode,
        bpp: info.bpp,
        pitch: info.pitch,
        height: info.height,
        samples: 1,
        is_depth: false,
        pipe_swizzle: 0,
        bank_swizzle: 0,
    };
    assert_eq!(info.hw_tile_mode, 4);
    for (x, y) in (0..16).flat_map(|y| (0..64).map(move |x| (x, y))) {
        let at = target_image + layout.address(x, y, 0);
        assert_eq!(env.memory.read_u16(at).unwrap(), (y * 64 + x) as u16 + 1, "({x}, {y})");
    }
}
