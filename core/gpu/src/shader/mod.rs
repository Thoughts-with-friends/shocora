//! Latte (R600-family) shader programs → WGSL.
//!
//! ```text
//!  guest program ─► cf::parse ─► emit::Translator ─► WGSL source
//!  fetch shader ──► fetch::parse ─┘ (vertex stage: attributes pulled from the arena)
//! ```
//!
//! Registers keep raw bits (`vec4<u32>`), which matches the hardware: an
//! instruction decides how to read them. Control flow uses the same execution
//! mask stack the hardware has, evaluated per invocation.
mod cf;
mod emit;
pub mod fetch;

pub use emit::{
    PixelInputs, RECT_CORNER_FLAG, RECT_CORNER_SHIFT, RECT_ENTRY_POINT, translate_pixel,
    translate_vertex,
};
pub use fetch::{Attribute, parse_fetch_shader};

/// `E_DIM` as far as shader code is concerned.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextureDim {
    #[default]
    D2,
    D2Array,
    D3,
    Cube,
}

/// How the texture is declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextureKind {
    #[default]
    Float,
    /// A depth surface: comparisons work, filtering does not.
    Depth,
    Uint,
    Sint,
}

/// A texture binding as far as generated code depends on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TextureSignature {
    pub dim: TextureDim,
    pub kind: TextureKind,
    /// `DST_SEL_X/Y/Z/W` of `SQ_TEX_RESOURCE_WORD4`.
    pub comp_sel: [u8; 4],
    /// The unit is used with a comparison sampler.
    pub comparison: bool,
    /// `DEPTH_COMPARE_FUNCTION` of the sampler (`0` never ... `7` always): what a
    /// comparing fetch does when the host cannot compare (not a depth surface).
    pub compare_function: u8,
}

/// Texture units of one stage.
pub const TEXTURE_UNITS: usize = 18;

/// What the generated code binds besides the arena, by stage.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TranslatedShader {
    pub wgsl: String,
    /// `(texture unit, sampler unit)` pairs the program samples.
    pub samplers: Vec<(u8, u8)>,
    /// Texture units accessed.
    pub textures: Vec<u8>,
    /// Pixel shaders: color targets written, as a bit mask.
    pub color_exports: u8,
    pub writes_depth: bool,
    /// Writes stream-out buffers.
    pub stream_out: bool,
}

/// `group(0)`: arena and per-draw parameters, shared by every shader.
pub const WGSL_PRELUDE: &str = "\
diagnostic(off, derivative_uniformity);
struct DrawParams {
    vs_const: u32,
    ps_const: u32,
    alpha_ref: f32,
    y_sign: f32,
    half_z: u32,
    divisor0: u32,
    divisor1: u32,
    base_instance: u32,
    vs_ubo: array<u32, 16>,
    ps_ubo: array<u32, 16>,
    attr_base: array<u32, 16>,
    attr_stride: array<u32, 16>,
    so_base: array<u32, 4>,
    so_stride: array<u32, 4>,
    so_instance_vertices: u32,
    res_scale: f32,
    target_scaled: u32,
    vs_scaled: u32,
    ps_scaled: u32,
}
@group(0) @binding(0) var<storage, read> arena: array<u32>;
@group(0) @binding(1) var<storage, read> P: DrawParams;

