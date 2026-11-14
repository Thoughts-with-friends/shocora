//! Shaders, buffers, textures and draw calls (port of Cemu's GX2_Draw,
//! GX2_Shader and GX2_Texture).
#![allow(clippy::cast_possible_truncation, clippy::too_many_arguments)]

use wiiu_gpu::{
    addrlib,
    command::opcode,
    regs::{base, config, context as reg, resource},
};
use wiiu_hle_macros::hle_export;

use super::{Gx2State, physical, surface::Gx2Surface};
use crate::hle::{HleContext, HleError, HleExport};

// ---- Draw calls ------------------------------------------------------------

/// `GX2IndexType::U32_BE`.
const INDEX_U32_BE: u32 = 9;

fn draw_setup(gx2: &mut Gx2State, mode: u32, index_type: u32, base_vertex: u32, instances: u32) {
    gx2.emit(opcode::SET_CTL_CONST, &[0, base_vertex]);
    gx2.set_config(config::VGT_PRIMITIVE_TYPE, &[mode]);
    gx2.emit(opcode::INDEX_TYPE, &[index_type]);
    gx2.emit(opcode::NUM_INSTANCES, &[instances]);
}

#[hle_export(lib = "gx2", name = "GX2DrawEx")]
fn gx2_draw_ex(
    ctx: &mut HleContext<'_>,
    mode: u32,
    count: u32,
    base_vertex: u32,
    instances: u32,
) -> Result<(), HleError> {
    draw_setup(&mut ctx.os.gx2, mode, INDEX_U32_BE, base_vertex, instances);
    ctx.os.gx2.emit(opcode::DRAW_INDEX_AUTO, &[count, 0]);
    super::flush_if_large(ctx)
}

#[hle_export(lib = "gx2", name = "GX2DrawIndexedEx")]
fn gx2_draw_indexed_ex(
    ctx: &mut HleContext<'_>,
    mode: u32,
    count: u32,
    index_type: u32,
    indices: u32,
    base_vertex: u32,
    instances: u32,
) -> Result<(), HleError> {
    draw_setup(&mut ctx.os.gx2, mode, index_type, base_vertex, instances);
    ctx.os.gx2.emit(opcode::DRAW_INDEX_2, &[u32::MAX, physical(indices), 0, count, 0]);
    super::flush_if_large(ctx)
}

#[hle_export(lib = "gx2", name = "GX2DrawIndexedEx2")]
fn gx2_draw_indexed_ex2(
    ctx: &mut HleContext<'_>,
    mode: u32,
    count: u32,
    index_type: u32,
    indices: u32,
    base_vertex: u32,
    instances: u32,
    base_instance: u32,
) -> Result<(), HleError> {
    ctx.os.gx2.emit(opcode::SET_CTL_CONST, &[1, base_instance]);
    draw_setup(&mut ctx.os.gx2, mode, index_type, base_vertex, instances);
    ctx.os.gx2.emit(opcode::DRAW_INDEX_2, &[u32::MAX, physical(indices), 0, count, 0]);
    ctx.os.gx2.emit(opcode::SET_CTL_CONST, &[1, 0]);
    super::flush_if_large(ctx)
}

/// Indices travel inside the packet, in the order the index type implies.
#[hle_export(lib = "gx2", name = "GX2DrawIndexedImmediateEx")]
fn gx2_draw_indexed_immediate_ex(
    ctx: &mut HleContext<'_>,
    mode: u32,
    count: u32,
    index_type: u32,
    indices: u32,
    base_vertex: u32,
    instances: u32,
) -> Result<(), HleError> {
    let is_32_bit = matches!(index_type, 1 | 9);
    let words = if is_32_bit { count } else { count.div_ceil(2) };
    let mut body = vec![count, 0];
    for i in 0..words {
        // Memory holds big-endian halves; the executor expects (first << 16) | second.
        let word = ctx.memory.read_u32(indices + 4 * i)?;
        body.push(word);
    }
    draw_setup(&mut ctx.os.gx2, mode, index_type, base_vertex, instances);
    ctx.os.gx2.emit(opcode::DRAW_INDEX_IMMD, &body);
    super::flush_if_large(ctx)
}

