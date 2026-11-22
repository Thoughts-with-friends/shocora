//! Part of [`super`]: see its documentation.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ shaders, buffers,  │   │ gx2/draw/shaders_texture │   │ draw commands      │
//! │ draws              │   │ s.rs                     │   │                    │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ drawing: shaders         │   │                    │
//! │                    │   │ textures                 │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "shaders textures" share of draw.rs (drawing), moved here so that no file is longer than
//! 500 lines. It works exactly as if it were still written in that file. Binds shaders, vertex
//! buffers and textures, and issues the draw calls that make up a frame. Terms such as HLE, JIT
//! or shader are explained in `docs/guide/wiiu_term.md`.

use super::*;

#[hle_export(lib = "gx2", name = "GX2SetPixelShader")]
pub(super) fn gx2_set_pixel_shader(ctx: &mut HleContext<'_>, shader: u32) -> Result<(), HleError> {
    use pixel_shader as f;
    let (address, size) = program(ctx, shader, f::PROGRAM, f::SIZE, f::R_BUFFER)?;
    let read = |o| ctx.memory.read_u32(shader + o);
    let resources = read(f::RESOURCES)?;
    let in_control = [read(f::IN_CONTROL)?, read(f::IN_CONTROL + 4)?];
    let count = read(f::INPUT_COUNT)?.min(32);
    let inputs = (0..count).map(|i| read(f::INPUTS + 4 * i)).collect::<Result<Vec<_>, _>>()?;
    let (mask, control, db_control, input_z) = (
        read(f::SHADER_MASK)?,
        read(f::SHADER_CONTROL)?,
        read(f::DB_SHADER_CONTROL)?,
        read(f::INPUT_Z)?,
    );
    let gx2 = &mut ctx.os.gx2;
    gx2.set_context(
        reg::SQ_PGM_START_PS,
        &[physical(address) >> 8, size >> 3, PROGRAM_RING_SIZE, PROGRAM_RING_SIZE, resources],
    );
    gx2.set_context(reg::SPI_PS_IN_CONTROL_0, &in_control);
    if !inputs.is_empty() {
        gx2.set_context(reg::SPI_PS_INPUT_CNTL_0, &inputs);
    }
    gx2.set_context(reg::CB_SHADER_MASK, &[mask]);
    gx2.set_context(reg::CB_SHADER_CONTROL, &[control]);
    gx2.set_context(reg::DB_SHADER_CONTROL, &[db_control]);
    gx2.set_context(reg::SPI_INPUT_Z, &[input_z]);
    Ok(())
}

/// Geometry shaders run as vertex pipeline substitutes host APIs lack; the
/// executor skips draws while `VGT_GS_MODE` selects them.
#[hle_export(lib = "gx2", name = "GX2SetGeometryShader")]
pub(super) fn gx2_set_geometry_shader(
    ctx: &mut HleContext<'_>,
    shader: u32,
) -> Result<(), HleError> {
    let mode = ctx.memory.read_u32(shader + 8)?;
    ctx.os.gx2.set_context(reg::VGT_GS_MODE, &[mode]);
    Ok(())
}

/// `VGT_GS_MODE` follows the shader mode unless geometry shaders set it themselves.
pub(crate) fn set_shader_mode(gx2: &mut Gx2State, mode: u32) {
    if mode != SHADER_MODE_GEOMETRY {
        gx2.set_context(reg::VGT_GS_MODE, &[0]);
    }
    // DX9_CONSTS: uniforms come from registers instead of blocks.
    gx2.set_config(config::SQ_CONFIG, &[u32::from(mode == 0) << 8]);
}

#[hle_export(lib = "gx2", name = "GX2SetShaderModeEx")]
pub(super) fn gx2_set_shader_mode_ex(
    ctx: &mut HleContext<'_>,
    mode: u32,
    _vs_gprs: u32,
    _vs_stack: u32,
    _gs_gprs: u32,
    _gs_stack: u32,
    _ps_gprs: u32,
    _ps_stack: u32,
) {
    set_shader_mode(&mut ctx.os.gx2, mode);
}

