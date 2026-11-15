//! Initialisation, scan buffers, clears and surface copies.
#![allow(clippy::too_many_arguments)]

use wiiu_gpu::{
    addrlib::{self, tile_mode},
    command::opcode,
    regs::context as reg,
    tiling::{Layout, swizzle_from_address},
};
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
    let mip = surface.info(level);
    [
        physical(level_image(surface, level)),
        (surface.width >> level).max(1),
        (surface.height >> level).max(1),
        surface.depth.max(1),
        mip.pitch,
        surface.tile_mode,
        level_swizzle(surface, level),
        surface.format,
        surface.aa,
        (level << 16) | (slice & 0xFFFF),
    ]
}

/// Where mip `level` of `surface` starts (effective address).
fn level_image(surface: &Gx2Surface, level: u32) -> u32 {
    match level {
        0 => surface.image,
        1 => surface.mip_ptr,
        n => surface.mip_ptr.wrapping_add(surface.mip_offset[(n as usize - 1).min(12)]),
    }
}

/// The pipe and bank swizzle of mip `level`: macro-tiled levels before the stop level only.
fn level_swizzle(surface: &Gx2Surface, level: u32) -> u32 {
    let macro_swizzle =
        tile_mode::is_macro_tiled(surface.tile_mode) && level < ((surface.swizzle >> 16) & 0xFF);
    if macro_swizzle { surface.swizzle & 0xFFFF } else { 0 }
}

/// Copies from a `LINEAR_SPECIAL` surface on the CPU, straight into memory, as GX2 and
/// Cemu (`LatteSurfaceCopy_CopyInRAM`) do. BotW builds its wind texture this way.
///
/// Returns `false` when the surfaces do not fit together or lie outside memory.
fn copy_in_memory(
    ctx: &mut HleContext<'_>,
    (source, src_mip, src_slice): (&Gx2Surface, u32, u32),
    (target, dst_mip, dst_slice): (&Gx2Surface, u32, u32),
) -> bool {
    let (src_info, dst_info) = (source.info(src_mip), target.info(dst_mip));
    if src_info.bpp != dst_info.bpp || dst_info.bpp < 8 {
        log::debug!("GX2CopySurface: bpp {} to {}", src_info.bpp, dst_info.bpp);
        return false;
    }
    let layout = |surface: &Gx2Surface, info: &addrlib::SurfaceInfo, level: u32| {
        let (pipe_swizzle, bank_swizzle) = swizzle_from_address(level_swizzle(surface, level));
        Layout {
            tile_mode: info.hw_tile_mode,
            bpp: info.bpp,
            pitch: info.pitch,
            height: info.height,
            samples: 1 << surface.aa,
            is_depth: false,
            pipe_swizzle,
            bank_swizzle,
        }
    };
    let base = |surface: &Gx2Surface, info: &addrlib::SurfaceInfo, level: u32| {
        let image = level_image(surface, level);
        if tile_mode::is_macro_tiled(info.hw_tile_mode) { image & !0x7FF } else { image }
    };
    let (from, to) = (layout(source, &src_info, src_mip), layout(target, &dst_info, dst_mip));
    let size = |info: &addrlib::SurfaceInfo| (info.surf_size as u32).max(1);
    let Ok(input) = ctx.memory.slice(base(source, &src_info, src_mip), size(&src_info)) else {
        return false;
    };
    let input = input.to_vec();
    let target_base = base(target, &dst_info, dst_mip);
    let Ok(output) = ctx.memory.slice_mut(target_base, size(&dst_info)) else { return false };
    // Like a texture, a compressed surface is copied in blocks.
    let blocks =
        |n: u32| if addrlib::is_compressed(target.format & 0x3F) { n.div_ceil(4) } else { n };
    let width = blocks((target.width >> dst_mip).max(1));
    let height = blocks((target.height >> dst_mip).max(1));
    let bytes = (dst_info.bpp / 8) as usize;
    to.for_each_pixel(width, height, dst_slice, |x, y, offset| {
        let at = from.address(x, y, src_slice) as usize;
        let offset = offset as usize;
        if let (Some(pixel), Some(out)) =
            (input.get(at..at + bytes), output.get_mut(offset..offset + bytes))
        {
            out.copy_from_slice(pixel);
        }
    });
    ctx.gpu.note_cpu_write(target_base, size(&dst_info));
    true
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
    let mut target = target & (opcode::scan_target::TV | opcode::scan_target::DRC);
    if ctx.os.gx2.tv_srgb {
        target |= opcode::scan_target::TV_SRGB;
    }
    if ctx.os.gx2.drc_srgb {
        target |= opcode::scan_target::DRC_SRGB;
    }
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
    if source.tile_mode == tile_mode::TM_LINEAR_SPECIAL
        && copy_in_memory(ctx, (&source, src_mip, src_slice), (&target, dst_mip, dst_slice))
    {
        return Ok(());
    }
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

#[cfg(test)]
mod tests {
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
}