/// `GX2RBuffer` as stored in guest memory.
struct RBuffer {
    size: u32,
    address: u32,
}

fn read_rbuffer(ctx: &HleContext<'_>, buffer: u32) -> Result<RBuffer, HleError> {
    Ok(RBuffer {
        size: ctx.memory.read_u32(buffer + 4)?.wrapping_mul(ctx.memory.read_u32(buffer + 8)?),
        address: ctx.memory.read_u32(buffer + 12)?,
    })
}

fn set_buffer_resource(
    gx2: &mut Gx2State,
    register: u32,
    address: u32,
    size: u32,
    word2: u32,
    word3: u32,
) {
    gx2.emit(
        opcode::SET_RESOURCE,
        &[register, physical(address), size.wrapping_sub(1), word2, word3, 0, 0, 0xC000_0000],
    );
}

#[hle_export(lib = "gx2", name = "GX2SetAttribBuffer")]
fn gx2_set_attrib_buffer(ctx: &mut HleContext<'_>, index: u32, size: u32, stride: u32, data: u32) {
    set_buffer_resource(
        &mut ctx.os.gx2,
        resource::ATTRIBUTE_BLOCK + index * 7,
        data,
        size,
        (stride & 0xFFFF) << 11,
        0,
    );
}

#[hle_export(lib = "gx2", name = "GX2RSetAttributeBuffer")]
fn gx2r_set_attribute_buffer(
    ctx: &mut HleContext<'_>,
    buffer: u32,
    index: u32,
    stride: u32,
    offset: u32,
) -> Result<(), HleError> {
    let r = read_rbuffer(ctx, buffer)?;
    set_buffer_resource(
        &mut ctx.os.gx2,
        resource::ATTRIBUTE_BLOCK + index * 7,
        r.address + offset,
        r.size - offset,
        (stride & 0xFFFF) << 11,
        0,
    );
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetVertexUniformBlock")]
fn gx2_set_vertex_uniform_block(ctx: &mut HleContext<'_>, index: u32, size: u32, data: u32) {
    set_buffer_resource(&mut ctx.os.gx2, resource::VS_UNIFORM_BLOCK + index * 7, data, size, 0, 1);
}

#[hle_export(lib = "gx2", name = "GX2SetPixelUniformBlock")]
fn gx2_set_pixel_uniform_block(ctx: &mut HleContext<'_>, index: u32, size: u32, data: u32) {
    set_buffer_resource(&mut ctx.os.gx2, resource::PS_UNIFORM_BLOCK + index * 7, data, size, 0, 1);
}

#[hle_export(lib = "gx2", name = "GX2SetGeometryUniformBlock")]
fn gx2_set_geometry_uniform_block(ctx: &mut HleContext<'_>, index: u32, size: u32, data: u32) {
    set_buffer_resource(&mut ctx.os.gx2, resource::GS_UNIFORM_BLOCK + index * 7, data, size, 0, 1);
}

#[hle_export(lib = "gx2", name = "GX2RSetVertexUniformBlock")]
fn gx2r_set_vertex_uniform_block(
    ctx: &mut HleContext<'_>,
    buffer: u32,
    index: u32,
    offset: u32,
) -> Result<(), HleError> {
    let r = read_rbuffer(ctx, buffer)?;
    set_buffer_resource(
        &mut ctx.os.gx2,
        resource::VS_UNIFORM_BLOCK + index * 7,
        r.address + offset,
        r.size - offset,
        0,
        1,
    );
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2RSetPixelUniformBlock")]
fn gx2r_set_pixel_uniform_block(
    ctx: &mut HleContext<'_>,
    buffer: u32,
    index: u32,
    offset: u32,
) -> Result<(), HleError> {
    let r = read_rbuffer(ctx, buffer)?;
    set_buffer_resource(
        &mut ctx.os.gx2,
        resource::PS_UNIFORM_BLOCK + index * 7,
        r.address + offset,
        r.size - offset,
        0,
        1,
    );
    Ok(())
}

fn uniform_registers(
    ctx: &mut HleContext<'_>,
    offset: u32,
    count: u32,
    values: u32,
) -> Result<(), HleError> {
    let mut body = Vec::with_capacity(1 + count as usize);
    body.push(offset);
    for i in 0..count & !3 {
        body.push(ctx.memory.read_u32(values + 4 * i)?);
    }
    ctx.os.gx2.emit(opcode::SET_ALU_CONST, &body);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetVertexUniformReg")]
fn gx2_set_vertex_uniform_reg(
    ctx: &mut HleContext<'_>,
    offset: u32,
    count: u32,
    values: u32,
) -> Result<(), HleError> {
    uniform_registers(ctx, 0x400 + offset, count, values)
}

#[hle_export(lib = "gx2", name = "GX2SetPixelUniformReg")]
fn gx2_set_pixel_uniform_reg(
    ctx: &mut HleContext<'_>,
    offset: u32,
    count: u32,
    values: u32,
) -> Result<(), HleError> {
    uniform_registers(ctx, offset, count, values)
}

// ---- Shaders ---------------------------------------------------------------

mod vertex_shader {
    pub const RESOURCES: u32 = 0x00;
    pub const PRIMITIVE_ID_EN: u32 = 0x04;
    pub const OUT_CONFIG: u32 = 0x08;
    pub const OUT_ID_COUNT: u32 = 0x0C;
    pub const OUT_IDS: u32 = 0x10;
    pub const OUT_CNTL: u32 = 0x38;
    pub const SEMANTIC_COUNT: u32 = 0x40;
    pub const SEMANTICS: u32 = 0x44;
    pub const USES_STREAM_OUT: u32 = 0x110;
    pub const STREAM_OUT_STRIDES: u32 = 0x114;
    pub const SIZE: u32 = 0xD0;
    pub const PROGRAM: u32 = 0xD4;
    pub const MODE: u32 = 0xD8;
    pub const R_BUFFER: u32 = 0x124;
}

mod pixel_shader {
    pub const RESOURCES: u32 = 0x00;
    pub const IN_CONTROL: u32 = 0x08;
    pub const INPUT_COUNT: u32 = 0x10;
    pub const INPUTS: u32 = 0x14;
    pub const SHADER_MASK: u32 = 0x94;
    pub const SHADER_CONTROL: u32 = 0x98;
    pub const DB_SHADER_CONTROL: u32 = 0x9C;
    pub const INPUT_Z: u32 = 0xA0;
    pub const SIZE: u32 = 0xA4;
    pub const PROGRAM: u32 = 0xA8;
    pub const R_BUFFER: u32 = 0xD8;
}

/// `GX2_SHADER_MODE::GEOMETRY_SHADER`.
const SHADER_MODE_GEOMETRY: u32 = 2;
/// Value the GX2 library leaves in the two "ring item size" words.
const PROGRAM_RING_SIZE: u32 = 0x10_0000;
const FETCH_RING_SIZE: u32 = 0x1_0000;

fn program(
    ctx: &HleContext<'_>,
    shader: u32,
    pointer: u32,
    size: u32,
    r_buffer: u32,
) -> Result<(u32, u32), HleError> {
    let address = ctx.memory.read_u32(shader + pointer)?;
    if address != 0 {
        return Ok((address, ctx.memory.read_u32(shader + size)?));
    }
    let r = read_rbuffer(ctx, shader + r_buffer)?;
    Ok((r.address, r.size))
}

#[hle_export(lib = "gx2", name = "GX2SetFetchShader")]
fn gx2_set_fetch_shader(ctx: &mut HleContext<'_>, shader: u32) -> Result<(), HleError> {
    let resources = ctx.memory.read_u32(shader + 0x04)?;
    let size = ctx.memory.read_u32(shader + 0x08)?;
    let address = ctx.memory.read_u32(shader + 0x0C)?;
    let divisors = [ctx.memory.read_u32(shader + 0x18)?, ctx.memory.read_u32(shader + 0x1C)?];
    ctx.os.gx2.set_context(
        reg::SQ_PGM_START_FS,
        &[physical(address) >> 8, size >> 3, FETCH_RING_SIZE, FETCH_RING_SIZE, resources],
    );
    ctx.os.gx2.set_context(reg::VGT_INSTANCE_STEP_RATE_0, &divisors);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetVertexShader")]
fn gx2_set_vertex_shader(ctx: &mut HleContext<'_>, shader: u32) -> Result<(), HleError> {
    use vertex_shader as f;
    let (address, size) = program(ctx, shader, f::PROGRAM, f::SIZE, f::R_BUFFER)?;
    let resources = ctx.memory.read_u32(shader + f::RESOURCES)?;
    let mode = ctx.memory.read_u32(shader + f::MODE)?;
    let start =
        [physical(address) >> 8, size >> 3, PROGRAM_RING_SIZE, PROGRAM_RING_SIZE, resources];
    if mode == SHADER_MODE_GEOMETRY {
        ctx.os.gx2.set_context(reg::SQ_PGM_START_ES, &start);
    } else {
        ctx.os.gx2.set_context(reg::SQ_PGM_START_VS, &start);
        let primitive_id = ctx.memory.read_u32(shader + f::PRIMITIVE_ID_EN)?;
        let out_config = ctx.memory.read_u32(shader + f::OUT_CONFIG)?;
        let out_cntl = ctx.memory.read_u32(shader + f::OUT_CNTL)?;
        ctx.os.gx2.set_context(reg::VGT_PRIMITIVEID_EN, &[primitive_id]);
        ctx.os.gx2.set_context(reg::SPI_VS_OUT_CONFIG, &[out_config]);
        ctx.os.gx2.set_context(reg::PA_CL_VS_OUT_CNTL, &[out_cntl]);
        let id_count = ctx.memory.read_u32(shader + f::OUT_ID_COUNT)?.min(10);
        let ids = (0..id_count)
            .map(|i| ctx.memory.read_u32(shader + f::OUT_IDS + 4 * i))
            .collect::<Result<Vec<_>, _>>()?;
        if !ids.is_empty() {
            ctx.os.gx2.set_context(reg::SPI_VS_OUT_ID_0, &ids);
        }
        if ctx.memory.read_u32(shader + f::USES_STREAM_OUT)? != 0 {
            for buffer in 0..STREAM_OUT_BUFFERS {
                let stride = ctx.memory.read_u32(shader + f::STREAM_OUT_STRIDES + 4 * buffer)?;
                ctx.os.gx2.set_context(reg::VGT_STRMOUT_VTX_STRIDE_0 + buffer * 4, &[stride >> 2]);
            }
        }
    }
    let semantic_count = ctx.memory.read_u32(shader + f::SEMANTIC_COUNT)?.min(32);
    if semantic_count > 0 {
        let table = (0..semantic_count)
            .map(|i| ctx.memory.read_u32(shader + f::SEMANTICS + 4 * i))
            .collect::<Result<Vec<_>, _>>()?;
        ctx.os.gx2.set_context(reg::SQ_VTX_SEMANTIC_CLEAR, &[u32::MAX]);
        ctx.os.gx2.set_context(reg::SQ_VTX_SEMANTIC_0, &table);
    }
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetPixelShader")]
fn gx2_set_pixel_shader(ctx: &mut HleContext<'_>, shader: u32) -> Result<(), HleError> {
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
fn gx2_set_geometry_shader(ctx: &mut HleContext<'_>, shader: u32) -> Result<(), HleError> {
    let mode = ctx.memory.read_u32(shader + 8)?;
    ctx.os.gx2.set_context(reg::VGT_GS_MODE, &[mode]);
    Ok(())
}

/// `VGT_GS_MODE` follows the shader mode unless geometry shaders set it themselves.
pub(super) fn set_shader_mode(gx2: &mut Gx2State, mode: u32) {
    if mode != SHADER_MODE_GEOMETRY {
        gx2.set_context(reg::VGT_GS_MODE, &[0]);
    }
    // DX9_CONSTS: uniforms come from registers instead of blocks.
    gx2.set_config(config::SQ_CONFIG, &[u32::from(mode == 0) << 8]);
}

#[hle_export(lib = "gx2", name = "GX2SetShaderModeEx")]
fn gx2_set_shader_mode_ex(
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
fn gx2_set_shader_mode(ctx: &mut HleContext<'_>, mode: u32) {
    set_shader_mode(&mut ctx.os.gx2, mode);
}

// ---- Stream-out --------------------------------------------------------------

/// `GX2_MAX_STREAMOUT_BUFFERS`.
const STREAM_OUT_BUFFERS: u32 = 4;

/// `GX2StreamOutBuffer`.
mod stream_out {
    pub const SIZE: u32 = 0x00;
    pub const DATA: u32 = 0x04;
    pub const R_BUFFER: u32 = 0x0C;
    pub const CONTEXT: u32 = 0x1C;
}

#[hle_export(lib = "gx2", name = "GX2SetStreamOutBuffer")]
fn gx2_set_stream_out_buffer(
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
fn gx2r_set_stream_out_buffer(
    ctx: &mut HleContext<'_>,
    index: u32,
    buffer: u32,
) -> Result<(), HleError> {
    gx2_set_stream_out_buffer(ctx, index, buffer)
}

#[hle_export(lib = "gx2", name = "GX2SetStreamOutEnable")]
fn gx2_set_stream_out_enable(ctx: &mut HleContext<'_>, enable: u32) {
    ctx.os.gx2.set_context(reg::VGT_STRMOUT_EN, &[enable & 1]);
}

/// `mode` 0 continues from the offset saved in the buffer's context, 1 starts over.
#[hle_export(lib = "gx2", name = "GX2SetStreamOutContext")]
fn gx2_set_stream_out_context(
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
fn gx2_save_stream_out_context(
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

// ---- Textures --------------------------------------------------------------

mod texture {
    pub const VIEW_FIRST_MIP: u32 = 0x74;
    pub const VIEW_NUM_MIPS: u32 = 0x78;
    pub const VIEW_FIRST_SLICE: u32 = 0x7C;
    pub const VIEW_NUM_SLICES: u32 = 0x80;
    pub const COMP_SEL: u32 = 0x84;
    pub const WORD0: u32 = 0x88;
}

const DIM_CUBEMAP: u32 = 3;
const DIM_3D: u32 = 2;
const DIM_1D_ARRAY: u32 = 4;
const DIM_2D_ARRAY: u32 = 5;
const DIM_2D_ARRAY_MSAA: u32 = 7;
const FORMAT_BIT_INT: u32 = 0x100;
const FORMAT_BIT_SIGNED: u32 = 0x200;
const FORMAT_BIT_SRGB: u32 = 0x400;
const FORMAT_BIT_FLOAT: u32 = 0x800;
/// `E_NUM_FORMAT_ALL`.
const NUM_FORMAT_INT: u32 = 1;
const NUM_FORMAT_SCALED: u32 = 2;

#[hle_export(lib = "gx2", name = "GX2InitTextureRegs")]
fn gx2_init_texture_regs(ctx: &mut HleContext<'_>, tex: u32) -> Result<(), HleError> {
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

fn set_texture(
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
fn gx2_set_pixel_texture(ctx: &mut HleContext<'_>, tex: u32, unit: u32) -> Result<(), HleError> {
    set_texture(ctx, tex, resource::PS_TEXTURE, unit)
}

#[hle_export(lib = "gx2", name = "GX2SetVertexTexture")]
fn gx2_set_vertex_texture(ctx: &mut HleContext<'_>, tex: u32, unit: u32) -> Result<(), HleError> {
    set_texture(ctx, tex, resource::VS_TEXTURE, unit)
}

#[hle_export(lib = "gx2", name = "GX2SetGeometryTexture")]
fn gx2_set_geometry_texture(ctx: &mut HleContext<'_>, tex: u32, unit: u32) -> Result<(), HleError> {
    set_texture(ctx, tex, resource::GS_TEXTURE, unit)
}

/// Sampler register index of the first unit of each stage.
const SAMPLER_BASE_PIXEL: u32 = 0;
const SAMPLER_BASE_VERTEX: u32 = 18;
const SAMPLER_BASE_GEOMETRY: u32 = 36;

fn set_sampler(ctx: &mut HleContext<'_>, sampler: u32, index: u32) -> Result<(), HleError> {
    let read = |i: u32| ctx.memory.read_u32(sampler + 4 * i);
    let words = [index * 3, read(0)?, read(1)?, read(2)?];
    ctx.os.gx2.emit(opcode::SET_SAMPLER, &words);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetPixelSampler")]
fn gx2_set_pixel_sampler(
    ctx: &mut HleContext<'_>,
    sampler: u32,
    unit: u32,
) -> Result<(), HleError> {
    set_sampler(ctx, sampler, unit + SAMPLER_BASE_PIXEL)
}

#[hle_export(lib = "gx2", name = "GX2SetVertexSampler")]
fn gx2_set_vertex_sampler(
    ctx: &mut HleContext<'_>,
    sampler: u32,
    unit: u32,
) -> Result<(), HleError> {
    set_sampler(ctx, sampler, unit + SAMPLER_BASE_VERTEX)
}

#[hle_export(lib = "gx2", name = "GX2SetGeometrySampler")]
fn gx2_set_geometry_sampler(
    ctx: &mut HleContext<'_>,
    sampler: u32,
    unit: u32,
) -> Result<(), HleError> {
    set_sampler(ctx, sampler, unit + SAMPLER_BASE_GEOMETRY)
}

fn set_border_color(gx2: &mut Gx2State, register: u32, unit: u32, color: [f32; 4]) {
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
fn gx2_set_pixel_sampler_border_color(
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
fn gx2_set_vertex_sampler_border_color(
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

pub const EXPORTS: &[HleExport] = &[
    GX2_DRAW_EX,
    GX2_DRAW_INDEXED_EX,
    GX2_DRAW_INDEXED_EX2,
    GX2_DRAW_INDEXED_IMMEDIATE_EX,
    GX2_SET_ATTRIB_BUFFER,
    GX2R_SET_ATTRIBUTE_BUFFER,
    GX2_SET_VERTEX_UNIFORM_BLOCK,
    GX2_SET_PIXEL_UNIFORM_BLOCK,
    GX2_SET_GEOMETRY_UNIFORM_BLOCK,
    GX2R_SET_VERTEX_UNIFORM_BLOCK,
    GX2R_SET_PIXEL_UNIFORM_BLOCK,
    GX2_SET_VERTEX_UNIFORM_REG,
    GX2_SET_PIXEL_UNIFORM_REG,
    GX2_SET_FETCH_SHADER,
    GX2_SET_VERTEX_SHADER,
    GX2_SET_PIXEL_SHADER,
    GX2_SET_GEOMETRY_SHADER,
    GX2_SET_SHADER_MODE_EX,
    GX2_SET_SHADER_MODE,
    GX2_INIT_TEXTURE_REGS,
    GX2_SET_PIXEL_TEXTURE,
    GX2_SET_VERTEX_TEXTURE,
    GX2_SET_GEOMETRY_TEXTURE,
    GX2_SET_PIXEL_SAMPLER,
    GX2_SET_VERTEX_SAMPLER,
    GX2_SET_GEOMETRY_SAMPLER,
    GX2_SET_PIXEL_SAMPLER_BORDER_COLOR,
    GX2_SET_VERTEX_SAMPLER_BORDER_COLOR,
    GX2_SET_STREAM_OUT_BUFFER,
    GX2R_SET_STREAM_OUT_BUFFER,
    GX2_SET_STREAM_OUT_ENABLE,
    GX2_SET_STREAM_OUT_CONTEXT,
    GX2_SAVE_STREAM_OUT_CONTEXT,
];

#[cfg(test)]
#[path = "draw_tests.rs"]
mod tests;
