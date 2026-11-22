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
//! │ GX2R buffer calls  │   │ gx2/resource/display_lis │   │ buffers in game    │
//! │                    │   │ t.rs                     │   │ memory             │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ GX2R resources: display  │   │                    │
//! │                    │   │ list                     │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "display list" share of resource.rs (GX2R resources), moved here so that no file is
//! longer than 500 lines. It works exactly as if it were still written in that file. A helper
//! layer for buffers and images whose memory comes from the game's own allocator. Terms such as
//! HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.

use super::*;

#[hle_export(lib = "gx2", name = "GX2RDirectCallDisplayList")]
pub(super) fn gx2r_direct_call_display_list(
    ctx: &mut HleContext<'_>,
    address: u32,
    size: u32,
) -> Result<(), HleError> {
    let ptr = read_buffer(ctx, address)?.ptr;
    super::super::command::gx2_direct_call_display_list(ctx, ptr, size)
}

#[hle_export(lib = "gx2", name = "GX2RDrawIndexed")]
#[allow(clippy::too_many_arguments)]
pub(super) fn gx2r_draw_indexed(
    ctx: &mut HleContext<'_>,
    mode: u32,
    index_buffer: u32,
    index_type: u32,
    count: u32,
    base_index: u32,
    base_vertex: u32,
    instances: u32,
) -> Result<(), HleError> {
    let b = read_buffer(ctx, index_buffer)?;
    let indices = b.ptr.wrapping_add(base_index.wrapping_mul(b.element_size));
    super::super::draw::gx2_draw_indexed_ex(
        ctx,
        mode,
        count,
        index_type,
        indices,
        base_vertex,
        instances,
    )
}
