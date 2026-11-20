//! Remaining GX2 exports of Cemu's GX2.cpp, GX2_Misc, GX2_Query, GX2_Event, GX2_State,
//! GX2_Texture, GX2_Draw (compute) and GX2_Surface_Copy.
use wiiu_gpu::{
    command::opcode,
    regs::{config, context},
};
use wiiu_hle_macros::hle_export;
use wiiu_mem_addr::TILING_APERTURE;

use super::{Gx2Surface, physical};
use crate::hle::{HleContext, HleError, HleExport, callback::GuestCall};

/// `GX2ScanTarget`.
const SCAN_TARGET_TV: u32 = 1;
const SCAN_TARGET_DRC_FIRST: u32 = 4;
/// `GX2QueryType`.
const QUERY_OCCLUSION_CPU: u32 = 0;
const QUERY_OCCLUSION_GPU: u32 = 2;
/// `sizeof(GX2ColorBuffer)`.
const COLOR_BUFFER_SIZE: u32 = 0xBC;
/// `R8_G8_B8_A8_UNORM`.
const FORMAT_RGBA8: u32 = 0x1A;
/// `TM_LINEAR_ALIGNED`.
const TILE_LINEAR_ALIGNED: u32 = 1;
/// Last-frame image `GX2GetLastFrame` hands out (640x480 RGBA8).
const LAST_FRAME_SIZE: u32 = 640 * 480 * 4;
/// Xenoblade Chronicles X (EU, US, JP): GPU occlusion queries start out "visible".
const XCX_TITLE_IDS: [u64; 3] =
    [0x0005_0000_101C_4C00, 0x0005_0000_101C_4D00, 0x0005_0000_1011_6100];
/// `'OCPU'` byte-swapped, as Cemu stores it in a CPU query.
const OCPU_MARKER: u32 = u32::from_le_bytes(*b"UPCO");

/// Fake scan buffer: Cemu points it into the tiling aperture.
#[hle_export(lib = "gx2", name = "GX2GetCurrentScanBuffer")]
fn gx2_get_current_scan_buffer(
    ctx: &mut HleContext<'_>,
    target: u32,
    out: u32,
) -> Result<(), HleError> {
    ctx.memory.fill(out, COLOR_BUFFER_SIZE, 0)?;
    ctx.memory.write_u32(out + 4, 100)?;
    ctx.memory.write_u32(out + 8, 100)?;
    let image = match target {
        SCAN_TARGET_TV => TILING_APERTURE.start + 0x20_0000,
        SCAN_TARGET_DRC_FIRST => TILING_APERTURE.start + 0x4_0000,
        _ => 0,
    };
    ctx.memory.write_u32(out + 0x24, image)?;
    Ok(())
}

/// The title drew into the TV scan buffer itself: present it.
#[hle_export(lib = "gx2", name = "GX2MarkScanBufferCopied")]
fn gx2_mark_scan_buffer_copied(ctx: &mut HleContext<'_>, target: u32) {
    if target == SCAN_TARGET_TV {
        let body = [
            physical(TILING_APERTURE.start + 0x20_0000),
            1920,
            1080,
            1,
            1920,
            4,
            0,
            FORMAT_RGBA8,
            0,
            0,
            target,
        ];
        ctx.os.gx2.emit(opcode::HLE_COPY_COLOR_BUFFER_TO_SCAN_BUFFER, &body);
    }
}

/// The first GamePad is always connected; the second never is.
#[hle_export(lib = "gx2", name = "GX2SetDRCConnectCallback")]
fn gx2_set_drc_connect_callback(ctx: &mut HleContext<'_>, channel: u32, callback: u32) -> u32 {
    if callback != 0 {
        let connected = u32::from(channel == 0);
        ctx.os.callbacks.push(GuestCall { function: callback, args: vec![channel, connected] });
    }
    0
}

