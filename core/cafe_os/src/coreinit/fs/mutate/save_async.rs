//! Part of [`super`]: see its documentation.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ write/rename/delet │   │ coreinit/fs/mutate/save_ │   │ updated save data  │
//! │ e                  │   │ async.rs                 │   │                    │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ changing files, saves:   │   │                    │
//! │                    │   │ save async               │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "save async" share of mutate.rs (changing files, saves), moved here so that no file is
//! longer than 500 lines. It works exactly as if it were still written in that file. File
//! functions that change things, plus the save-data library that points the game at your save
//! folder. Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.

use super::*;

/// Quotas are not enforced, so there is nothing to commit.
#[hle_export(lib = "nn_save", name = "SAVEFlushQuotaAsync")]
pub(super) fn save_flush_quota_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    _slot: u8,
    _mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    complete_async(ctx, client, block, params, status::OK)
}

#[hle_export(lib = "nn_save", name = "SAVEFlushQuota")]
pub(super) const fn save_flush_quota(
    _ctx: &mut HleContext<'_>,
    _client: u32,
    _block: u32,
    _slot: u8,
    _mask: u32,
) -> i32 {
    status::OK
}

/// Writes reach the host at once, so there is nothing to roll back either.
#[hle_export(lib = "nn_save", name = "SAVERollbackQuota")]
pub(super) const fn save_rollback_quota(
    _ctx: &mut HleContext<'_>,
    _client: u32,
    _block: u32,
    _slot: u8,
    _mask: u32,
) -> i32 {
    status::OK
}

#[hle_export(lib = "nn_save", name = "SAVERollbackQuotaAsync")]
pub(super) fn save_rollback_quota_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    _slot: u8,
    _mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    complete_async(ctx, client, block, params, status::OK)
}

pub(super) fn stat_into(ctx: &mut HleContext<'_>, file: &str, out: u32) -> Result<i32, HleError> {
    let result = ctx.iosu.fsa.stat(file);
    if let Ok(stat) = result {
        super::super::write_stat(ctx.memory, out, stat)?;
    }
    Ok(finish(result.map(|_| status::OK), "SAVEGetStat", file))
}

#[hle_export(lib = "nn_save", name = "SAVEGetStat")]
pub(super) fn save_get_stat(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: u32,
    slot: u8,
    raw: GuestPtr<u8>,
    out: u32,
    _mask: u32,
) -> Result<i32, HleError> {
    let file = save_path(ctx, slot, raw)?;
    stat_into(ctx, &file, out)
}

#[hle_export(lib = "nn_save", name = "SAVEGetStatAsync")]
pub(super) fn save_get_stat_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    slot: u8,
    raw: GuestPtr<u8>,
    out: u32,
    _mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let file = save_path(ctx, slot, raw)?;
    let status = stat_into(ctx, &file, out)?;
    complete_async(ctx, client, block, params, status)
}

pub(super) fn open_file(
    ctx: &mut HleContext<'_>,
    file: &str,
    mode: GuestPtr<u8>,
    handle_out: GuestPtr<u32>,
) -> Result<i32, HleError> {
    let mode = ctx.memory.read_c_string(mode.address(), 16)?;
    let result = ctx.iosu.fsa.open_file(file, &mode);
    if let Ok(handle) = result {
        handle_out.write(ctx.memory, handle)?;
    }
    Ok(finish(result.map(|_| status::OK), "SAVEOpenFile", file))
}

#[hle_export(lib = "nn_save", name = "SAVEOpenFile")]
pub(super) fn save_open_file(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: u32,
    slot: u8,
    raw: GuestPtr<u8>,
    mode: GuestPtr<u8>,
    handle_out: GuestPtr<u32>,
    _mask: u32,
) -> Result<i32, HleError> {
    let file = save_path(ctx, slot, raw)?;
    open_file(ctx, &file, mode, handle_out)
}

#[hle_export(lib = "nn_save", name = "SAVEOpenFileAsync")]
pub(super) fn save_open_file_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    slot: u8,
    raw: GuestPtr<u8>,
    mode: GuestPtr<u8>,
    handle_out: GuestPtr<u32>,
    _mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let file = save_path(ctx, slot, raw)?;
    let status = open_file(ctx, &file, mode, handle_out)?;
    complete_async(ctx, client, block, params, status)
}

pub(super) fn open_dir(
    ctx: &mut HleContext<'_>,
    dir: &str,
    handle_out: GuestPtr<u32>,
) -> Result<i32, HleError> {
    let result = ctx.iosu.fsa.open_dir(dir);
    if let Ok(handle) = result {
        handle_out.write(ctx.memory, handle)?;
    }
    Ok(finish(result.map(|_| status::OK), "SAVEOpenDir", dir))
}

#[hle_export(lib = "nn_save", name = "SAVEOpenDir")]
pub(super) fn save_open_dir(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: u32,
    slot: u8,
    raw: GuestPtr<u8>,
    handle_out: GuestPtr<u32>,
    _mask: u32,
) -> Result<i32, HleError> {
    let dir = save_path(ctx, slot, raw)?;
    open_dir(ctx, &dir, handle_out)
}

#[hle_export(lib = "nn_save", name = "SAVEOpenDirAsync")]
pub(super) fn save_open_dir_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    slot: u8,
    raw: GuestPtr<u8>,
    handle_out: GuestPtr<u32>,
    _mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let dir = save_path(ctx, slot, raw)?;
    let status = open_dir(ctx, &dir, handle_out)?;
    complete_async(ctx, client, block, params, status)
}
