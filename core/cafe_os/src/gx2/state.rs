//! Fixed-function state (port of Cemu's GX2_State): every setter ends up as a
//! `SET_CONTEXT_REG` packet, `Init*Reg` variants store the same words in a
//! guest structure for a later `Set*Reg`.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ blend, depth,      │   │ gx2/state.rs             │   │ register writes    │
//! │ cull…              │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ fixed-function state     │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Settings such as blending, depth test and culling. Each one becomes a register write in the
//! GPU command stream. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::too_many_arguments)]

use wiiu_gpu::regs::context as reg;
use wiiu_hle_macros::hle_export;

use super::Gx2State;
use crate::hle::{HleContext, HleError, HleExport};

fn store(ctx: &mut HleContext<'_>, address: u32, words: &[u32]) -> Result<(), HleError> {
    for (i, word) in words.iter().enumerate() {
        ctx.memory.write_u32(address + 4 * i as u32, *word)?;
    }
    Ok(())
}

fn load<const N: usize>(ctx: &HleContext<'_>, address: u32) -> Result<[u32; N], HleError> {
    let mut words = [0; N];
    for (i, word) in words.iter_mut().enumerate() {
        *word = ctx.memory.read_u32(address + 4 * i as u32)?;
    }
    Ok(words)
}

// ---- Alpha test ------------------------------------------------------------

const fn alpha_test(enable: u32, function: u32, reference: f32) -> [u32; 2] {
    [(function & 7) | ((enable & 1) << 3), reference.to_bits()]
}

fn set_alpha_test_words(gx2: &mut Gx2State, words: [u32; 2]) {
    gx2.set_context(reg::SX_ALPHA_TEST_CONTROL, &[words[0]]);
    gx2.set_context(reg::SX_ALPHA_REF, &[words[1]]);
}

#[hle_export(lib = "gx2", name = "GX2InitAlphaTestReg")]
fn gx2_init_alpha_test_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    enable: u32,
    function: u32,
    reference: f32,
) -> Result<(), HleError> {
    store(ctx, out, &alpha_test(enable, function, reference))
}

#[hle_export(lib = "gx2", name = "GX2SetAlphaTestReg")]
fn gx2_set_alpha_test_reg(ctx: &mut HleContext<'_>, source: u32) -> Result<(), HleError> {
    let words = load::<2>(ctx, source)?;
    set_alpha_test_words(&mut ctx.os.gx2, words);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetAlphaTest")]
fn gx2_set_alpha_test(ctx: &mut HleContext<'_>, enable: u32, function: u32, reference: f32) {
    set_alpha_test_words(&mut ctx.os.gx2, alpha_test(enable, function, reference));
}

/// `SPECIAL_OP` 0 is normal rendering, 2 (`DISABLE`) keeps the color buffers untouched.
const fn color_control(logic_op: u32, blend_mask: u32, multiwrite: u32, color_enable: u32) -> u32 {
    let special = if color_enable == 0 { 4 } else { 0 };
    ((multiwrite & 1) << 1)
        | (special << 4)
        | ((blend_mask & 0xFF) << 8)
        | ((logic_op & 0xFF) << 16)
}

#[hle_export(lib = "gx2", name = "GX2InitColorControlReg")]
fn gx2_init_color_control_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    logic_op: u32,
    blend_mask: u32,
    multiwrite: u32,
    color_enable: u32,
) -> Result<(), HleError> {
    store(ctx, out, &[color_control(logic_op, blend_mask, multiwrite, color_enable)])
}

