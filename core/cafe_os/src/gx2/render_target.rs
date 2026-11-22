//! Color/depth buffers, viewport and scissor (port of Cemu's GX2_RenderTarget
//! and the matching parts of GX2_State). Everything ends up as
//! `SET_CONTEXT_REG` packets.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ "draw into this    │   │ gx2/render_target.rs     │   │ color/depth        │
//! │ image"             │──►│ ──────────────────────── │──►│ targets set        │
//! │                    │   │ render targets           │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Chooses which images the GPU draws into (color and depth) and the visible area of the
//! screen. Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use wiiu_gpu::{
    addrlib::tile_mode,
    command::opcode,
    regs::{base, context as reg},
};
use wiiu_hle_macros::hle_export;

use super::surface::Gx2Surface;
use crate::hle::{HleContext, HleError};

/// Offsets inside `GX2ColorBuffer` / `GX2DepthBuffer` after the surface.
mod field {
    pub const VIEW_MIP: u32 = 0x74;
    pub const VIEW_FIRST_SLICE: u32 = 0x78;
    pub const VIEW_NUM_SLICES: u32 = 0x7C;
    pub const CB_REG_SIZE: u32 = 0x88;
    pub const CB_REG_INFO: u32 = 0x8C;
    pub const CB_REG_VIEW: u32 = 0x90;
    pub const CB_REG_MASK: u32 = 0x94;
    pub const DB_CLEAR_DEPTH: u32 = 0x88;
    pub const DB_CLEAR_STENCIL: u32 = 0x8C;
    pub const DB_REG_SIZE: u32 = 0x90;
}

/// Surface format flag bits (`GX2SurfaceFormat` bits 8..=11).
mod format_bit {
    pub const INT: u32 = 0x100;
    pub const FLOAT: u32 = 0x800;
}

