//! Shaders, buffers, textures and draw calls (port of Cemu's GX2_Draw,
//! GX2_Shader and GX2_Texture).
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ shaders, buffers,  │   │ gx2/draw.rs              │   │ draw commands      │
//! │ draws              │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ drawing                  │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Binds shaders, vertex buffers and textures, and issues the draw calls that make up a frame.
//! Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
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
pub(super) fn gx2_draw_indexed_ex(
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

/// Sampler register index of the first unit of each stage.
const SAMPLER_BASE_PIXEL: u32 = 0;
const SAMPLER_BASE_VERTEX: u32 = 18;
const SAMPLER_BASE_GEOMETRY: u32 = 36;

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

mod shaders_textures;
#[allow(unused_imports)]
pub(crate) use shaders_textures::*;