#[hle_export(lib = "gx2", name = "GX2SetColorControlReg")]
fn gx2_set_color_control_reg(ctx: &mut HleContext<'_>, source: u32) -> Result<(), HleError> {
    let [word] = load::<1>(ctx, source)?;
    ctx.os.gx2.set_context(reg::CB_COLOR_CONTROL, &[word]);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetColorControl")]
fn gx2_set_color_control(
    ctx: &mut HleContext<'_>,
    logic_op: u32,
    blend_mask: u32,
    multiwrite: u32,
    color_enable: u32,
) {
    ctx.os.gx2.set_context(
        reg::CB_COLOR_CONTROL,
        &[color_control(logic_op, blend_mask, multiwrite, color_enable)],
    );
}

// ---- Polygon state ---------------------------------------------------------

const fn polygon_control(
    front_face: u32,
    cull_front: u32,
    cull_back: u32,
    polygon_mode: u32,
    front_mode: u32,
    back_mode: u32,
    offset_front: u32,
    offset_back: u32,
    offset_para: u32,
) -> u32 {
    (cull_front & 1)
        | ((cull_back & 1) << 1)
        | ((front_face & 1) << 2)
        | ((polygon_mode & 3) << 3)
        | ((front_mode & 7) << 5)
        | ((back_mode & 7) << 8)
        | ((offset_front & 1) << 11)
        | ((offset_back & 1) << 12)
        | ((offset_para & 1) << 13)
}

#[hle_export(lib = "gx2", name = "GX2InitPolygonControlReg")]
fn gx2_init_polygon_control_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    front_face: u32,
    cull_front: u32,
    cull_back: u32,
    polygon_mode: u32,
    front_mode: u32,
    back_mode: u32,
    offset_front: u32,
    offset_back: u32,
    offset_para: u32,
) -> Result<(), HleError> {
    let word = polygon_control(
        front_face,
        cull_front,
        cull_back,
        polygon_mode,
        front_mode,
        back_mode,
        offset_front,
        offset_back,
        offset_para,
    );
    store(ctx, out, &[word])
}

#[hle_export(lib = "gx2", name = "GX2SetPolygonControlReg")]
fn gx2_set_polygon_control_reg(ctx: &mut HleContext<'_>, source: u32) -> Result<(), HleError> {
    let [word] = load::<1>(ctx, source)?;
    ctx.os.gx2.set_context(reg::PA_SU_SC_MODE_CNTL, &[word]);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetPolygonControl")]
fn gx2_set_polygon_control(
    ctx: &mut HleContext<'_>,
    front_face: u32,
    cull_front: u32,
    cull_back: u32,
    polygon_mode: u32,
    front_mode: u32,
    back_mode: u32,
    offset_front: u32,
    offset_back: u32,
    offset_para: u32,
) {
    let word = polygon_control(
        front_face,
        cull_front,
        cull_back,
        polygon_mode,
        front_mode,
        back_mode,
        offset_front,
        offset_back,
        offset_para,
    );
    ctx.os.gx2.set_context(reg::PA_SU_SC_MODE_CNTL, &[word]);
}

#[hle_export(lib = "gx2", name = "GX2SetCullOnlyControl")]
fn gx2_set_cull_only_control(
    ctx: &mut HleContext<'_>,
    front_face: u32,
    cull_front: u32,
    cull_back: u32,
) {
    let word = polygon_control(front_face, cull_front, cull_back, 0, 0, 0, 0, 0, 0);
    ctx.os.gx2.set_context(reg::PA_SU_SC_MODE_CNTL, &[word]);
}

fn polygon_offset(
    front_offset: f32,
    front_scale: f32,
    back_offset: f32,
    back_scale: f32,
    clamp: f32,
) -> [u32; 5] {
    [
        (front_scale * 16.0).to_bits(),
        front_offset.to_bits(),
        (back_scale * 16.0).to_bits(),
        back_offset.to_bits(),
        clamp.to_bits(),
    ]
}

fn set_polygon_offset_words(gx2: &mut Gx2State, words: [u32; 5]) {
    gx2.set_context(reg::PA_SU_POLY_OFFSET_FRONT_SCALE, &words[..4]);
    gx2.set_context(reg::PA_SU_POLY_OFFSET_CLAMP, &words[4..]);
}

#[hle_export(lib = "gx2", name = "GX2InitPolygonOffsetReg")]
fn gx2_init_polygon_offset_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    front_offset: f32,
    front_scale: f32,
    back_offset: f32,
    back_scale: f32,
    clamp: f32,
) -> Result<(), HleError> {
    store(ctx, out, &polygon_offset(front_offset, front_scale, back_offset, back_scale, clamp))
}

