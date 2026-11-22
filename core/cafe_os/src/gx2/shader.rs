//! Fetch shaders (R600 microcode generated on the CPU) and samplers.
//! Ported from Cemu's GX2_Shader / GX2_Texture.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ vertex layouts,    │   │ gx2/shader.rs            │   │ GPU programs       │
//! │ samplers           │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ fetch shaders, samplers  │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Builds the small GPU programs that read vertex data, and describes how textures are sampled.
//! Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
#![allow(clippy::cast_possible_truncation)]

use wiiu_hle_macros::hle_export;

use crate::hle::{HleContext, HleError};

/// CF opcodes.
const CF_INST_VTX_TC: u32 = 0x03;
const CF_INST_RETURN: u32 = 0x14;
/// `VTX_INST_SEMANTIC`.
const VTX_INST_SEMANTIC: u32 = 1;
/// `GX2EndianSwapMode::DEFAULT`.
const ENDIAN_SWAP_DEFAULT: u32 = 3;

/// `GX2AttribFormat` (low 6 bits) → hardware fetch format.
const FETCH_FORMAT: [u32; 20] = [
    1, 2, 5, 6, 7, 0xD, 0xE, 0xF, 0x10, 0x16, 0x1A, 0x19, 0x1D, 0x1E, 0x1F, 0x20, 0x2F, 0x30, 0x22,
    0x23,
];

/// Per-format default endian swap (0 none, 1 16-bit, 2 32-bit).
const fn default_endian_swap(format: u32) -> u32 {
    match format {
        2 | 3 | 7 | 8 | 14 | 15 => 1,
        5 | 6 | 9 | 11 | 12 | 13 | 16..=19 => 2,
        _ => 0,
    }
}

/// Bytes of CF code: one `VTX_TC` per 16 attributes plus `RETURN`, 16-byte aligned.
const fn cf_size(attributes: u32) -> u32 {
    ((attributes.div_ceil(16) + 1) * 8).next_multiple_of(16)
}

