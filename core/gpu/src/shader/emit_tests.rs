//! Tests of the R600 → WGSL translator. `emit.rs` is over 500 lines, so they live here.
use super::{PixelInputs, translate_pixel, translate_vertex};
use crate::shader::{TEXTURE_UNITS, TextureKind, TextureSignature};

/// `CF_INST_EXPORT_DONE`, with end of program.
const EXPORT_DONE: u32 = (0x28 << 23) | (1 << 21);
/// Export swizzle `xyzw`.
const XYZW: u32 = (1 << 3) | (2 << 6) | (3 << 9);

/// A control-flow program of one export of GPR 0, as the GPU reads it (little-endian).
fn export_gpr0(kind: u32, array_base: u32) -> Vec<u8> {
    let word0 = array_base | (kind << 13);
    [word0, EXPORT_DONE | XYZW].iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn validate(wgsl: &str) {
    let module = naga::front::wgsl::parse_str(wgsl).unwrap_or_else(|e| panic!("{e}\n{wgsl}"));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{e:?}\n{wgsl}"));
}

/// Intent: the smallest pixel shader, "write interpolated input 0 to color target 0",
/// becomes WGSL that the host's shader compiler accepts.
/// Pass: naga parses and validates the output, and target 0 is marked as written.
#[test]
fn passthrough_pixel_shader_is_valid_wgsl() {
    let textures = [TextureSignature::default(); TEXTURE_UNITS];
    let inputs = PixelInputs {
        control0: 1,
        control1: 0,
        input_controls: &[0],
        vs_out_semantics: &[0],
        vs_exports: 1,
        alpha_function: None,
        target_kinds: [TextureKind::default(); 8],
    };
    let (shader, _flat) = translate_pixel(&export_gpr0(0, 0), &inputs, &textures);
    assert_eq!(shader.color_exports & 1, 1);
    assert!(!shader.writes_depth);
    validate(&shader.wgsl);
}

/// Intent: the smallest vertex shader, "GPR 0 is the position", becomes valid WGSL.
/// Pass: naga parses and validates the output; nothing is sampled.
#[test]
fn position_only_vertex_shader_is_valid_wgsl() {
    let textures = [TextureSignature::default(); TEXTURE_UNITS];
    // Position exports start at array base 60.
    let shader = translate_vertex(&export_gpr0(1, 60), &[], &[], &textures, 0);
    assert!(shader.samplers.is_empty() && shader.textures.is_empty());
    validate(&shader.wgsl);
}