#[hle_export(lib = "gx2", name = "GX2SetPolygonOffsetReg")]
fn gx2_set_polygon_offset_reg(ctx: &mut HleContext<'_>, source: u32) -> Result<(), HleError> {
    let words = load::<5>(ctx, source)?;
    set_polygon_offset_words(&mut ctx.os.gx2, words);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetPolygonOffset")]
fn gx2_set_polygon_offset(
    ctx: &mut HleContext<'_>,
    front_offset: f32,
    front_scale: f32,
    back_offset: f32,
    back_scale: f32,
    clamp: f32,
) {
    set_polygon_offset_words(
        &mut ctx.os.gx2,
        polygon_offset(front_offset, front_scale, back_offset, back_scale, clamp),
    );
}

/// `PA_CL_CLIP_CNTL`: DX linear attribute clipping is always on.
fn set_clip_control(gx2: &mut Gx2State, rasterizer: u32, z_clip: u32, half_z: u32) {
    let mut word = 1 << 24;
    if z_clip == 0 {
        word |= (1 << 26) | (1 << 27);
    }
    if rasterizer == 0 {
        word |= 1 << 22;
    }
    if half_z != 0 {
        word |= 1 << 19;
    }
    gx2.set_context(reg::PA_CL_CLIP_CNTL, &[word]);
}

#[hle_export(lib = "gx2", name = "GX2SetRasterizerClipControl")]
fn gx2_set_rasterizer_clip_control(ctx: &mut HleContext<'_>, rasterizer: u32, z_clip: u32) {
    set_clip_control(&mut ctx.os.gx2, rasterizer, z_clip, 0);
}

#[hle_export(lib = "gx2", name = "GX2SetRasterizerClipControlHalfZ")]
fn gx2_set_rasterizer_clip_control_half_z(
    ctx: &mut HleContext<'_>,
    rasterizer: u32,
    z_clip: u32,
    half_z: u32,
) {
    set_clip_control(&mut ctx.os.gx2, rasterizer, z_clip, half_z);
}

#[hle_export(lib = "gx2", name = "GX2SetRasterizerClipControlEx")]
fn gx2_set_rasterizer_clip_control_ex(
    ctx: &mut HleContext<'_>,
    rasterizer: u32,
    z_clip: u32,
    half_z: u32,
) {
    set_clip_control(&mut ctx.os.gx2, rasterizer, z_clip, half_z);
}

// ---- Viewport and scissor --------------------------------------------------

fn viewport(x: f32, y: f32, width: f32, height: f32, near: f32, far: f32) -> [u32; 6] {
    [
        width * 0.5,
        width.mul_add(0.5, x),
        height * -0.5,
        height.mul_add(0.5, y),
        (far - near) * 0.5,
        (near + far) * 0.5,
    ]
    .map(f32::to_bits)
}

#[hle_export(lib = "gx2", name = "GX2InitViewportReg")]
fn gx2_init_viewport_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    near: f32,
    far: f32,
) -> Result<(), HleError> {
    store(ctx, out, &viewport(x, y, width, height, near, far))
}

