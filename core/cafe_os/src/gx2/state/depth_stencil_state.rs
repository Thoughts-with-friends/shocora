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
//! │ blend, depth,      │   │ gx2/state/depth_stencil_ │   │ register writes    │
//! │ cull…              │   │ state.rs                 │   │                    │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ fixed-function state:    │   │                    │
//! │                    │   │ depth stencil state      │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "depth stencil state" share of state.rs (fixed-function state), moved here so that no
//! file is longer than 500 lines. It works exactly as if it were still written in that file.
//! Settings such as blending, depth test and culling. Each one becomes a register write in the
//! GPU command stream. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;

// ---- Depth and stencil -----------------------------------------------------

pub(super) const fn depth_stencil(
    depth_test: u32,
    depth_write: u32,
    depth_function: u32,
    stencil_test: u32,
    back_stencil_test: u32,
    front_function: u32,
    front_z_pass: u32,
    front_z_fail: u32,
    front_fail: u32,
    back_function: u32,
    back_z_pass: u32,
    back_z_fail: u32,
    back_fail: u32,
) -> u32 {
    (stencil_test & 1)
        | ((depth_test & 1) << 1)
        | ((depth_write & 1) << 2)
        | ((depth_function & 7) << 4)
        | ((back_stencil_test & 1) << 7)
        | ((front_function & 7) << 8)
        | ((front_fail & 7) << 11)
        | ((front_z_pass & 7) << 14)
        | ((front_z_fail & 7) << 17)
        | ((back_function & 7) << 20)
        | ((back_fail & 7) << 23)
        | ((back_z_pass & 7) << 26)
        | ((back_z_fail & 7) << 29)
}

#[hle_export(lib = "gx2", name = "GX2SetDepthOnlyControl")]
pub(super) fn gx2_set_depth_only_control(
    ctx: &mut HleContext<'_>,
    test: u32,
    write: u32,
    function: u32,
) {
    ctx.os.gx2.set_context(
        reg::DB_DEPTH_CONTROL,
        &[depth_stencil(test, write, function, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)],
    );
}

#[hle_export(lib = "gx2", name = "GX2SetDepthStencilControl")]
pub(super) fn gx2_set_depth_stencil_control(
    ctx: &mut HleContext<'_>,
    depth_test: u32,
    depth_write: u32,
    depth_function: u32,
    stencil_test: u32,
    back_stencil_test: u32,
    front_function: u32,
    front_z_pass: u32,
    front_z_fail: u32,
    front_fail: u32,
    back_function: u32,
    back_z_pass: u32,
    back_z_fail: u32,
    back_fail: u32,
) {
    let word = depth_stencil(
        depth_test,
        depth_write,
        depth_function,
        stencil_test,
        back_stencil_test,
        front_function,
        front_z_pass,
        front_z_fail,
        front_fail,
        back_function,
        back_z_pass,
        back_z_fail,
        back_fail,
    );
    ctx.os.gx2.set_context(reg::DB_DEPTH_CONTROL, &[word]);
}

#[hle_export(lib = "gx2", name = "GX2InitDepthStencilControlReg")]
pub(super) fn gx2_init_depth_stencil_control_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    depth_test: u32,
    depth_write: u32,
    depth_function: u32,
    stencil_test: u32,
    back_stencil_test: u32,
    front_function: u32,
    front_z_pass: u32,
    front_z_fail: u32,
    front_fail: u32,
    back_function: u32,
    back_z_pass: u32,
    back_z_fail: u32,
    back_fail: u32,
) -> Result<(), HleError> {
    let word = depth_stencil(
        depth_test,
        depth_write,
        depth_function,
        stencil_test,
        back_stencil_test,
        front_function,
        front_z_pass,
        front_z_fail,
        front_fail,
        back_function,
        back_z_pass,
        back_z_fail,
        back_fail,
    );
    store(ctx, out, &[word])
}

#[hle_export(lib = "gx2", name = "GX2SetDepthStencilControlReg")]
pub(super) fn gx2_set_depth_stencil_control_reg(
    ctx: &mut HleContext<'_>,
    source: u32,
) -> Result<(), HleError> {
    let [word] = load::<1>(ctx, source)?;
    ctx.os.gx2.set_context(reg::DB_DEPTH_CONTROL, &[word]);
    Ok(())
}

pub(super) const fn stencil_mask(compare: u32, write: u32, reference: u32) -> u32 {
    (reference & 0xFF) | ((compare & 0xFF) << 8) | ((write & 0xFF) << 16)
}

#[hle_export(lib = "gx2", name = "GX2InitStencilMaskReg")]
pub(super) fn gx2_init_stencil_mask_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    front_compare: u32,
    front_write: u32,
    front_ref: u32,
    back_compare: u32,
    back_write: u32,
    back_ref: u32,
) -> Result<(), HleError> {
    store(
        ctx,
        out,
        &[
            stencil_mask(front_compare, front_write, front_ref),
            stencil_mask(back_compare, back_write, back_ref),
        ],
    )
}