/// `CB_COLOR*_INFO.NUMBER_TYPE` "export normalised" table (Cemu's
/// `GetSurfaceColorBufferExportFormat`).
const EXPORT_FORMAT: [u8; 0x40] = [
    1, 1, 1, 1, 1, 0, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 1, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0,
    0, //
    1, 0, 0, 0, 1, 1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// Queues `SET_CONTEXT_REG` for consecutive registers starting at `register`.
pub(crate) fn set_context_regs(ctx: &mut HleContext<'_>, register: u32, values: &[u32]) {
    let mut body = vec![register - base::CONTEXT];
    body.extend_from_slice(values);
    ctx.os.gx2.emit(opcode::SET_CONTEXT_REG, &body);
}

fn size_register(info_pitch: u32, info_height: u32) -> u32 {
    let pitch_height = (info_height * info_pitch) >> 6;
    (((info_pitch >> 3).wrapping_sub(1)) & 0x3FF)
        | ((pitch_height.wrapping_sub(1) & 0xF_FFFF) << 10)
}

#[hle_export(lib = "gx2", name = "GX2InitColorBufferRegs")]
fn gx2_init_color_buffer_regs(ctx: &mut HleContext<'_>, buffer: u32) -> Result<(), HleError> {
    let surface = Gx2Surface::read(ctx.memory, buffer)?;
    let view_mip = ctx.memory.read_u32(buffer + field::VIEW_MIP)?;
    let info = surface.info(view_mip);
    ctx.memory.write_u32(buffer + field::CB_REG_SIZE, size_register(info.pitch, info.height))?;

    let format = surface.format;
    let hw_format = format & 0x3F;
    let mut reg_info = (hw_format << 2) | ((info.hw_tile_mode & 0xF) << 8);
    let mut clamp_blend = false;
    match format & 0xF00 {
        0x000 => clamp_blend = true,
        0x100 => reg_info |= 4 << 12,
        0x200 => {
            reg_info |= 1 << 12;
            clamp_blend = true;
        }
        0x300 => reg_info |= 5 << 12,
        0x400 => {
            reg_info |= 6 << 12;
            clamp_blend = true;
        }
        0x800 => reg_info |= 7 << 12,
        _ => {}
    }
    if hw_format == 0x0C || hw_format == 0x1B {
        reg_info |= 2 << 16;
    }
    if surface.aa != 0 {
        reg_info |= 2 << 18;
    }
    let is_integer = format & format_bit::INT != 0;
    if !is_integer {
        reg_info |= u32::from(EXPORT_FORMAT[hw_format as usize]) << 27;
    }
    // R24_X8_UNORM, R24_X8_FLOAT, R32_X8_FLOAT
    if is_integer || matches!(format, 0x0011 | 0x0811 | 0x081C) {
        reg_info |= 1 << 22;
        clamp_blend = false;
    }
    if clamp_blend {
        reg_info |= 1 << 20;
    }
    if format & format_bit::FLOAT != 0 {
        reg_info |= 1 << 25;
    }
    ctx.memory.write_u32(buffer + field::CB_REG_INFO, reg_info)?;

    let reg_view = if surface.tile_mode == tile_mode::TM_LINEAR_SPECIAL {
        0
    } else {
        let first = ctx.memory.read_u32(buffer + field::VIEW_FIRST_SLICE)?;
        let count = ctx.memory.read_u32(buffer + field::VIEW_NUM_SLICES)?;
        (first & 0x7FF) | (((count + first).wrapping_sub(1) & 0x7FF) << 13)
    };
    ctx.memory.write_u32(buffer + field::CB_REG_VIEW, reg_view)?;
    ctx.memory.write_u32(buffer + field::CB_REG_MASK, 0)?;
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2InitDepthBufferRegs")]
fn gx2_init_depth_buffer_regs(ctx: &mut HleContext<'_>, buffer: u32) -> Result<(), HleError> {
    let surface = Gx2Surface::read(ctx.memory, buffer)?;
    let view_mip = ctx.memory.read_u32(buffer + field::VIEW_MIP)?;
    let info = surface.info(view_mip);
    ctx.memory.write_u32(buffer + field::DB_REG_SIZE, size_register(info.pitch, info.height))?;
    Ok(())
}

/// Physical address of the mip a render target views (with macro-tile swizzle).
fn view_base(surface: &Gx2Surface, view_mip: u32) -> u32 {
    let virtual_address = match view_mip {
        0 => surface.image,
        1 => surface.mip_ptr,
        n => surface.mip_ptr + surface.mip_offset[(n - 1) as usize],
    };
    let mut address =
        wiiu_mem_addr::effective_to_physical(virtual_address).unwrap_or(virtual_address);
    if tile_mode::is_macro_tiled(surface.tile_mode) && view_mip < ((surface.swizzle >> 16) & 0xFF) {
        address ^= surface.swizzle & 0xFFFF;
    }
    address
}

#[hle_export(lib = "gx2", name = "GX2SetColorBuffer")]
fn gx2_set_color_buffer(
    ctx: &mut HleContext<'_>,
    buffer: u32,
    target: u32,
) -> Result<(), HleError> {
    let surface = Gx2Surface::read(ctx.memory, buffer)?;
    let view_mip = ctx.memory.read_u32(buffer + field::VIEW_MIP)?;
    let size = ctx.memory.read_u32(buffer + field::CB_REG_SIZE)?;
    let view = ctx.memory.read_u32(buffer + field::CB_REG_VIEW)?;
    let info = ctx.memory.read_u32(buffer + field::CB_REG_INFO)?;
    let target = target & 7;
    set_context_regs(ctx, reg::CB_COLOR0_BASE + target, &[view_base(&surface, view_mip)]);
    set_context_regs(ctx, reg::CB_COLOR0_SIZE + target, &[size]);
    set_context_regs(ctx, reg::CB_COLOR0_VIEW + target, &[view]);
    set_context_regs(ctx, reg::CB_COLOR0_INFO + target, &[info]);
    announce_target(ctx, target, &surface, view_mip, buffer)
}

/// Tells the GPU layer which surface a target slot views: register values
/// alone do not carry the real width.
fn announce_target(
    ctx: &mut HleContext<'_>,
    slot: u32,
    surface: &Gx2Surface,
    view_mip: u32,
    buffer: u32,
) -> Result<(), HleError> {
    let slice = ctx.memory.read_u32(buffer + field::VIEW_FIRST_SLICE)?;
    let mut body = vec![slot];
    body.extend(super::display::descriptor(surface, view_mip, slice));
    ctx.os.gx2.emit(opcode::HLE_SET_TARGET, &body);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetDepthBuffer")]
fn gx2_set_depth_buffer(ctx: &mut HleContext<'_>, buffer: u32) -> Result<(), HleError> {
    let surface = Gx2Surface::read(ctx.memory, buffer)?;
    let size = ctx.memory.read_u32(buffer + field::DB_REG_SIZE)?;
    let htile_base =
        wiiu_mem_addr::effective_to_physical(surface.image).unwrap_or(surface.image) >> 8;
    let format_code = match surface.format {
        0x0005 => 1, // D16_UNORM
        0x0011 => 3, // D24_S8_UNORM
        0x0806 => 6, // D32_FLOAT
        0x081C => 7, // D32_S8_FLOAT
        0x0811 => 5, // D24_S8_FLOAT
        other => {
            log::debug!("GX2SetDepthBuffer: unsupported format {other:#06x}");
            0
        }
    };
    let depth_info = ((surface.tile_mode & 0xF) << 15) | format_code;
    let first = ctx.memory.read_u32(buffer + field::VIEW_FIRST_SLICE)?;
    let count = ctx.memory.read_u32(buffer + field::VIEW_NUM_SLICES)?;
    let view = (first & 0x7FF) | (((count + first).wrapping_sub(1) & 0x7FF) << 13);
    set_context_regs(ctx, reg::DB_DEPTH_SIZE, &[size]);
    set_context_regs(ctx, reg::DB_DEPTH_BASE, &[0, depth_info, htile_base]);
    set_context_regs(ctx, reg::DB_DEPTH_VIEW, &[view]);
    let view_mip = ctx.memory.read_u32(buffer + field::VIEW_MIP)?;
    announce_target(ctx, 8, &surface, view_mip, buffer)
}

#[hle_export(lib = "gx2", name = "GX2SetClearDepthStencil")]
fn gx2_set_clear_depth_stencil(
    ctx: &mut HleContext<'_>,
    buffer: u32,
    depth: f32,
    stencil: u32,
) -> Result<(), HleError> {
    ctx.memory.write_f32(buffer + field::DB_CLEAR_DEPTH, depth)?;
    ctx.memory.write_u32(buffer + field::DB_CLEAR_STENCIL, stencil)?;
    set_context_regs(ctx, reg::DB_STENCIL_CLEAR, &[stencil & 0xFF, depth.to_bits()]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_register_matches_hardware_encoding() {
        // pitch 1280, height 720: PITCH_TILE_MAX = 159, SLICE_TILE_MAX = 14399
        assert_eq!(size_register(1280, 720), 159 | (14399 << 10));
    }
}
