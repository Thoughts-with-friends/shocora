//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                                        ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ latte_tests.rs           │   │ pass / fail        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tests of latte.rs        │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for latte.rs (draws → host work). They run with `cargo test` only and are not
//! part of the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;

/// Intent: what the renderer reads back of a linear target lands in guest memory where
/// the CPU (and a linear texture fetch) looks for each texel: row y at y × pitch × bytes.
/// Pass: a 2x3 `R32G32B32A32_FLOAT` target with a 64-texel pitch, drawn twice, is asked
/// for once at the flip; the answer puts every texel at `Layout::address` and leaves the
/// bytes after each row alone.
#[test]
fn linear_targets_are_read_back_to_their_rows() {
    use wiiu_mem_addr::{MEM2, Region, effective_to_physical};
    let mut memory = Memory::new();
    memory.map(Region::new("MEM2", MEM2.start, 0x10_0000)).unwrap();
    let base = MEM2.start + 0x1000;
    let words = SurfaceWords {
        address: effective_to_physical(base).unwrap(),
        width: 2,
        height: 3,
        depth: 1,
        pitch: 64,
        tile_mode: tile_mode::TM_LINEAR_ALIGNED,
        format: 0x823,
        ..SurfaceWords::default()
    };
    let mut latte = Latte::default();
    let key = Latte::key_of_target(&words, false);
    latte.note_readback(&words, key);
    latte.note_readback(&words, key);
    latte.flush_readbacks();
    let [RenderCommand::Readback { target, reply }] = &latte.commands[..] else {
        panic!("{:?}", latte.commands);
    };
    // The renderer's answer: every byte of texel (x, y) is x + 2y.
    let rows: Vec<u8> = (0..6_u8).flat_map(|texel| [texel; 16]).collect();
    reply.0.send(ReadbackData { target: *target, rows }).unwrap();
    latte.apply_readbacks(&mut memory);
    let layout = Layout {
        tile_mode: tile_mode::TM_LINEAR_ALIGNED,
        bpp: 128,
        pitch: 64,
        height: 3,
        samples: 1,
        is_depth: false,
        pipe_swizzle: 0,
        bank_swizzle: 0,
    };
    for y in 0..3 {
        for x in 0..2 {
            let at = base + layout.address(x, y, 0);
            assert_eq!(memory.read_u8(at + 15).unwrap(), (x + 2 * y) as u8, "({x}, {y})");
        }
        assert_eq!(memory.read_u8(base + layout.address(2, y, 0)).unwrap(), 0);
    }
}

fn target(width: u32, height: u32) -> SurfaceKey {
    SurfaceKey { address: 0x1000, width, height, layers: 1, format: HostFormat::Rgba8Unorm }
}

#[test]
fn exports_land_in_the_slots_the_shader_mask_enables() {
    // Exports 0 to 3 over slots 0, 1, 3 and 5.
    let slots = export_slots(0x00F0_F0FF, 0b1111);
    assert_eq!(slots[..5], [Some(0), Some(1), Some(3), Some(5), None]);
    // Nothing to close: exports stay where they are.
    assert_eq!(export_slots(0xFF, 0b11)[..2], [Some(0), Some(1)]);
    assert_eq!(export_slots(0xF0, 0b10)[1], Some(1));
    assert_eq!(export_slots(0, 0b1)[0], Some(0));
}

#[test]
fn running_out_of_memory_steps_down_through_the_factors() {
    assert_eq!(ResolutionScale::lower_factor(8), 6);
    assert_eq!(ResolutionScale::lower_factor(5), 4);
    assert_eq!(ResolutionScale::lower_factor(2), 1);
    assert_eq!(ResolutionScale::lower_factor(1), 1);
}

#[test]
fn the_scale_stays_within_10k() {
    assert_eq!(ResolutionScale::for_tv(8, 1280).factor, 8, "10240 x 5760");
    assert_eq!(ResolutionScale::for_tv(8, 1920).factor, 5, "9600 x 5400");
    assert_eq!(ResolutionScale::for_tv(2, 1920).factor, 2);
    assert_eq!(ResolutionScale::for_tv(1, 1280).factor, 1);
}