#[hle_export(lib = "gx2", name = "GX2SetViewportReg")]
fn gx2_set_viewport_reg(ctx: &mut HleContext<'_>, source: u32) -> Result<(), HleError> {
    let words = load::<6>(ctx, source)?;
    ctx.os.gx2.set_context(reg::PA_CL_VPORT_XSCALE, &words);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetViewport")]
fn gx2_set_viewport(
    ctx: &mut HleContext<'_>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    near: f32,
    far: f32,
) {
    ctx.os.gx2.set_context(reg::PA_CL_VPORT_XSCALE, &viewport(x, y, width, height, near, far));
}

fn scissor(x: u32, y: u32, width: u32, height: u32) -> [u32; 2] {
    let clamp = |v: u32| v.min(8192);
    let (tlx, tly, brx, bry) =
        (clamp(x), clamp(y), clamp(x.saturating_add(width)), clamp(y.saturating_add(height)));
    [tlx | (tly << 16) | (1 << 31), brx | (bry << 16)]
}

#[hle_export(lib = "gx2", name = "GX2InitScissorReg")]
fn gx2_init_scissor_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<(), HleError> {
    store(ctx, out, &scissor(x, y, width, height))
}

#[hle_export(lib = "gx2", name = "GX2SetScissorReg")]
fn gx2_set_scissor_reg(ctx: &mut HleContext<'_>, source: u32) -> Result<(), HleError> {
    let words = load::<2>(ctx, source)?;
    ctx.os.gx2.set_context(reg::PA_SC_GENERIC_SCISSOR_TL, &words);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2GetScissorReg")]
fn gx2_get_scissor_reg(
    ctx: &mut HleContext<'_>,
    source: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<(), HleError> {
    let [tl, br] = load::<2>(ctx, source)?;
    let (tlx, tly, brx, bry) = (tl & 0x7FFF, (tl >> 16) & 0x7FFF, br & 0x7FFF, (br >> 16) & 0x7FFF);
    for (out, value) in
        [(x, tlx), (y, tly), (width, brx.wrapping_sub(tlx)), (height, bry.wrapping_sub(tly))]
    {
        ctx.memory.write_u32(out, value)?;
    }
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetScissor")]
fn gx2_set_scissor(ctx: &mut HleContext<'_>, x: u32, y: u32, width: u32, height: u32) {
    ctx.os.gx2.set_context(reg::PA_SC_GENERIC_SCISSOR_TL, &scissor(x, y, width, height));
}

// ---- Special state and defaults --------------------------------------------

const VTE_DEFAULT: u32 = 0x43F;
/// `VTX_XY_FMT | VTX_Z_FMT`: vertices arrive in window coordinates.
const VTE_FAST_CLEAR: u32 = 0x300;

pub const EXPORTS: &[HleExport] = &[
    GX2_INIT_ALPHA_TEST_REG,
    GX2_SET_ALPHA_TEST_REG,
    GX2_SET_ALPHA_TEST,
    GX2_INIT_COLOR_CONTROL_REG,
    GX2_SET_COLOR_CONTROL_REG,
    GX2_SET_COLOR_CONTROL,
    GX2_INIT_POLYGON_CONTROL_REG,
    GX2_SET_POLYGON_CONTROL_REG,
    GX2_SET_POLYGON_CONTROL,
    GX2_SET_CULL_ONLY_CONTROL,
    GX2_INIT_POLYGON_OFFSET_REG,
    GX2_SET_POLYGON_OFFSET_REG,
    GX2_SET_POLYGON_OFFSET,
    GX2_SET_RASTERIZER_CLIP_CONTROL,
    GX2_SET_RASTERIZER_CLIP_CONTROL_HALF_Z,
    GX2_SET_RASTERIZER_CLIP_CONTROL_EX,
    GX2_INIT_VIEWPORT_REG,
    GX2_SET_VIEWPORT_REG,
    GX2_SET_VIEWPORT,
    GX2_INIT_SCISSOR_REG,
    GX2_SET_SCISSOR_REG,
    GX2_GET_SCISSOR_REG,
    GX2_SET_SCISSOR,
    GX2_SET_DEPTH_ONLY_CONTROL,
    GX2_SET_DEPTH_STENCIL_CONTROL,
    GX2_INIT_DEPTH_STENCIL_CONTROL_REG,
    GX2_SET_DEPTH_STENCIL_CONTROL_REG,
    GX2_INIT_STENCIL_MASK_REG,
    GX2_SET_STENCIL_MASK,
    GX2_SET_STENCIL_MASK_REG,
    GX2_SET_PRIMITIVE_RESTART_INDEX,
    GX2_INIT_TARGET_CHANNEL_MASKS_REG,
    GX2_SET_TARGET_CHANNEL_MASKS_REG,
    GX2_SET_TARGET_CHANNEL_MASKS,
    GX2_INIT_BLEND_CONTROL_REG,
    GX2_SET_BLEND_CONTROL_REG,
    GX2_SET_BLEND_CONTROL,
    GX2_INIT_BLEND_CONSTANT_COLOR_REG,
    GX2_SET_BLEND_CONSTANT_COLOR_REG,
    GX2_SET_BLEND_CONSTANT_COLOR,
    GX2_SET_POINT_SIZE,
    GX2_SET_POINT_LIMITS,
    GX2_SET_LINE_WIDTH,
    GX2_SET_SPECIAL_STATE,
];

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;

mod depth_stencil_state;
#[allow(unused_imports)]
pub(crate) use depth_stencil_state::*;