#[hle_export(lib = "gx2", name = "GX2SetSemaphore")]
fn gx2_set_semaphore(ctx: &mut HleContext<'_>, semaphore: u32, mode: u32) -> u32 {
    let select = match mode {
        0 => 7,
        1 => 6,
        _ => return 0,
    };
    let control = (select << 29) | 0x1000;
    ctx.os.gx2.emit(opcode::MEM_SEMAPHORE, &[physical(semaphore), control]);
    0
}

#[hle_export(lib = "gx2", name = "GX2SetGPUFence")]
fn gx2_set_gpu_fence(ctx: &mut HleContext<'_>, fence: u32, mask: u32, compare: u32, value: u32) {
    const OPS: [u32; 8] = [0x7, 0x1, 0x3, 0x2, 0x6, 0x4, 0x5, 0x0];
    let body = [OPS[(compare & 7) as usize] | 0x10, physical(fence) | 2, 0, value, mask, 0xA];
    ctx.os.gx2.emit(opcode::WAIT_REG_MEM, &body);
}

#[hle_export(lib = "gx2", name = "GX2ResetGPU")]
fn gx2_reset_gpu(ctx: &mut HleContext<'_>, _unknown: u32) -> Result<(), HleError> {
    log::warn!("GX2ResetGPU()");
    super::command::gx2_draw_done(ctx)?;
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetTVGamma")]
fn gx2_set_tv_gamma(ctx: &mut HleContext<'_>, gamma: f32) {
    ctx.os.gx2.tv_gamma = 1.0 - gamma;
}

#[hle_export(lib = "gx2", name = "GX2SetDRCGamma")]
fn gx2_set_drc_gamma(ctx: &mut HleContext<'_>, gamma: f32) {
    ctx.os.gx2.drc_gamma = 1.0 - gamma;
}

/// A 640x480 RGBA8 texture standing for the last frame shown (Cemu leaves it black).
#[hle_export(lib = "gx2", name = "GX2GetLastFrame")]
fn gx2_get_last_frame(ctx: &mut HleContext<'_>, _device: u32, out: u32) -> Result<bool, HleError> {
    if ctx.os.gx2.last_frame == 0 {
        ctx.os.gx2.last_frame = ctx.os.registry.allocate_data(LAST_FRAME_SIZE, 0x100);
    }
    let mut surface = Gx2Surface {
        dim: 1,
        width: 640,
        height: 480,
        depth: 1,
        num_levels: 1,
        format: FORMAT_RGBA8,
        tile_mode: TILE_LINEAR_ALIGNED,
        ..Gx2Surface::default()
    };
    surface.calc_size_and_alignment();
    surface.image = ctx.os.gx2.last_frame;
    surface.write(ctx.memory, out)?;
    for (offset, value) in [(0x74, 0), (0x78, 1), (0x7C, 0), (0x80, 1), (0x84, 0x0001_0203)] {
        ctx.memory.write_u32(out + offset, value)?;
    }
    super::draw::gx2_init_texture_regs(ctx, out)?;
    Ok(true)
}

fn write_gamma(ctx: &mut HleContext<'_>, out: u32) -> Result<bool, HleError> {
    ctx.memory.write_f32(out, 1.0)?;
    Ok(true)
}

#[hle_export(lib = "gx2", name = "GX2GetLastFrameGammaA")]
fn gx2_get_last_frame_gamma_a(
    ctx: &mut HleContext<'_>,
    _device: u32,
    out: u32,
) -> Result<bool, HleError> {
    write_gamma(ctx, out)
}

#[hle_export(lib = "gx2", name = "GX2GetLastFrameGammaB")]
fn gx2_get_last_frame_gamma_b(
    ctx: &mut HleContext<'_>,
    _device: u32,
    out: u32,
) -> Result<bool, HleError> {
    write_gamma(ctx, out)
}

#[hle_export(lib = "gx2", name = "GX2GetSystemDRCMode")]
const fn gx2_get_system_drc_mode(_ctx: &mut HleContext<'_>) -> u32 {
    1
}

// ---- Occlusion queries ------------------------------------------------------

fn begin_occlusion_query(ctx: &mut HleContext<'_>, query: u32, gpu: bool) -> Result<(), HleError> {
    if gpu {
        if XCX_TITLE_IDS.contains(&ctx.os.title_id) {
            // Xenoblade X hides objects whose query still reads 0: start them visible.
            let passed = wiiu_gpu::command::executor::OCCLUSION_PASSED_SAMPLES;
            ctx.memory.write_bytes(query + 8, &passed.to_le_bytes())?;
        } else {
            for i in 0..8_u32 {
                let high = if i >= 4 { 0x8000_0000 } else { 0 };
                let body = [physical(query + i * 8) | 2, 0x2_0000, 0, high];
                ctx.os.gx2.emit(opcode::MEM_WRITE, &body);
            }
        }
    } else {
        ctx.memory.fill(query, 0x10, 0)?;
        ctx.memory.write_u32(query + 0x20, 0)?;
        ctx.memory.write_u32(query + 0x24, OCPU_MARKER)?;
    }
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2QueryBegin")]
fn gx2_query_begin(ctx: &mut HleContext<'_>, kind: u32, query: u32) -> Result<(), HleError> {
    match kind {
        QUERY_OCCLUSION_CPU => begin_occlusion_query(ctx, query, false)?,
        QUERY_OCCLUSION_GPU => begin_occlusion_query(ctx, query, true)?,
        _ => {
            log::warn!("GX2QueryBegin: unsupported query type {kind}");
            return Ok(());
        }
    }
    ctx.os.gx2.emit(opcode::HLE_BEGIN_OCCLUSION_QUERY, &[query]);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2QueryEnd")]
fn gx2_query_end(ctx: &mut HleContext<'_>, kind: u32, query: u32) {
    if kind == QUERY_OCCLUSION_CPU || kind == QUERY_OCCLUSION_GPU {
        ctx.os.gx2.emit(opcode::HLE_END_OCCLUSION_QUERY, &[query]);
    } else {
        log::warn!("GX2QueryEnd: unsupported query type {kind}");
    }
}

fn read_le_u64(ctx: &HleContext<'_>, address: u32) -> Result<u64, HleError> {
    let mut bytes = [0_u8; 8];
    for (i, byte) in (0..).zip(bytes.iter_mut()) {
        *byte = ctx.memory.read_u8(address + i)?;
    }
    Ok(u64::from_le_bytes(bytes))
}

#[hle_export(lib = "gx2", name = "GX2QueryGetOcclusionResult")]
fn gx2_query_get_occlusion_result(
    ctx: &mut HleContext<'_>,
    query: u32,
    out: u32,
) -> Result<bool, HleError> {
    if ctx.memory.read_u32(query + 0x24)? == OCPU_MARKER && ctx.memory.read_u32(query + 0x20)? == 0
    {
        return Ok(false);
    }
    let start = read_le_u64(ctx, query)?;
    let end = read_le_u64(ctx, query + 8)?;
    if (start | end) & (1 << 63) != 0 {
        return Ok(false);
    }
    ctx.memory.write_u64(out, end.wrapping_sub(start))?;
    Ok(true)
}

#[hle_export(lib = "gx2", name = "GX2QueryBeginConditionalRender")]
fn gx2_query_begin_conditional_render(
    ctx: &mut HleContext<'_>,
    kind: u32,
    query: u32,
    dont_wait: u32,
    must_pass: u32,
) {
    let mut flags = if kind == QUERY_OCCLUSION_GPU { 1 << 13 } else { 2 << 13 };
    if must_pass != 0 {
        flags |= 1 << 31;
    }
    flags |= u32::from(dont_wait != 0) << 19;
    ctx.os.gx2.emit(opcode::SET_PREDICATION, &[physical(query), flags]);
}

#[hle_export(lib = "gx2", name = "GX2QueryEndConditionalRender")]
fn gx2_query_end_conditional_render(ctx: &mut HleContext<'_>) {
    ctx.os.gx2.emit(opcode::SET_PREDICATION, &[0, 0]);
}

// ---- Register helpers (GX2_State) --------------------------------------------

#[hle_export(lib = "gx2", name = "GX2GetDepthStencilControlReg")]
fn gx2_get_depth_stencil_control_reg(ctx: &mut HleContext<'_>, reg: u32) -> Result<(), HleError> {
    let value = ctx.memory.read_u32(reg)?;
    // (out-pointer argument index, bit shift, width)
    const FIELDS: [(usize, u32, u32); 13] = [
        (1, 1, 1),   // depth test enable (Z_ENABLE)
        (2, 2, 1),   // depth write enable
        (3, 4, 3),   // depth function
        (4, 0, 1),   // stencil enable
        (5, 7, 1),   // back stencil enable
        (6, 8, 3),   // front stencil function
        (7, 14, 3),  // front z-pass
        (8, 17, 3),  // front z-fail
        (9, 11, 3),  // front fail
        (10, 20, 3), // back stencil function
        (11, 26, 3), // back z-pass
        (12, 29, 3), // back z-fail
        (13, 23, 3), // back fail
    ];
    let mut args = crate::hle::ArgReader::new();
    let mut outs = [0_u32; 14];
    for slot in &mut outs {
        *slot = args.next_u32(ctx)?;
    }
    for (index, shift, width) in FIELDS {
        let out = outs[index];
        if out != 0 {
            ctx.memory.write_u32(out, (value >> shift) & ((1 << width) - 1))?;
        }
    }
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2GetTargetChannelMasksReg")]
fn gx2_get_target_channel_masks_reg(ctx: &mut HleContext<'_>, reg: u32) -> Result<(), HleError> {
    let value = ctx.memory.read_u32(reg)?;
    let mut args = crate::hle::ArgReader::new();
    let _reg = args.next_u32(ctx)?;
    for i in 0..8 {
        let out = args.next_u32(ctx)?;
        if out != 0 {
            ctx.memory.write_u32(out, (value >> (i * 4)) & 0xF)?;
        }
    }
    Ok(())
}

/// Seen in Color Splash, which never sets the result: nothing to do.
#[hle_export(lib = "gx2", name = "GX2InitHiStencilInfoRegs")]
const fn gx2_init_hi_stencil_info_regs(_ctx: &mut HleContext<'_>, _reg: u32) {}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn point_units(value: f32) -> u32 {
    ((value.max(0.0) * 8.0) as u32).min(0xFFFF)
}

#[hle_export(lib = "gx2", name = "GX2InitPointLimitsReg")]
fn gx2_init_point_limits_reg(
    ctx: &mut HleContext<'_>,
    reg: u32,
    min: f32,
    max: f32,
) -> Result<(), HleError> {
    Ok(ctx.memory.write_u32(reg, point_units(min) | (point_units(max) << 16))?)
}

#[hle_export(lib = "gx2", name = "GX2InitPointSizeReg")]
fn gx2_init_point_size_reg(
    ctx: &mut HleContext<'_>,
    reg: u32,
    width: f32,
    height: f32,
) -> Result<(), HleError> {
    Ok(ctx.memory.write_u32(reg, point_units(height) | (point_units(width) << 16))?)
}

#[hle_export(lib = "gx2", name = "GX2SetPointLimitsReg")]
fn gx2_set_point_limits_reg(ctx: &mut HleContext<'_>, reg: u32) -> Result<(), HleError> {
    let value = ctx.memory.read_u32(reg)?;
    ctx.os.gx2.set_context(context::PA_SU_POINT_MINMAX, &[value]);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetPointSizeReg")]
fn gx2_set_point_size_reg(ctx: &mut HleContext<'_>, reg: u32) -> Result<(), HleError> {
    let value = ctx.memory.read_u32(reg)?;
    ctx.os.gx2.set_context(context::PA_SU_POINT_SIZE, &[value]);
    Ok(())
}

// ---- Compute ------------------------------------------------------------------

/// `GX2ComputeShader` fields.
mod compute {
    pub const REGS: u32 = 0x00;
    pub const PROGRAM_SIZE: u32 = 0x30;
    pub const PROGRAM_PTR: u32 = 0x34;
    pub const RBUFFER_ELEMENT_SIZE: u32 = 0x78;
    pub const RBUFFER_ELEMENT_COUNT: u32 = 0x7C;
    pub const RBUFFER_PTR: u32 = 0x80;
}
/// `SQ_PGM_START_ES`: GX2 runs compute shaders in the export-shader slot.
const SQ_PGM_START_ES: u32 = 0xA228;
/// `SQ_CS_DISPATCH_PARAMS` resource slot, relative to `SQ_TEX_RESOURCE_WORD0`.
const CS_DISPATCH_PARAMS: u32 = 0x8DA4;

#[hle_export(lib = "gx2", name = "GX2SetComputeShader")]
fn gx2_set_compute_shader(ctx: &mut HleContext<'_>, shader: u32) -> Result<(), HleError> {
    let read = |o| ctx.memory.read_u32(shader + o);
    let (program, size) = match read(compute::PROGRAM_PTR)? {
        0 => (
            read(compute::RBUFFER_PTR)?,
            read(compute::RBUFFER_ELEMENT_SIZE)?
                .wrapping_mul(read(compute::RBUFFER_ELEMENT_COUNT)?),
        ),
        ptr => (ptr, read(compute::PROGRAM_SIZE)?),
    };
    let resources = read(compute::REGS)?;
    ctx.os.gx2.set_context(
        SQ_PGM_START_ES,
        &[physical(program) >> 8, size >> 3, 0x10_0000, 0x10_0000, resources],
    );
    Ok(())
}

/// Sets the dispatch parameters and issues the work as Cemu does (a point-list
/// auto draw). shocora's renderer has no compute path, so the draw does nothing visible.
#[hle_export(lib = "gx2", name = "GX2DispatchCompute")]
fn gx2_dispatch_compute(ctx: &mut HleContext<'_>, params: u32) -> Result<(), HleError> {
    let body = [
        CS_DISPATCH_PARAMS,
        physical(params),
        0xF,
        0x86_2000,
        1,
        0xABCD_1234,
        0xABCD_1234,
        0xC000_0000,
    ];
    ctx.os.gx2.emit(opcode::SET_RESOURCE, &body);
    let count = ctx
        .memory
        .read_u32(params)?
        .wrapping_mul(ctx.memory.read_u32(params + 4)?)
        .wrapping_mul(ctx.memory.read_u32(params + 8)?);
    log::debug!("GX2DispatchCompute: {count} work groups (no compute support)");
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetComputeTexture")]
fn gx2_set_compute_texture(ctx: &mut HleContext<'_>, tex: u32, unit: u32) -> Result<(), HleError> {
    super::draw::gx2_set_vertex_texture(ctx, tex, unit)
}

#[hle_export(lib = "gx2", name = "GX2SetComputeSampler")]
fn gx2_set_compute_sampler(
    ctx: &mut HleContext<'_>,
    sampler: u32,
    unit: u32,
) -> Result<(), HleError> {
    super::draw::gx2_set_vertex_sampler(ctx, sampler, unit)
}

#[hle_export(lib = "gx2", name = "GX2SetGeometrySamplerBorderColor")]
fn gx2_set_geometry_sampler_border_color(
    ctx: &mut HleContext<'_>,
    unit: u32,
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
) {
    super::draw::set_border_color(
        &mut ctx.os.gx2,
        config::TD_GS_SAMPLER0_BORDER_RED,
        unit,
        [red, green, blue, alpha],
    );
}

/// Only whole-surface copies are supported, as in Cemu: they act like `GX2CopySurface`.
#[hle_export(lib = "gx2", name = "GX2CopySurfaceEx")]
fn gx2_copy_surface_ex(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    let mut args = crate::hle::ArgReader::new();
    let mut a = [0_u32; 9];
    for slot in &mut a {
        *slot = args.next_u32(ctx)?;
    }
    let [src, src_mip, src_slice, dst, dst_mip, dst_slice, count, rects, _points] = a;
    let source = Gx2Surface::read(ctx.memory, src)?;
    for i in 0..count.cast_signed().max(0).cast_unsigned() {
        let rect = rects + i * 16;
        let left = ctx.memory.read_u32(rect)?;
        let top = ctx.memory.read_u32(rect + 4)?;
        let width = ctx.memory.read_u32(rect + 8)?.wrapping_sub(left);
        let height = ctx.memory.read_u32(rect + 12)?.wrapping_sub(top);
        if left == 0 && top == 0 && width == source.width && height == source.height {
            super::display::gx2_copy_surface(
                ctx, src, src_mip, src_slice, dst, dst_mip, dst_slice,
            )?;
        } else {
            log::warn!("GX2CopySurfaceEx: partial rectangle copies are not supported");
        }
    }
    Ok(())
}

pub const EXPORTS: &[HleExport] = &[
    GX2_GET_CURRENT_SCAN_BUFFER,
    GX2_MARK_SCAN_BUFFER_COPIED,
    GX2_SET_DRC_CONNECT_CALLBACK,
    GX2_SET_SEMAPHORE,
    GX2_SET_GPU_FENCE,
    GX2_RESET_GPU,
    GX2_SET_TV_GAMMA,
    GX2_SET_DRC_GAMMA,
    GX2_GET_LAST_FRAME,
    GX2_GET_LAST_FRAME_GAMMA_A,
    GX2_GET_LAST_FRAME_GAMMA_B,
    GX2_GET_SYSTEM_DRC_MODE,
    GX2_QUERY_BEGIN,
    GX2_QUERY_END,
    GX2_QUERY_GET_OCCLUSION_RESULT,
    GX2_QUERY_BEGIN_CONDITIONAL_RENDER,
    GX2_QUERY_END_CONDITIONAL_RENDER,
    GX2_GET_DEPTH_STENCIL_CONTROL_REG,
    GX2_GET_TARGET_CHANNEL_MASKS_REG,
    GX2_INIT_HI_STENCIL_INFO_REGS,
    GX2_INIT_POINT_LIMITS_REG,
    GX2_INIT_POINT_SIZE_REG,
    GX2_SET_POINT_LIMITS_REG,
    GX2_SET_POINT_SIZE_REG,
    GX2_SET_COMPUTE_SHADER,
    GX2_DISPATCH_COMPUTE,
    GX2_SET_COMPUTE_TEXTURE,
    GX2_SET_COMPUTE_SAMPLER,
    GX2_SET_GEOMETRY_SAMPLER_BORDER_COLOR,
    GX2_COPY_SURFACE_EX,
];

#[cfg(test)]
mod tests {
    use crate::hle::test_env::{SCRATCH, TestEnv};

    /// Intent: a CPU occlusion query is "not ready" until the GPU ends it, then reports
    /// the samples written between begin and end.
    /// Pass: before the end the result call returns 0; after the executor's end packet is
    /// emulated by writing the counters, it returns 1 with end - start.
    #[test]
    fn cpu_occlusion_query_is_pending_until_ended() {
        let mut env = TestEnv::new();
        let query = SCRATCH;
        let out = SCRATCH + 0x100;
        env.call("gx2", "GX2QueryBegin", &[0, query]);
        assert_eq!(env.call("gx2", "GX2QueryGetOcclusionResult", &[query, out]), 0);
        env.memory.write_bytes(query + 8, &50_u64.to_le_bytes()).unwrap();
        env.memory.write_u32(query + 0x20, 1).unwrap();
        assert_eq!(env.call("gx2", "GX2QueryGetOcclusionResult", &[query, out]), 1);
        assert_eq!(env.memory.read_u64(out).unwrap(), 50);
    }

    /// Intent: point registers hold sizes in 1/8 pixel units, clamped to 16 bits.
    /// Pass: width 2 and height 1 give 0x0010_0008.
    #[test]
    fn point_size_is_in_eighths() {
        let mut env = TestEnv::new();
        env.cpu.fpr[1].ps0 = 2.0;
        env.cpu.fpr[2].ps0 = 1.0;
        env.call("gx2", "GX2InitPointSizeReg", &[SCRATCH]);
        assert_eq!(env.memory.read_u32(SCRATCH).unwrap(), 0x0010_0008);
    }
}
