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
//! │ finished command   │   │ gx2/command/display_list │   │ commands sent to   │
//! │ lists              │   │ .rs                      │   │ GPU                │
//! │                    │──►│ ──────────────────────── │──►│ frame flips        │
//! │                    │   │ command submission:      │   │                    │
//! │                    │   │ display list             │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "display list" share of command.rs (command submission), moved here so that no file is
//! longer than 500 lines. It works exactly as if it were still written in that file. Hands the
//! commands GX2 wrote to the GPU, and handles display lists, timestamps and showing finished
//! frames. Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.

use super::*;

/// Redirects a recorded `SQ_PGM_START_*` to the shader's current program.
#[hle_export(lib = "gx2", name = "GX2PatchDisplayList")]
pub(super) fn gx2_patch_display_list(
    ctx: &mut HleContext<'_>,
    list: u32,
    kind: u32,
    offset: u32,
    object: u32,
) -> Result<(), HleError> {
    let program = match kind {
        PATCH_FETCH_SHADER => ctx.memory.read_u32(object + 0x0C)?,
        PATCH_VERTEX_SHADER => match ctx.memory.read_u32(object + 0xD4)? {
            0 => ctx.memory.read_u32(object + 0x124 + 0x0C)?,
            address => address,
        },
        PATCH_PIXEL_SHADER => match ctx.memory.read_u32(object + 0xA8)? {
            0 => ctx.memory.read_u32(object + 0xD8 + 0x0C)?,
            address => address,
        },
        other => {
            log::warn!("GX2PatchDisplayList: unsupported patch type {other}");
            return Ok(());
        }
    };
    Ok(ctx.memory.write_u32(list + (offset & !3) + 8, physical(program) >> 8)?)
}

// ---- Context states --------------------------------------------------------

#[hle_export(lib = "gx2", name = "GX2SetDefaultState")]
pub(super) fn gx2_set_default_state(ctx: &mut HleContext<'_>) {
    super::super::state::set_default_state(&mut ctx.os.gx2);
}

/// A context starts from the default state; the GPU keeps its copy of the registers
/// host-side, so the guest structure only holds the display list that restores it.
#[hle_export(lib = "gx2", name = "GX2SetupContextStateEx")]
pub(super) fn gx2_setup_context_state_ex(
    ctx: &mut HleContext<'_>,
    state: u32,
    _profiling: u32,
) -> Result<(), HleError> {
    super::super::state::set_default_state(&mut ctx.os.gx2);
    ctx.os.gx2.emit(opcode::HLE_CONTEXT_STATE, &[0, state]);
    let list = [type3_header(opcode::HLE_CONTEXT_STATE, 2), 1, state];
    for (i, word) in list.iter().enumerate() {
        ctx.memory.write_u32(state + CONTEXT_LOAD_LIST + 4 * i as u32, *word)?;
    }
    ctx.memory.write_u32(state + CONTEXT_LOAD_LIST_SIZE, 4 * list.len() as u32)?;
    Ok(())
}

/// The list titles call themselves instead of `GX2SetContextState`.
#[hle_export(lib = "gx2", name = "GX2GetContextStateDisplayList")]
pub(super) fn gx2_get_context_state_display_list(
    ctx: &mut HleContext<'_>,
    state: u32,
    list: u32,
    size: u32,
) -> Result<(), HleError> {
    ctx.memory.write_u32(list, state + CONTEXT_LOAD_LIST)?;
    let bytes = ctx.memory.read_u32(state + CONTEXT_LOAD_LIST_SIZE)?;
    Ok(ctx.memory.write_u32(size, bytes)?)
}

#[hle_export(lib = "gx2", name = "GX2SetContextState")]
pub(super) fn gx2_set_context_state(ctx: &mut HleContext<'_>, state: u32) {
    ctx.os.gx2.emit(opcode::HLE_CONTEXT_STATE, &[1, state]);
}