#[test]
fn pictures_grow_and_shadow_maps_do_not() {
    let scale = ResolutionScale::for_tv(4, 1280);
    assert_eq!(scale.of(&target(1280, 720)), 4);
    assert_eq!(scale.of(&target(640, 360)), 4, "half-size effects keep their proportion");
    assert_eq!(scale.of(&target(1024, 1024)), 1, "square: a shadow map");
    assert_eq!(scale.of(&target(2048, 1024)), 1, "wider than the TV");
    assert_eq!(scale.of(&target(16, 9)), 1, "a tiny target holds data");
    let cascades = SurfaceKey { layers: 4, ..target(1280, 720) };
    assert_eq!(scale.of(&cascades), 1);
    assert_eq!(ResolutionScale::default().of(&target(1280, 720)), 1);
}

#[test]
fn the_renderer_hears_of_the_scale_once_the_tv_is_known() {
    let mut latte = Latte::default();
    let scale_commands = |latte: &Latte| -> Vec<ResolutionScale> {
        latte
            .commands
            .iter()
            .filter_map(|c| match c {
                RenderCommand::SetResolution(scale) => Some(*scale),
                _ => None,
            })
            .collect()
    };
    latte.set_resolution_factor(4);
    assert!(scale_commands(&latte).is_empty(), "no picture yet");
    let tv = SurfaceWords { width: 1280, height: 720, ..SurfaceWords::default() };
    latte.copy_to_scan(&tv, crate::Screen::Tv, false);
    latte.copy_to_scan(&tv, crate::Screen::Tv, false);
    assert_eq!(scale_commands(&latte), [ResolutionScale { factor: 4, limit_width: 1280 }]);
    latte.set_resolution_factor(1);
    assert_eq!(scale_commands(&latte).len(), 2);
}

/// Intent: a graphic pack's resolution rule for the TV-sized target sets the scale, and
/// the user's own factor still wins when larger.
/// Pass: 1280 -> 2560 gives factor 2; asking for 3 gives 3; dropping the rules gives 1.
#[test]
fn a_resolution_pack_sets_the_scale() {
    let mut latte = Latte::default();
    let tv = SurfaceWords { width: 1280, height: 720, ..SurfaceWords::default() };
    latte.copy_to_scan(&tv, crate::Screen::Tv, false);
    let rule = wiiu_graphic_pack::TextureRule {
        width: 1280,
        height: 720,
        overwrite_width: 2560,
        overwrite_height: 1440,
        ..wiiu_graphic_pack::TextureRule::default()
    };
    latte.set_texture_rules(vec![rule]);
    assert_eq!(latte.resolution.factor, 2);
    latte.set_resolution_factor(3);
    assert_eq!(latte.resolution.factor, 3);
    latte.set_resolution_factor(1);
    latte.set_texture_rules(Vec::new());
    assert_eq!(latte.resolution.factor, 1);
}

#[test]
fn depth_read_as_a_texture_keeps_its_precision() {
    assert_eq!(host_format(0x011), HostFormat::R32Float);
    assert_eq!(host_format(0x811), HostFormat::R32Float);
    assert_eq!(host_depth_format(0x011), HostFormat::Depth24PlusStencil8);
}

#[test]
fn a_program_is_translated_once_whatever_earlier_draws_left_in_the_registers() {
    let mut latte = Latte::default();
    let mut regs = RegisterFile::default();
    let code: Arc<[u8]> = Arc::from(vec![0_u8; 64]);
    let program = |latte: &mut Latte, regs: &RegisterFile| {
        latte.program_pair(
            regs,
            (1, Arc::clone(&code)),
            (2, Arc::clone(&code)),
            (3, Arc::clone(&code)),
        )
    };
    let first = program(&mut latte, &regs);

    // A texture unit, an input and a semantic these shaders do not use.
    let unit = base::RESOURCE + resource::PS_TEXTURE;
    regs.set(unit + 2, 0x100);
    regs.set(unit + 4, u32::MAX);
    regs.set(reg::SPI_PS_INPUT_CNTL_0 + 5, 0x1234);
    regs.set(reg::SQ_VTX_SEMANTIC_0 + 7, 9);
    assert!(Arc::ptr_eq(&first, &program(&mut latte, &regs)));

    // An input in use does make another program.
    regs.set(reg::SPI_PS_IN_CONTROL_0, 1);
    assert!(!Arc::ptr_eq(&first, &program(&mut latte, &regs)));
}