/// Microcode is little-endian in guest memory.
fn write_le(ctx: &mut HleContext<'_>, address: u32, words: &[u32]) -> Result<(), HleError> {
    for (i, word) in words.iter().enumerate() {
        ctx.memory.write_u32(address + 4 * i as u32, word.swap_bytes())?;
    }
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2CalcFetchShaderSizeEx")]
fn gx2_calc_fetch_shader_size_ex(
    _ctx: &mut HleContext<'_>,
    attributes: u32,
    _kind: u32,
    _tessellation: u32,
) -> u32 {
    cf_size(attributes) + attributes * 16
}

/// `GX2InitFetchShaderEx(fetchShader, program, count, attribs, type, tessellation)`.
#[hle_export(lib = "gx2", name = "GX2InitFetchShaderEx")]
fn gx2_init_fetch_shader_ex(
    ctx: &mut HleContext<'_>,
    shader: u32,
    program: u32,
    attributes: u32,
    descriptions: u32,
    _kind: u32,
    _tessellation: u32,
) -> Result<(), HleError> {
    ctx.memory.fill(shader, 0x20, 0)?;
    let cf_bytes = cf_size(attributes);

    // CF program.
    let mut cf = Vec::new();
    let mut index = 0;
    while index < attributes {
        let count = (attributes - index).min(16) - 1;
        let word1 = (CF_INST_VTX_TC << 23) | ((count & 7) << 10) | ((count << 16) & (1 << 19));
        cf.extend([(cf_bytes + index * 16) >> 3, word1]);
        index += 16;
    }
    cf.extend([0, (CF_INST_RETURN << 23) | (1 << 31)]);
    write_le(ctx, program, &cf)?;

    // One VTX clause instruction per attribute.
    let mut divisors: Vec<u32> = Vec::new();
    for i in 0..attributes {
        let desc = descriptions + i * 0x20;
        let field = |o| ctx.memory.read_u32(desc + o);
        let (location, buffer, offset, format) =
            (field(0x00)?, field(0x04)?, field(0x08)?, field(0x0C)?);
        let (index_type, divisor, dest_sel, mut endian) =
            (field(0x10)?, field(0x14)?, field(0x18)?, field(0x1C)?);
        if endian == ENDIAN_SWAP_DEFAULT {
            endian = default_endian_swap(format & 0x3F);
        }
        let src_sel_x = match (index_type, divisor) {
            (0, _) => 0,
            (_, 1) => 3,
            (_, d) => match divisors.iter().position(|&x| x == d) {
                Some(p) => {
                    if p == 0 {
                        1
                    } else {
                        2
                    }
                }
                None if divisors.len() < 2 => {
                    divisors.push(d);
                    if divisors.len() == 1 { 1 } else { 2 }
                }
                None => 1,
            },
        };
        let fetch_format = FETCH_FORMAT.get((format & 0x3F) as usize).copied().unwrap_or(0) & 0x3F;
        let num_format = if format & 0x800 != 0 {
            2
        } else if format & 0x100 != 0 {
            1
        } else {
            0
        };
        let word0 = VTX_INST_SEMANTIC
            | ((index_type & 3) << 5)
            | (((buffer + 0xA0) & 0xFF) << 8)
            | (src_sel_x << 24);
        let mut word1 = (location & 0xFF) | (fetch_format << 22) | (num_format << 28);
        if format & 0x200 != 0 {
            word1 |= 1 << 30;
        }
        for (component, shift) in [(0, 24), (1, 16), (2, 8), (3, 0)] {
            word1 |= ((dest_sel >> shift) & 7) << (9 + component * 3);
        }
        let word2 = (offset & 0xFFFF) | ((endian & 3) << 16);
        write_le(ctx, program + cf_bytes + i * 16, &[word0, word1, word2, 0])?;
    }

    // GX2FetchShader
    ctx.memory.write_u32(shader + 0x04, 2)?; // SQ_PGM_RESOURCES_FS: NUM_GPRS = 2
    ctx.memory.write_u32(shader + 0x08, cf_bytes + attributes * 16)?;
    ctx.memory.write_u32(shader + 0x0C, program)?;
    ctx.memory.write_u32(shader + 0x10, attributes)?;
    #[allow(clippy::cast_possible_truncation)]
    ctx.memory.write_u32(shader + 0x14, divisors.len() as u32)?;
    for (i, d) in divisors.iter().enumerate() {
        ctx.memory.write_u32(shader + 0x18 + 4 * i as u32, *d)?;
    }
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2TempGetGPUVersion")]
fn gx2_temp_get_gpu_version(_ctx: &mut HleContext<'_>) -> u32 {
    2
}

/// `GX2Sampler`: three `SQ_TEX_SAMPLER_WORD*` registers.
fn update_sampler_word(
    ctx: &mut HleContext<'_>,
    sampler: u32,
    word: u32,
    mask: u32,
    value: u32,
) -> Result<(), HleError> {
    let address = sampler + 4 * word;
    let current = ctx.memory.read_u32(address)?;
    Ok(ctx.memory.write_u32(address, (current & !mask) | (value & mask))?)
}

#[hle_export(lib = "gx2", name = "GX2InitSampler")]
fn gx2_init_sampler(
    ctx: &mut HleContext<'_>,
    sampler: u32,
    clamp: u32,
    filter: u32,
) -> Result<(), HleError> {
    let clamp = clamp & 7;
    let filter = filter & 7;
    // Z/MIP filter POINT (1), TEX_ARRAY_OVERRIDE.
    let word0 = clamp
        | (clamp << 3)
        | (clamp << 6)
        | (filter << 9)
        | (filter << 12)
        | (1 << 15)
        | (1 << 17)
        | (1 << 25);
    ctx.memory.write_u32(sampler, word0)?;
    ctx.memory.write_u32(sampler + 4, 0x3FF << 10)?; // MAX_LOD
    ctx.memory.write_u32(sampler + 8, 1 << 31)?; // TYPE
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2InitSamplerZMFilter")]
fn gx2_init_sampler_zm_filter(
    ctx: &mut HleContext<'_>,
    sampler: u32,
    z: u32,
    mip: u32,
) -> Result<(), HleError> {
    update_sampler_word(ctx, sampler, 0, (3 << 15) | (3 << 17), ((z & 3) << 15) | ((mip & 3) << 17))
}

#[hle_export(lib = "gx2", name = "GX2InitSamplerXYFilter")]
fn gx2_init_sampler_xy_filter(
    ctx: &mut HleContext<'_>,
    sampler: u32,
    mag: u32,
    min: u32,
    aniso: u32,
) -> Result<(), HleError> {
    let (mag, min) = if aniso == 0 { (mag & 7, min & 7) } else { ((mag & 7) | 4, (min & 7) | 4) };
    let value = (mag << 9) | (min << 12) | ((aniso & 7) << 19);
    update_sampler_word(ctx, sampler, 0, (7 << 9) | (7 << 12) | (7 << 19), value)
}

#[hle_export(lib = "gx2", name = "GX2InitSamplerClamping")]
fn gx2_init_sampler_clamping(
    ctx: &mut HleContext<'_>,
    sampler: u32,
    x: u32,
    y: u32,
    z: u32,
) -> Result<(), HleError> {
    update_sampler_word(ctx, sampler, 0, 0x1FF, (x & 7) | ((y & 7) << 3) | ((z & 7) << 6))
}

#[hle_export(lib = "gx2", name = "GX2InitSamplerBorderType")]
fn gx2_init_sampler_border_type(
    ctx: &mut HleContext<'_>,
    sampler: u32,
    border: u32,
) -> Result<(), HleError> {
    update_sampler_word(ctx, sampler, 0, 3 << 22, (border & 3) << 22)
}

#[hle_export(lib = "gx2", name = "GX2InitSamplerDepthCompare")]
fn gx2_init_sampler_depth_compare(
    ctx: &mut HleContext<'_>,
    sampler: u32,
    function: u32,
) -> Result<(), HleError> {
    update_sampler_word(ctx, sampler, 0, 7 << 26, (function & 7) << 26)
}

#[hle_export(lib = "gx2", name = "GX2InitSamplerLOD")]
#[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
fn gx2_init_sampler_lod(
    ctx: &mut HleContext<'_>,
    sampler: u32,
    min: f32,
    max: f32,
    bias: f32,
) -> Result<(), HleError> {
    let to_fixed = |v: f32| ((v.clamp(0.0, 16.0) * 64.0).floor() as u32).min(1023);
    let bias = ((bias * 64.0).floor() as i32 & 0xFFF) as u32;
    ctx.memory.write_u32(sampler + 4, to_fixed(min) | (to_fixed(max) << 10) | (bias << 20))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn fetch_shader_layout_matches_cemu() {
        let mut env = TestEnv::new();
        assert_eq!(env.call("gx2", "GX2CalcFetchShaderSizeEx", &[3, 0, 0]), 16 + 48);
        let (shader, program, attribs) = (SCRATCH, SCRATCH + 0x100, SCRATCH + 0x200);
        // attribute 0: location 0, buffer 1, offset 8, format 0x80D (32_32_32 float), dest sel xyzw
        for (offset, value) in
            [(0x00, 0), (0x04, 1), (0x08, 8), (0x0C, 0x80D), (0x18, 0x0001_0203), (0x1C, 3)]
        {
            env.memory.write_u32(attribs + offset, value).unwrap();
        }
        env.call("gx2", "GX2InitFetchShaderEx", &[shader, program, 1, attribs, 0, 0]);
        assert_eq!(env.memory.read_u32(shader + 0x08).unwrap(), 32);
        // CF[0]: VTX_TC, ADDR = 16 bytes / 8
        assert_eq!(env.memory.read_u32(program).unwrap().swap_bytes(), 2);
        assert_eq!(env.memory.read_u32(program + 4).unwrap().swap_bytes() >> 23, 0x03);
        // CF[1]: RETURN with barrier
        assert_eq!(
            env.memory.read_u32(program + 12).unwrap().swap_bytes(),
            (0x14 << 23) | (1 << 31)
        );
        // VTX word0: semantic fetch from buffer 0xA1
        assert_eq!(env.memory.read_u32(program + 16).unwrap().swap_bytes(), 1 | (0xA1 << 8));
    }
}