#[hle_export(lib = "gx2", name = "GX2SetStencilMask")]
pub(super) fn gx2_set_stencil_mask(
    ctx: &mut HleContext<'_>,
    front_compare: u32,
    front_write: u32,
    front_ref: u32,
    back_compare: u32,
    back_write: u32,
    back_ref: u32,
) {
    ctx.os.gx2.set_context(
        reg::DB_STENCILREFMASK,
        &[
            stencil_mask(front_compare, front_write, front_ref),
            stencil_mask(back_compare, back_write, back_ref),
        ],
    );
}

#[hle_export(lib = "gx2", name = "GX2SetStencilMaskReg")]
pub(super) fn gx2_set_stencil_mask_reg(
    ctx: &mut HleContext<'_>,
    source: u32,
) -> Result<(), HleError> {
    let words = load::<2>(ctx, source)?;
    ctx.os.gx2.set_context(reg::DB_STENCILREFMASK, &words);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetPrimitiveRestartIndex")]
pub(super) fn gx2_set_primitive_restart_index(ctx: &mut HleContext<'_>, index: u32) {
    ctx.os.gx2.set_context(reg::VGT_MULTI_PRIM_IB_RESET_INDX, &[index]);
}

// ---- Color targets ---------------------------------------------------------

pub(super) const fn channel_masks(t: [u32; 8]) -> u32 {
    (t[0] & 0xF)
        | ((t[1] & 0xF) << 4)
        | ((t[2] & 0xF) << 8)
        | ((t[3] & 0xF) << 12)
        | ((t[4] & 0xF) << 16)
        | ((t[5] & 0xF) << 20)
        | ((t[6] & 0xF) << 24)
        | ((t[7] & 0xF) << 28)
}

#[hle_export(lib = "gx2", name = "GX2InitTargetChannelMasksReg")]
pub(super) fn gx2_init_target_channel_masks_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    t0: u32,
    t1: u32,
    t2: u32,
    t3: u32,
    t4: u32,
    t5: u32,
    t6: u32,
    t7: u32,
) -> Result<(), HleError> {
    store(ctx, out, &[channel_masks([t0, t1, t2, t3, t4, t5, t6, t7])])
}

#[hle_export(lib = "gx2", name = "GX2SetTargetChannelMasksReg")]
pub(super) fn gx2_set_target_channel_masks_reg(
    ctx: &mut HleContext<'_>,
    source: u32,
) -> Result<(), HleError> {
    let [word] = load::<1>(ctx, source)?;
    ctx.os.gx2.set_context(reg::CB_TARGET_MASK, &[word]);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetTargetChannelMasks")]
pub(super) fn gx2_set_target_channel_masks(
    ctx: &mut HleContext<'_>,
    t0: u32,
    t1: u32,
    t2: u32,
    t3: u32,
    t4: u32,
    t5: u32,
    t6: u32,
    t7: u32,
) {
    ctx.os.gx2.set_context(reg::CB_TARGET_MASK, &[channel_masks([t0, t1, t2, t3, t4, t5, t6, t7])]);
}

pub(super) const fn blend_control(
    color_src: u32,
    color_dst: u32,
    color_fn: u32,
    separate: u32,
    alpha_src: u32,
    alpha_dst: u32,
    alpha_fn: u32,
) -> u32 {
    (color_src & 0x1F)
        | ((color_fn & 7) << 5)
        | ((color_dst & 0x1F) << 8)
        | ((alpha_src & 0x1F) << 16)
        | ((alpha_fn & 7) << 21)
        | ((alpha_dst & 0x1F) << 24)
        | (((separate != 0) as u32) << 29)
}

#[hle_export(lib = "gx2", name = "GX2InitBlendControlReg")]
pub(super) fn gx2_init_blend_control_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    target: u32,
    color_src: u32,
    color_dst: u32,
    color_fn: u32,
    separate: u32,
    alpha_src: u32,
    alpha_dst: u32,
    alpha_fn: u32,
) -> Result<(), HleError> {
    store(
        ctx,
        out,
        &[
            target,
            blend_control(color_src, color_dst, color_fn, separate, alpha_src, alpha_dst, alpha_fn),
        ],
    )
}