#[hle_export(lib = "gx2", name = "GX2SetShaderMode")]
pub(super) fn gx2_set_shader_mode(ctx: &mut HleContext<'_>, mode: u32) {
    set_shader_mode(&mut ctx.os.gx2, mode);
}

#[hle_export(lib = "gx2", name = "GX2SetStreamOutBuffer")]
pub(super) fn gx2_set_stream_out_buffer(
    ctx: &mut HleContext<'_>,
    index: u32,
    buffer: u32,
) -> Result<(), HleError> {
    if index >= STREAM_OUT_BUFFERS {
        return Ok(());
    }
    let data = ctx.memory.read_u32(buffer + stream_out::DATA)?;
    let (address, size) = if data == 0 {
        let r = read_rbuffer(ctx, buffer + stream_out::R_BUFFER)?;
        (r.address, r.size)
    } else {
        (data, ctx.memory.read_u32(buffer + stream_out::SIZE)?)
    };
    ctx.os.gx2.set_context(reg::VGT_STRMOUT_BUFFER_SIZE_0 + index * 4, &[size >> 2]);
    ctx.os.gx2.set_context(reg::VGT_STRMOUT_BUFFER_BASE_0 + index * 4, &[physical(address) >> 8]);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2RSetStreamOutBuffer")]
pub(super) fn gx2r_set_stream_out_buffer(
    ctx: &mut HleContext<'_>,
    index: u32,
    buffer: u32,
) -> Result<(), HleError> {
    gx2_set_stream_out_buffer(ctx, index, buffer)
}

#[hle_export(lib = "gx2", name = "GX2SetStreamOutEnable")]
pub(super) fn gx2_set_stream_out_enable(ctx: &mut HleContext<'_>, enable: u32) {
    ctx.os.gx2.set_context(reg::VGT_STRMOUT_EN, &[enable & 1]);
}

/// `mode` 0 continues from the offset saved in the buffer's context, 1 starts over.
#[hle_export(lib = "gx2", name = "GX2SetStreamOutContext")]
pub(super) fn gx2_set_stream_out_context(
    ctx: &mut HleContext<'_>,
    index: u32,
    buffer: u32,
    mode: u32,
) -> Result<(), HleError> {
    if index >= STREAM_OUT_BUFFERS || mode > 1 {
        return Ok(());
    }
    let context = ctx.memory.read_u32(buffer + stream_out::CONTEXT)?;
    let source = if mode == 0 { 2 << 1 } else { 0 };
    ctx.os
        .gx2
        .emit(opcode::STRMOUT_BUFFER_UPDATE, &[source | (index << 8), 0, 0, physical(context), 0]);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SaveStreamOutContext")]
pub(super) fn gx2_save_stream_out_context(
    ctx: &mut HleContext<'_>,
    index: u32,
    buffer: u32,
) -> Result<(), HleError> {
    if index >= STREAM_OUT_BUFFERS {
        return Ok(());
    }
    let context = ctx.memory.read_u32(buffer + stream_out::CONTEXT)?;
    ctx.os.gx2.emit(
        opcode::STRMOUT_BUFFER_UPDATE,
        &[1 | (3 << 1) | (index << 8), physical(context), 0, 0, 0],
    );
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2InitTextureRegs")]
pub(crate) fn gx2_init_texture_regs(ctx: &mut HleContext<'_>, tex: u32) -> Result<(), HleError> {
    let mut surface = Gx2Surface::read(ctx.memory, tex)?;
    surface.height = surface.height.max(1);
    surface.depth = surface.depth.max(1);
    surface.num_levels = surface.num_levels.max(1);
    surface.write(ctx.memory, tex)?;
    let read = |o| ctx.memory.read_u32(tex + o);
    let view_num_mips = read(texture::VIEW_NUM_MIPS)?.max(1);
    let view_num_slices = read(texture::VIEW_NUM_SLICES)?.max(1);
    let view_first_mip = read(texture::VIEW_FIRST_MIP)?;
    let view_first_slice = read(texture::VIEW_FIRST_SLICE)?;
    let comp_sel = read(texture::COMP_SEL)?;
    let format = surface.format;
    let hw_format = format & 0x3F;

    let pixel_pitch =
        if addrlib::is_compressed(hw_format) { surface.pitch * 4 } else { surface.pitch };
    let pitch = if pixel_pitch == 0 { 0x7FF } else { (pixel_pitch >> 3) - 1 };
    let width = if surface.width == 0 { 0x1FFF } else { surface.width - 1 };
    let word0 = (surface.dim & 7)
        | ((surface.tile_mode & 0xF) << 3)
        | (u32::from(surface.resource_flags & 4 != 0) << 7)
        | ((pitch & 0x7FF) << 8)
        | ((width & 0x1FFF) << 19);

    let depth = match surface.dim {
        DIM_CUBEMAP => surface.depth / 6 - 1,
        DIM_3D | DIM_2D_ARRAY_MSAA | DIM_2D_ARRAY | DIM_1D_ARRAY => surface.depth - 1,
        _ => 0,
    };
    let word1 = ((surface.height - 1) & 0x1FFF) | ((depth & 0x1FFF) << 13) | (hw_format << 26);

    let component = u32::from(format & FORMAT_BIT_SIGNED != 0);
    let num_format = if format & FORMAT_BIT_FLOAT != 0 {
        NUM_FORMAT_SCALED
    } else if format & FORMAT_BIT_INT != 0 {
        NUM_FORMAT_INT
    } else {
        0
    };
    let word4 = component
        | (component << 2)
        | (component << 4)
        | (component << 6)
        | (num_format << 8)
        | (u32::from(format & FORMAT_BIT_SRGB != 0) << 11)
        | (2 << 14)
        | (((comp_sel >> 24) & 7) << 16)
        | (((comp_sel >> 16) & 7) << 19)
        | (((comp_sel >> 8) & 7) << 22)
        | ((comp_sel & 7) << 25)
        | ((view_first_mip & 0xF) << 28);

    let last_level =
        if (1..=3).contains(&surface.aa) { surface.aa } else { view_first_mip + view_num_mips - 1 };
    let word5 = (last_level & 0xF)
        | ((view_first_slice & 0x1FFF) << 4)
        | (((view_first_slice + view_num_slices - 1) & 0x1FFF) << 17);
    // MAX_ANISO 4, PERF_MODULATION 7, TYPE = valid texture.
    let word6 = (4 << 2) | (7 << 5) | (2 << 30);

    ctx.memory.write_u32(tex + texture::VIEW_NUM_MIPS, view_num_mips)?;
    ctx.memory.write_u32(tex + texture::VIEW_NUM_SLICES, view_num_slices)?;
    for (i, word) in [word0, word1, word4, word5, word6].into_iter().enumerate() {
        ctx.memory.write_u32(tex + texture::WORD0 + 4 * i as u32, word)?;
    }
    Ok(())
}

pub(super) fn set_texture(
    ctx: &mut HleContext<'_>,
    tex: u32,
    unit_base: u32,
    unit: u32,
) -> Result<(), HleError> {
    let surface = Gx2Surface::read(ctx.memory, tex)?;
    let mut image = surface.image;
    let mut mip = if surface.mip_ptr == 0 { image } else { surface.mip_ptr };
    if addrlib::tile_mode::is_macro_tiled(surface.tile_mode) {
        let stop_level = (surface.swizzle >> 16) & 0xFF;
        if stop_level > 0 {
            image ^= surface.swizzle & 0xFFFF;
        }
        if stop_level > 1 {
            mip ^= surface.swizzle & 0xFFFF;
        }
    }
    let read = |i: u32| ctx.memory.read_u32(tex + texture::WORD0 + 4 * i);
    let (word0, word1, word4, word5, word6) = (read(0)?, read(1)?, read(2)?, read(3)?, read(4)?);
    ctx.os.gx2.emit(
        opcode::SET_RESOURCE,
        &[
            unit_base + unit * 7,
            word0,
            word1,
            physical(image) >> 8,
            physical(mip) >> 8,
            word4,
            word5,
            word6,
        ],
    );
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetPixelTexture")]
pub(super) fn gx2_set_pixel_texture(
    ctx: &mut HleContext<'_>,
    tex: u32,
    unit: u32,
) -> Result<(), HleError> {
    set_texture(ctx, tex, resource::PS_TEXTURE, unit)
}

#[hle_export(lib = "gx2", name = "GX2SetVertexTexture")]
pub(crate) fn gx2_set_vertex_texture(
    ctx: &mut HleContext<'_>,
    tex: u32,
    unit: u32,
) -> Result<(), HleError> {
    set_texture(ctx, tex, resource::VS_TEXTURE, unit)
}

#[hle_export(lib = "gx2", name = "GX2SetGeometryTexture")]
pub(super) fn gx2_set_geometry_texture(
    ctx: &mut HleContext<'_>,
    tex: u32,
    unit: u32,
) -> Result<(), HleError> {
    set_texture(ctx, tex, resource::GS_TEXTURE, unit)
}

pub(super) fn set_sampler(
    ctx: &mut HleContext<'_>,
    sampler: u32,
    index: u32,
) -> Result<(), HleError> {
    let read = |i: u32| ctx.memory.read_u32(sampler + 4 * i);
    let words = [index * 3, read(0)?, read(1)?, read(2)?];
    ctx.os.gx2.emit(opcode::SET_SAMPLER, &words);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetPixelSampler")]
pub(super) fn gx2_set_pixel_sampler(
    ctx: &mut HleContext<'_>,
    sampler: u32,
    unit: u32,
) -> Result<(), HleError> {
    set_sampler(ctx, sampler, unit + SAMPLER_BASE_PIXEL)
}

#[hle_export(lib = "gx2", name = "GX2SetVertexSampler")]
pub(crate) fn gx2_set_vertex_sampler(
    ctx: &mut HleContext<'_>,
    sampler: u32,
    unit: u32,
) -> Result<(), HleError> {
    set_sampler(ctx, sampler, unit + SAMPLER_BASE_VERTEX)
}

#[hle_export(lib = "gx2", name = "GX2SetGeometrySampler")]
pub(super) fn gx2_set_geometry_sampler(
    ctx: &mut HleContext<'_>,
    sampler: u32,
    unit: u32,
) -> Result<(), HleError> {
    set_sampler(ctx, sampler, unit + SAMPLER_BASE_GEOMETRY)
}

pub(crate) fn set_border_color(gx2: &mut Gx2State, register: u32, unit: u32, color: [f32; 4]) {
    gx2.emit(
        opcode::SET_CONFIG_REG,
        &[
            register + unit * 4 - base::CONFIG,
            color[0].to_bits(),
            color[1].to_bits(),
            color[2].to_bits(),
            color[3].to_bits(),
        ],
    );
}

#[hle_export(lib = "gx2", name = "GX2SetPixelSamplerBorderColor")]
pub(super) fn gx2_set_pixel_sampler_border_color(
    ctx: &mut HleContext<'_>,
    unit: u32,
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
) {
    set_border_color(
        &mut ctx.os.gx2,
        config::TD_PS_SAMPLER0_BORDER_RED,
        unit,
        [red, green, blue, alpha],
    );
}

#[hle_export(lib = "gx2", name = "GX2SetVertexSamplerBorderColor")]
pub(super) fn gx2_set_vertex_sampler_border_color(
    ctx: &mut HleContext<'_>,
    unit: u32,
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
) {
    set_border_color(
        &mut ctx.os.gx2,
        config::TD_VS_SAMPLER0_BORDER_RED,
        unit,
        [red, green, blue, alpha],
    );
}