// How much larger than the title thinks texture `unit` is.
fn tex_scale(mask: u32, unit: u32) -> f32 {
    return select(1.0, P.res_scale, ((mask >> unit) & 1u) != 0u);
}
// A NaN written to a float target would spread through every later blur and
// average of the frame; the render back end keeps colors finite.
fn finite_color(v: vec4<f32>) -> vec4<f32> {
    return clamp(select(v, vec4<f32>(0.0), v != v), vec4<f32>(-1e18), vec4<f32>(1e18));
}
// LOG, RECIP and RECIPSQRT replace the infinities of IEEE arithmetic in their
// `CLAMPED` and `FF` forms: largest float and zero respectively.
fn log_clamped(v: f32) -> f32 {
    return select(max(log2(v), -3.40282347e38), -3.40282347e38, v <= 0.0);
}
fn recip_clamped(v: f32) -> f32 {
    return select(1.0 / v, select(3.40282347e38, -3.40282347e38, (bitcast<u32>(v) >> 31u) != 0u), v == 0.0);
}
fn recip_ff(v: f32) -> f32 {
    return select(1.0 / v, 0.0, v == 0.0);
}
fn recipsqrt_clamped(v: f32) -> f32 {
    return select(1.0 / sqrt(v), 3.40282347e38, v == 0.0);
}
fn recipsqrt_ff(v: f32) -> f32 {
    return select(1.0 / sqrt(v), 0.0, v == 0.0);
}
fn mul_ni(a: f32, b: f32) -> f32 {
    return select(a * b, 0.0, a == 0.0 || b == 0.0);
}
fn f16_bits(v: u32) -> f32 {
    let bits = v & 0xFFFFu;
    let sign = (bits & 0x8000u) << 16u;
    let exponent = (bits >> 10u) & 0x1Fu;
    let mantissa = bits & 0x3FFu;
    if (exponent == 0u) {
        let m = f32(mantissa) / 16777216.0;
        return select(m, -m, sign != 0u);
    }
    if (exponent == 31u) {
        return bitcast<f32>(sign | 0x7F800000u | (mantissa << 13u));
    }
    return bitcast<f32>(sign | ((exponent + 112u) << 23u) | (mantissa << 13u));
}
fn sext(v: u32, bits: u32) -> i32 {
    let shift = 32u - bits;
    return bitcast<i32>(v << shift) >> shift;
}
fn rd8(address: u32) -> u32 {
    return (arena[address >> 2u] >> ((address & 3u) * 8u)) & 0xFFu;
}
fn rd_be16(address: u32) -> u32 {
    return (rd8(address) << 8u) | rd8(address + 1u);
}
fn rd_be32(address: u32) -> u32 {
    return (rd8(address) << 24u) | (rd8(address + 1u) << 16u) | (rd8(address + 2u) << 8u) | rd8(address + 3u);
}
fn rd_le16(address: u32) -> u32 {
    return rd8(address) | (rd8(address + 1u) << 8u);
}
fn rd_le32(address: u32) -> u32 {
    return rd8(address) | (rd8(address + 1u) << 8u) | (rd8(address + 2u) << 16u) | (rd8(address + 3u) << 24u);
}
fn redc_cube(a: vec4<f32>, b: vec4<f32>) -> vec4<f32> {
    let d = vec3<f32>(a.z, a.w, a.x);
    let ad = abs(d);
    if (ad.z >= ad.x && ad.z >= ad.y) {
        return vec4<f32>(-d.y, select(-d.x, d.x, d.z >= 0.0), 2.0 * d.z, select(5.0, 4.0, d.z >= 0.0));
    }
    if (ad.y >= ad.x) {
        return vec4<f32>(select(-d.z, d.z, d.y >= 0.0), d.x, 2.0 * d.y, select(3.0, 2.0, d.y >= 0.0));
    }
    return vec4<f32>(-d.y, select(d.z, -d.z, d.x >= 0.0), 2.0 * d.x, select(1.0, 0.0, d.x >= 0.0));
}
fn cube_direction(st: vec2<f32>, face: i32) -> vec3<f32> {
    let tc = (st - vec2<f32>(1.5)) * 2.0;
    switch face {
        case 0: { return vec3<f32>(1.0, -tc.y, -tc.x); }
        case 1: { return vec3<f32>(-1.0, -tc.y, tc.x); }
        case 2: { return vec3<f32>(tc.x, 1.0, tc.y); }
        case 3: { return vec3<f32>(tc.x, -1.0, -tc.y); }
        case 4: { return vec3<f32>(tc.x, -tc.y, 1.0); }
        default: { return vec3<f32>(-tc.x, -tc.y, -1.0); }
    }
}
";
