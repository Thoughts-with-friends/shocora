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
//! │ remaining GX2      │   │ gx2/misc/compute_exports │   │ GPU state, queries │
//! │ calls              │   │ .rs                      │   │                    │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ other GX2 functions:     │   │                    │
//! │                    │   │ compute exports          │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "compute exports" share of misc.rs (other GX2 functions), moved here so that no file is
//! longer than 500 lines. It works exactly as if it were still written in that file. The rest
//! of GX2: queries, events, compute, a few state setters. Terms such as HLE, JIT or shader are
//! explained in `docs/guide/wiiu_term.md`.

use super::*;

#[hle_export(lib = "gx2", name = "GX2SetComputeTexture")]
pub(super) fn gx2_set_compute_texture(
    ctx: &mut HleContext<'_>,
    tex: u32,
    unit: u32,
) -> Result<(), HleError> {
    super::super::draw::gx2_set_vertex_texture(ctx, tex, unit)
}

#[hle_export(lib = "gx2", name = "GX2SetComputeSampler")]
pub(super) fn gx2_set_compute_sampler(
    ctx: &mut HleContext<'_>,
    sampler: u32,
    unit: u32,
) -> Result<(), HleError> {
    super::super::draw::gx2_set_vertex_sampler(ctx, sampler, unit)
}

#[hle_export(lib = "gx2", name = "GX2SetGeometrySamplerBorderColor")]
pub(super) fn gx2_set_geometry_sampler_border_color(
    ctx: &mut HleContext<'_>,
    unit: u32,
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
) {
    super::super::draw::set_border_color(
        &mut ctx.os.gx2,
        config::TD_GS_SAMPLER0_BORDER_RED,
        unit,
        [red, green, blue, alpha],
    );
}

/// Only whole-surface copies are supported, as in Cemu: they act like `GX2CopySurface`.
#[hle_export(lib = "gx2", name = "GX2CopySurfaceEx")]
pub(super) fn gx2_copy_surface_ex(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
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
            super::super::display::gx2_copy_surface(
                ctx, src, src_mip, src_slice, dst, dst_mip, dst_slice,
            )?;
        } else {
            log::warn!("GX2CopySurfaceEx: partial rectangle copies are not supported");
        }
    }
    Ok(())
}