#[hle_export(lib = "gx2", name = "GX2SetBlendControlReg")]
pub(super) fn gx2_set_blend_control_reg(
    ctx: &mut HleContext<'_>,
    source: u32,
) -> Result<(), HleError> {
    let [target, word] = load::<2>(ctx, source)?;
    ctx.os.gx2.set_context(reg::CB_BLEND0_CONTROL + (target & 7), &[word]);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetBlendControl")]
pub(super) fn gx2_set_blend_control(
    ctx: &mut HleContext<'_>,
    target: u32,
    color_src: u32,
    color_dst: u32,
    color_fn: u32,
    separate: u32,
    alpha_src: u32,
    alpha_dst: u32,
    alpha_fn: u32,
) {
    let word =
        blend_control(color_src, color_dst, color_fn, separate, alpha_src, alpha_dst, alpha_fn);
    ctx.os.gx2.set_context(reg::CB_BLEND0_CONTROL + (target & 7), &[word]);
}

#[hle_export(lib = "gx2", name = "GX2InitBlendConstantColorReg")]
pub(super) fn gx2_init_blend_constant_color_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
) -> Result<(), HleError> {
    store(ctx, out, &[red, green, blue, alpha].map(f32::to_bits))
}

#[hle_export(lib = "gx2", name = "GX2SetBlendConstantColorReg")]
pub(super) fn gx2_set_blend_constant_color_reg(
    ctx: &mut HleContext<'_>,
    source: u32,
) -> Result<(), HleError> {
    let words = load::<4>(ctx, source)?;
    ctx.os.gx2.set_context(reg::CB_BLEND_RED, &words);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetBlendConstantColor")]
pub(super) fn gx2_set_blend_constant_color(
    ctx: &mut HleContext<'_>,
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
) {
    ctx.os.gx2.set_context(reg::CB_BLEND_RED, &[red, green, blue, alpha].map(f32::to_bits));
}

// ---- Points ----------------------------------------------------------------

pub(super) fn point_pair(first: f32, second: f32) -> u32 {
    let fixed = |v: f32| ((v.max(0.0) * 8.0) as u32).min(0xFFFF);
    fixed(first) | (fixed(second) << 16)
}

#[hle_export(lib = "gx2", name = "GX2SetPointSize")]
pub(super) fn gx2_set_point_size(ctx: &mut HleContext<'_>, width: f32, height: f32) {
    ctx.os.gx2.set_context(reg::PA_SU_POINT_SIZE, &[point_pair(width, height)]);
}

#[hle_export(lib = "gx2", name = "GX2SetPointLimits")]
pub(super) fn gx2_set_point_limits(ctx: &mut HleContext<'_>, min: f32, max: f32) {
    ctx.os.gx2.set_context(reg::PA_SU_POINT_MINMAX, &[point_pair(min, max)]);
}

#[hle_export(lib = "gx2", name = "GX2SetLineWidth")]
pub(super) fn gx2_set_line_width(_ctx: &mut HleContext<'_>, _width: f32) {}

#[hle_export(lib = "gx2", name = "GX2SetSpecialState")]
pub(super) fn gx2_set_special_state(ctx: &mut HleContext<'_>, state: u32, enable: u32) {
    if state != 0 {
        return;
    }
    if enable != 0 {
        ctx.os.gx2.set_context(reg::PA_CL_VTE_CNTL, &[VTE_FAST_CLEAR]);
        ctx.os.gx2.set_context(reg::PA_CL_CLIP_CNTL, &[0x49_0000]);
    } else {
        ctx.os.gx2.set_context(reg::PA_CL_VTE_CNTL, &[VTE_DEFAULT]);
        set_clip_control(&mut ctx.os.gx2, 1, 1, 0);
    }
}

/// What `GX2SetDefaultState` programs.
pub(crate) fn set_default_state(gx2: &mut Gx2State) {
    super::super::draw::set_shader_mode(gx2, 0);
    gx2.set_context(reg::PA_CL_VTE_CNTL, &[VTE_DEFAULT]);
    gx2.set_context(reg::DB_DEPTH_CONTROL, &[depth_stencil(1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)]);
    set_alpha_test_words(gx2, alpha_test(0, 1, 0.0));
    // CCW front face, triangles.
    gx2.set_context(reg::PA_SU_SC_MODE_CNTL, &[polygon_control(0, 0, 0, 0, 2, 2, 0, 0, 0)]);
    set_polygon_offset_words(gx2, [0; 5]);
    gx2.set_context(reg::VGT_MULTI_PRIM_IB_RESET_INDX, &[u32::MAX]);
    gx2.set_context(reg::CB_TARGET_MASK, &[u32::MAX]);
    gx2.set_context(reg::CB_BLEND_RED, &[0; 4]);
    gx2.set_context(reg::PA_SU_POINT_SIZE, &[point_pair(1.0, 1.0)]);
    gx2.set_context(reg::PA_SU_POINT_MINMAX, &[point_pair(1.0, 1.0)]);
    gx2.set_context(reg::CB_COLOR_CONTROL, &[color_control(0xCC, 0, 0, 1)]);
    set_clip_control(gx2, 1, 1, 0);
    gx2.set_context(reg::DB_DEPTH_CLEAR, &[1.0_f32.to_bits()]);
}
