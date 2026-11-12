//! Initialisation, scan buffers, clears and surface copies.
#![allow(clippy::too_many_arguments)]

use wiiu_gpu::{addrlib::tile_mode, command::opcode, regs::context as reg};
use wiiu_hle_macros::hle_export;
use wiiu_memory::GuestPtr;

use super::{Gx2State, Gx2Surface, physical, surface::resource_flag};
use crate::hle::{HleContext, HleError};

/// `GX2TVRenderMode` → resolution.
const fn tv_size(mode: u32) -> (u32, u32) {
    match mode {
        1 => (640, 480),
        2 => (854, 480),
        5 | 6 => (1920, 1080),
        _ => (1280, 720),
    }
}

/// `GX2BufferingMode` → number of buffers.
const fn buffer_count(mode: u32) -> u32 {
    match mode {
        1 => 1,
        4 => 3,
        _ => 2,
    }
}

#[hle_export(lib = "gx2", name = "GX2Init")]
fn gx2_init(ctx: &mut HleContext<'_>, _attributes: GuestPtr<u32>) {
    ctx.os.gx2.initialized = true;
    ctx.os.gx2.main_core = ctx.cpu.core_id();
}

#[hle_export(lib = "gx2", name = "GX2Shutdown")]
fn gx2_shutdown(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    super::flush(ctx)?;
    ctx.gpu.wait_idle();
    ctx.os.gx2.initialized = false;
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2GetMainCoreId")]
fn gx2_get_main_core_id(ctx: &mut HleContext<'_>) -> i32 {
    if ctx.os.gx2.initialized { ctx.os.gx2.main_core.cast_signed() } else { -1 }
}

#[hle_export(lib = "gx2", name = "GX2SetTVEnable")]
fn gx2_set_tv_enable(ctx: &mut HleContext<'_>, enable: bool) {
    ctx.os.gx2.tv_enabled = enable;
}

#[hle_export(lib = "gx2", name = "GX2SetDRCEnable")]
fn gx2_set_drc_enable(ctx: &mut HleContext<'_>, enable: bool) {
    ctx.os.gx2.drc_enabled = enable;
}

#[hle_export(lib = "gx2", name = "GX2CalcTVSize")]
fn gx2_calc_tv_size(
    ctx: &mut HleContext<'_>,
    mode: u32,
    _format: u32,
    buffering: u32,
    size: GuestPtr<u32>,
    scale_needed: GuestPtr<u32>,
) -> Result<(), HleError> {
    let (width, height) = tv_size(mode);
    size.write(ctx.memory, width * height * 4 * buffer_count(buffering))?;
    if !scale_needed.is_null() {
        scale_needed.write(ctx.memory, 0)?;
    }
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2CalcDRCSize")]
fn gx2_calc_drc_size(
    ctx: &mut HleContext<'_>,
    _mode: u32,
    _format: u32,
    buffering: u32,
    size: GuestPtr<u32>,
    scale_needed: GuestPtr<u32>,
) -> Result<(), HleError> {
    size.write(ctx.memory, 854 * 480 * 4 * buffer_count(buffering))?;
    if !scale_needed.is_null() {
        scale_needed.write(ctx.memory, 0)?;
    }
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2GetSystemTVScanMode")]
fn gx2_get_system_tv_scan_mode(_ctx: &mut HleContext<'_>) -> u32 {
    // GX2_TV_SCAN_MODE_720P
    3
}

#[hle_export(lib = "gx2", name = "GX2GetSystemTVAspectRatio")]
fn gx2_get_system_tv_aspect_ratio(_ctx: &mut HleContext<'_>) -> u32 {
    // GX2_ASPECT_RATIO_16_9
    1
}

#[hle_export(lib = "gx2", name = "GX2IsVideoOutReady")]
fn gx2_is_video_out_ready(_ctx: &mut HleContext<'_>) -> bool {
    true
}

#[hle_export(lib = "gx2", name = "GX2GetGPUTimeout")]
fn gx2_get_gpu_timeout(_ctx: &mut HleContext<'_>) -> u32 {
    0x3E8
}

// ---- Surfaces as seen by the HLE packets ----------------------------------

/// `GX2ColorBuffer`/`GX2DepthBuffer` offsets after the surface.
mod view {
    pub const MIP: u32 = 0x74;
    pub const FIRST_SLICE: u32 = 0x78;
}

/// The descriptor of mip `level`, slice `slice`, as `HLE_*` packets carry it.
pub(super) fn descriptor(
    surface: &Gx2Surface,
    level: u32,
    slice: u32,
) -> [u32; opcode::SURFACE_WORDS] {
    let image = match level {
        0 => surface.image,
        1 => surface.mip_ptr,
        n => surface.mip_ptr.wrapping_add(surface.mip_offset[(n as usize - 1).min(12)]),
    };
    let macro_swizzle =
        tile_mode::is_macro_tiled(surface.tile_mode) && level < ((surface.swizzle >> 16) & 0xFF);
    let swizzle = if macro_swizzle { surface.swizzle & 0xFFFF } else { 0 };
    let mip = surface.info(level);
    [
        physical(image),
        (surface.width >> level).max(1),
        (surface.height >> level).max(1),
        surface.depth.max(1),
        mip.pitch,
        surface.tile_mode,
        swizzle,
        surface.format,
        surface.aa,
        (level << 16) | (slice & 0xFFFF),
    ]
}

fn view_descriptor(
    ctx: &HleContext<'_>,
    buffer: u32,
) -> Result<[u32; opcode::SURFACE_WORDS], HleError> {
    let surface = Gx2Surface::read(ctx.memory, buffer)?;
    let level = ctx.memory.read_u32(buffer + view::MIP)?;
    let slice = ctx.memory.read_u32(buffer + view::FIRST_SLICE)?;
    Ok(descriptor(&surface, level, slice))
}

/// `GX2ClearFlags`.
mod clear_flag {
    pub const DEPTH: u32 = 1;
    pub const STENCIL: u32 = 2;
    pub const SET_DEPTH_REG: u32 = 4;
    pub const SET_STENCIL_REG: u32 = 8;
}

fn update_clear_regs(gx2: &mut Gx2State, depth: f32, stencil: u32, flags: u32) {
    if flags & clear_flag::SET_DEPTH_REG != 0 {
        gx2.set_context(reg::DB_DEPTH_CLEAR, &[depth.to_bits()]);
    }
    if flags & clear_flag::SET_STENCIL_REG != 0 {
        gx2.set_context(reg::DB_STENCIL_CLEAR, &[stencil & 0xFF]);
    }
}

fn submit_clear(
    gx2: &mut Gx2State,
    color: Option<([u32; opcode::SURFACE_WORDS], [f32; 4])>,
    depth: Option<([u32; opcode::SURFACE_WORDS], f32, u32, u32)>,
) {
    let mut body = Vec::with_capacity(2 * opcode::SURFACE_WORDS + 7);
    let mut flags = 0;
    let (color_words, rgba) = match color {
        Some((words, rgba)) => {
            flags |= opcode::clear::COLOR;
            (words, rgba)
        }
        None => ([0; opcode::SURFACE_WORDS], [0.0; 4]),
    };
    let (depth_words, depth_value, stencil_value) = match depth {
        Some((words, value, stencil, clear_flags)) => {
            if clear_flags & clear_flag::DEPTH != 0 {
                flags |= opcode::clear::DEPTH;
            }
            if clear_flags & clear_flag::STENCIL != 0 {
                flags |= opcode::clear::STENCIL;
            }
            (words, value, stencil)
        }
        None => ([0; opcode::SURFACE_WORDS], 0.0, 0),
    };
    body.push(flags);
    body.extend(color_words);
    body.extend(depth_words);
    body.extend(rgba.map(f32::to_bits));
    body.push(depth_value.to_bits());
    body.push(stencil_value & 0xFF);
    gx2.emit(opcode::HLE_CLEAR, &body);
}

#[hle_export(lib = "gx2", name = "GX2ClearColor")]
fn gx2_clear_color(
    ctx: &mut HleContext<'_>,
    buffer: u32,
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
) -> Result<(), HleError> {
    let surface = Gx2Surface::read(ctx.memory, buffer)?;
    if surface.resource_flags & resource_flag::COLOR_BUFFER == 0 {
        log::warn!("GX2ClearColor: surface is not a color buffer");
    }
    let words = view_descriptor(ctx, buffer)?;
    submit_clear(&mut ctx.os.gx2, Some((words, [red, green, blue, alpha])), None);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2ClearBuffersEx")]
fn gx2_clear_buffers_ex(
    ctx: &mut HleContext<'_>,
    color: u32,
    depth: u32,
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
    depth_value: f32,
    stencil: u32,
    flags: u32,
) -> Result<(), HleError> {
    update_clear_regs(&mut ctx.os.gx2, depth_value, stencil, flags);
    let color_words = view_descriptor(ctx, color)?;
    let depth_words = view_descriptor(ctx, depth)?;
    submit_clear(
        &mut ctx.os.gx2,
        Some((color_words, [red, green, blue, alpha])),
        Some((depth_words, depth_value, stencil, flags)),
    );
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2ClearDepthStencilEx")]
fn gx2_clear_depth_stencil_ex(
    ctx: &mut HleContext<'_>,
    depth: u32,
    value: f32,
    stencil: u32,
    flags: u32,
) -> Result<(), HleError> {
    update_clear_regs(&mut ctx.os.gx2, value, stencil, flags);
    let words = view_descriptor(ctx, depth)?;
    submit_clear(&mut ctx.os.gx2, None, Some((words, value, stencil, flags)));
    Ok(())
}

/// `GX2DepthBuffer::clearDepth`.
const CLEAR_DEPTH: u32 = 0x88;
/// `GX2DepthBuffer::clearStencil`.
const CLEAR_STENCIL: u32 = 0x8C;

#[hle_export(lib = "gx2", name = "GX2SetClearDepth")]
fn gx2_set_clear_depth(ctx: &mut HleContext<'_>, buffer: u32, value: f32) -> Result<(), HleError> {
    ctx.memory.write_f32(buffer + CLEAR_DEPTH, value)?;
    ctx.os.gx2.set_context(reg::DB_DEPTH_CLEAR, &[value.to_bits()]);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetClearStencil")]
fn gx2_set_clear_stencil(
    ctx: &mut HleContext<'_>,
    buffer: u32,
    value: u32,
) -> Result<(), HleError> {
    ctx.memory.write_u32(buffer + CLEAR_STENCIL, value & 0xFF)?;
    ctx.os.gx2.set_context(reg::DB_STENCIL_CLEAR, &[value & 0xFF]);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2CopyColorBufferToScanBuffer")]
fn gx2_copy_color_buffer_to_scan_buffer(
    ctx: &mut HleContext<'_>,
    buffer: u32,
    target: u32,
) -> Result<(), HleError> {
    let mut body = view_descriptor(ctx, buffer)?.to_vec();
    body.push(target);
    ctx.os.gx2.emit(opcode::HLE_COPY_COLOR_BUFFER_TO_SCAN_BUFFER, &body);
    Ok(())
}

fn submit_copy(
    ctx: &mut HleContext<'_>,
    src: [u32; opcode::SURFACE_WORDS],
    dst: [u32; opcode::SURFACE_WORDS],
) {
    let mut body = src.to_vec();
    body.extend(dst);
    ctx.os.gx2.emit(opcode::HLE_COPY_SURFACE, &body);
}

#[hle_export(lib = "gx2", name = "GX2CopySurface")]
fn gx2_copy_surface(
    ctx: &mut HleContext<'_>,
    src: u32,
    src_mip: u32,
    src_slice: u32,
    dst: u32,
    dst_mip: u32,
    dst_slice: u32,
) -> Result<(), HleError> {
    let source = Gx2Surface::read(ctx.memory, src)?;
    let target = Gx2Surface::read(ctx.memory, dst)?;
    submit_copy(
        ctx,
        descriptor(&source, src_mip, src_slice),
        descriptor(&target, dst_mip, dst_slice),
    );
    Ok(())
}

/// Multisampling is resolved at render time, leaving a plain copy.
#[hle_export(lib = "gx2", name = "GX2ResolveAAColorBuffer")]
fn gx2_resolve_aa_color_buffer(
    ctx: &mut HleContext<'_>,
    buffer: u32,
    dst: u32,
    dst_mip: u32,
    dst_slice: u32,
) -> Result<(), HleError> {
    let target = Gx2Surface::read(ctx.memory, dst)?;
    let source = view_descriptor(ctx, buffer)?;
    submit_copy(ctx, source, descriptor(&target, dst_mip, dst_slice));
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2ConvertDepthBufferToTextureSurface")]
fn gx2_convert_depth_buffer_to_texture_surface(
    ctx: &mut HleContext<'_>,
    buffer: u32,
    dst: u32,
    dst_mip: u32,
    dst_slice: u32,
) -> Result<(), HleError> {
    let target = Gx2Surface::read(ctx.memory, dst)?;
    let source = view_descriptor(ctx, buffer)?;
    submit_copy(ctx, source, descriptor(&target, dst_mip, dst_slice));
    Ok(())
}
