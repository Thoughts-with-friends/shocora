//! Synchronous `FS*` exports.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ FS* calls          │   │ coreinit/fs/api.rs       │   │ file contents,     │
//! │                    │──►│ ──────────────────────── │──►│ listings           │
//! │                    │   │ reading files & folders  │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The file functions a game calls most: open, read, seek, list folders, get file sizes. Terms
//! such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap, clippy::too_many_arguments)]

use wiiu_hle_macros::hle_export;
use wiiu_iosu::FsaError;
use wiiu_memory::GuestPtr;

use super::{Client, DIR_ENTRY_NAME, DIR_ENTRY_NAME_SIZE, status, status_of, write_stat};
use crate::hle::{HleContext, HleError};

/// Longest path read from the guest.
const MAX_PATH: u32 = 640;
/// `FS_VOLSTATE_READY`.
const VOLUME_READY: i32 = 1;

pub(super) fn path(
    ctx: &HleContext<'_>,
    client: u32,
    raw: GuestPtr<u8>,
) -> Result<String, HleError> {
    let text = ctx.memory.read_c_string(raw.address(), MAX_PATH)?;
    Ok(ctx.os.coreinit.fs.resolve(client, &text))
}

/// Converts an FSA result to `FSStatus`, logging failures the caller did not
/// expect (real Cafe OS would raise a fatal error for unmasked errors).
pub(super) fn finish(result: Result<i32, FsaError>, operation: &str, detail: &str) -> i32 {
    match result {
        Ok(value) => value,
        Err(FsaError::EndOfDirectory) => status::END,
        Err(error) => {
            log::debug!("{operation}({detail}): {error}");
            status_of(error)
        }
    }
}

#[hle_export(lib = "coreinit", name = "FSInit")]
fn fs_init(ctx: &mut HleContext<'_>) {
    ctx.os.coreinit.fs.initialized = true;
}

#[hle_export(lib = "coreinit", name = "FSShutdown")]
fn fs_shutdown(ctx: &mut HleContext<'_>) {
    ctx.os.coreinit.fs.initialized = false;
}

#[hle_export(lib = "coreinit", name = "FSAddClient")]
fn fs_add_client(ctx: &mut HleContext<'_>, client: u32, _error_mask: u32) -> i32 {
    ctx.os.coreinit.fs.clients.insert(client, Client::default());
    status::OK
}

#[hle_export(lib = "coreinit", name = "FSAddClientEx")]
fn fs_add_client_ex(ctx: &mut HleContext<'_>, client: u32, _attach: u32, _error_mask: u32) -> i32 {
    ctx.os.coreinit.fs.clients.insert(client, Client::default());
    status::OK
}

#[hle_export(lib = "coreinit", name = "FSDelClient")]
fn fs_del_client(ctx: &mut HleContext<'_>, client: u32, _error_mask: u32) -> i32 {
    ctx.os.coreinit.fs.clients.remove(&client);
    status::OK
}

#[hle_export(lib = "coreinit", name = "FSGetClientNum")]
fn fs_get_client_num(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.coreinit.fs.clients.len() as u32
}

/// Command blocks carry no host state; nothing to initialise.
#[hle_export(lib = "coreinit", name = "FSSetCmdPriority")]
fn fs_set_cmd_priority(_ctx: &mut HleContext<'_>, _block: GuestPtr<u8>, _priority: u32) -> i32 {
    status::OK
}

#[hle_export(lib = "coreinit", name = "FSInitCmdBlock")]
fn fs_init_cmd_block(_ctx: &mut HleContext<'_>, _block: GuestPtr<u8>) {}

#[hle_export(lib = "coreinit", name = "FSSetStateChangeNotification")]
fn fs_set_state_change_notification(_ctx: &mut HleContext<'_>, _client: u32, _info: GuestPtr<u8>) {}

#[hle_export(lib = "coreinit", name = "FSGetVolumeState")]
fn fs_get_volume_state(_ctx: &mut HleContext<'_>, _client: u32) -> i32 {
    VOLUME_READY
}

#[hle_export(lib = "coreinit", name = "FSGetLastErrorCodeForViewer")]
fn fs_get_last_error_code_for_viewer(_ctx: &mut HleContext<'_>, _client: u32) -> i32 {
    0
}

#[hle_export(lib = "coreinit", name = "FSGetCwd")]
fn fs_get_cwd(
    ctx: &mut HleContext<'_>,
    client: u32,
    _block: GuestPtr<u8>,
    buffer: GuestPtr<u8>,
    size: u32,
    _error_mask: u32,
) -> Result<i32, HleError> {
    let cwd =
        ctx.os.coreinit.fs.clients.get(&client).map_or_else(|| "/".to_owned(), |c| c.cwd.clone());
    if size > 0 {
        let text: String = cwd.chars().take(size as usize - 1).collect();
        ctx.memory.write_c_string(buffer.address(), &text)?;
    }
    Ok(status::OK)
}

#[hle_export(lib = "coreinit", name = "FSChangeDir")]
fn fs_change_dir(
    ctx: &mut HleContext<'_>,
    client: u32,
    _block: GuestPtr<u8>,
    raw_path: GuestPtr<u8>,
    _error_mask: u32,
) -> Result<i32, HleError> {
    let mut target = path(ctx, client, raw_path)?;
    // The working directory ends with a slash, as FSGetCwd hands it out: titles append
    // their relative paths to it (Paper Mario: Color Splash's stream sounds).
    if !target.ends_with('/') {
        target.push('/');
    }
    ctx.os.coreinit.fs.clients.entry(client).or_default().cwd = target;
    Ok(status::OK)
}

#[hle_export(lib = "coreinit", name = "FSOpenFile")]
fn fs_open_file(
    ctx: &mut HleContext<'_>,
    client: u32,
    _block: GuestPtr<u8>,
    raw_path: GuestPtr<u8>,
    mode: GuestPtr<u8>,
    handle_out: GuestPtr<u32>,
    _error_mask: u32,
) -> Result<i32, HleError> {
    let file = path(ctx, client, raw_path)?;
    let mode = ctx.memory.read_c_string(mode.address(), 16)?;
    log::trace!("FSOpenFile({file}, {mode})");
    let result = ctx.iosu.fsa.open_file(&file, &mode);
    if let Ok(handle) = result {
        handle_out.write(ctx.memory, handle)?;
    }
    Ok(finish(result.map(|_| status::OK), "FSOpenFile", &file))
}

#[hle_export(lib = "coreinit", name = "FSCloseFile")]
fn fs_close_file(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: GuestPtr<u8>,
    handle: u32,
    _mask: u32,
) -> i32 {
    finish(ctx.iosu.fsa.close_file(handle).map(|()| status::OK), "FSCloseFile", "")
}

/// Shared body of `FSReadFile` / `FSReadFileWithPos`: returns elements read.
fn read(
    ctx: &mut HleContext<'_>,
    dst: GuestPtr<u8>,
    size: u32,
    count: u32,
    position: Option<u32>,
    handle: u32,
) -> Result<i32, HleError> {
    if let Some(position) = position {
        if let Err(error) = ctx.iosu.fsa.set_position(handle, u64::from(position)) {
            return Ok(finish(Err(error), "FSReadFile", "seek"));
        }
    }
    let len = size.saturating_mul(count) as usize;
    match ctx.iosu.fsa.read_file(handle, len) {
        Ok(bytes) => {
            ctx.memory.write_bytes(dst.address(), &bytes)?;
            ctx.gpu.note_cpu_write(dst.address(), bytes.len() as u32);
            Ok((bytes.len() as u32).checked_div(size).unwrap_or(0) as i32)
        }
        Err(error) => Ok(finish(Err(error), "FSReadFile", "")),
    }
}

#[hle_export(lib = "coreinit", name = "FSReadFile")]
fn fs_read_file(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: GuestPtr<u8>,
    dst: GuestPtr<u8>,
    size: u32,
    count: u32,
    handle: u32,
    _flag: u32,
    _error_mask: u32,
) -> Result<i32, HleError> {
    read(ctx, dst, size, count, None, handle)
}

#[hle_export(lib = "coreinit", name = "FSReadFileWithPos")]
fn fs_read_file_with_pos(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: GuestPtr<u8>,
    dst: GuestPtr<u8>,
    size: u32,
    count: u32,
    position: u32,
    handle: u32,
    _flag: u32,
    _error_mask: u32,
) -> Result<i32, HleError> {
    read(ctx, dst, size, count, Some(position), handle)
}

/// Shared body of the `FSWriteFile*` calls: returns elements written.
fn write(
    ctx: &mut HleContext<'_>,
    src: GuestPtr<u8>,
    size: u32,
    count: u32,
    position: Option<u32>,
    handle: u32,
) -> Result<i32, HleError> {
    if let Some(position) = position {
        if let Err(error) = ctx.iosu.fsa.set_position(handle, u64::from(position)) {
            return Ok(finish(Err(error), "FSWriteFile", "seek"));
        }
    }
    let mut bytes = vec![0; size.saturating_mul(count) as usize];
    ctx.memory.read_bytes(src.address(), &mut bytes)?;
    let result = ctx.iosu.fsa.write_file(handle, &bytes);
    Ok(finish(
        result.map(|written| (written as u32).checked_div(size).unwrap_or(0) as i32),
        "FSWriteFile",
        "",
    ))
}

#[hle_export(lib = "coreinit", name = "FSWriteFile")]
fn fs_write_file(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: GuestPtr<u8>,
    src: GuestPtr<u8>,
    size: u32,
    count: u32,
    handle: u32,
    _flag: u32,
    _error_mask: u32,
) -> Result<i32, HleError> {
    write(ctx, src, size, count, None, handle)
}

#[hle_export(lib = "coreinit", name = "FSWriteFileWithPos")]
fn fs_write_file_with_pos(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: GuestPtr<u8>,
    src: GuestPtr<u8>,
    size: u32,
    count: u32,
    position: u32,
    handle: u32,
    _flag: u32,
    _error_mask: u32,
) -> Result<i32, HleError> {
    write(ctx, src, size, count, Some(position), handle)
}

#[hle_export(lib = "coreinit", name = "FSGetPosFile")]
fn fs_get_pos_file(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: GuestPtr<u8>,
    handle: u32,
    position_out: GuestPtr<u32>,
    _error_mask: u32,
) -> Result<i32, HleError> {
    let result = ctx.iosu.fsa.file_info(handle);
    if let Ok((position, _)) = result {
        position_out.write(ctx.memory, position as u32)?;
    }
    Ok(finish(result.map(|_| status::OK), "FSGetPosFile", ""))
}

#[hle_export(lib = "coreinit", name = "FSSetPosFile")]
fn fs_set_pos_file(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: GuestPtr<u8>,
    handle: u32,
    position: u32,
    _error_mask: u32,
) -> i32 {
    finish(
        ctx.iosu.fsa.set_position(handle, u64::from(position)).map(|()| status::OK),
        "FSSetPosFile",
        "",
    )
}

#[hle_export(lib = "coreinit", name = "FSIsEof")]
fn fs_is_eof(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: GuestPtr<u8>,
    handle: u32,
    _error_mask: u32,
) -> i32 {
    let result = ctx.iosu.fsa.file_info(handle);
    finish(
        result.map(|(position, size)| if position >= size { status::END } else { status::OK }),
        "FSIsEof",
        "",
    )
}

#[hle_export(lib = "coreinit", name = "FSGetStatFile")]
fn fs_get_stat_file(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: GuestPtr<u8>,
    handle: u32,
    stat_out: GuestPtr<u8>,
    _error_mask: u32,
) -> Result<i32, HleError> {
    let result = ctx.iosu.fsa.file_info(handle);
    if let Ok((_, size)) = result {
        write_stat(
            ctx.memory,
            stat_out.address(),
            wiiu_iosu::FsaStat { is_directory: false, size },
        )?;
    }
    Ok(finish(result.map(|_| status::OK), "FSGetStatFile", ""))
}

#[hle_export(lib = "coreinit", name = "FSGetStat")]
fn fs_get_stat(
    ctx: &mut HleContext<'_>,
    client: u32,
    _block: GuestPtr<u8>,
    raw_path: GuestPtr<u8>,
    stat_out: GuestPtr<u8>,
    _error_mask: u32,
) -> Result<i32, HleError> {
    let file = path(ctx, client, raw_path)?;
    log::trace!("FSGetStat({file})");
    let result = ctx.iosu.fsa.stat(&file);
    if let Ok(stat) = result {
        write_stat(ctx.memory, stat_out.address(), stat)?;
    }
    Ok(finish(result.map(|_| status::OK), "FSGetStat", &file))
}

#[hle_export(lib = "coreinit", name = "FSOpenDir")]
fn fs_open_dir(
    ctx: &mut HleContext<'_>,
    client: u32,
    _block: GuestPtr<u8>,
    raw_path: GuestPtr<u8>,
    handle_out: GuestPtr<u32>,
    _error_mask: u32,
) -> Result<i32, HleError> {
    let dir = path(ctx, client, raw_path)?;
    let result = ctx.iosu.fsa.open_dir(&dir);
    if let Ok(handle) = result {
        handle_out.write(ctx.memory, handle)?;
    }
    Ok(finish(result.map(|_| status::OK), "FSOpenDir", &dir))
}

/// Fills an `FSDirEntry`; returns `FS_STATUS_END` after the last entry.
#[hle_export(lib = "coreinit", name = "FSReadDir")]
fn fs_read_dir(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: GuestPtr<u8>,
    handle: u32,
    entry_out: GuestPtr<u8>,
    _error_mask: u32,
) -> Result<i32, HleError> {
    let result = ctx.iosu.fsa.read_dir(handle);
    if let Ok((name, stat)) = &result {
        write_stat(ctx.memory, entry_out.address(), *stat)?;
        let name: String = name.chars().take(DIR_ENTRY_NAME_SIZE - 1).collect();
        ctx.memory.write_c_string(entry_out.address() + DIR_ENTRY_NAME, &name)?;
    }
    Ok(finish(result.map(|_| status::OK), "FSReadDir", ""))
}

#[hle_export(lib = "coreinit", name = "FSRewindDir")]
fn fs_rewind_dir(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: GuestPtr<u8>,
    handle: u32,
    _mask: u32,
) -> i32 {
    finish(ctx.iosu.fsa.rewind_dir(handle).map(|()| status::OK), "FSRewindDir", "")
}

#[hle_export(lib = "coreinit", name = "FSCloseDir")]
fn fs_close_dir(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: GuestPtr<u8>,
    handle: u32,
    _mask: u32,
) -> i32 {
    finish(ctx.iosu.fsa.close_dir(handle).map(|()| status::OK), "FSCloseDir", "")
}

// ---- Asynchronous variants -------------------------------------------------
//
// The operation completes immediately; its result is then delivered like
// Cafe OS does: as an `OSMessage` pointing at the command block's
// `FSAsyncResult`, or by calling the user callback.

/// Offset of `FSAsyncResult` inside `FSCmdBlock`.
const ASYNC_RESULT: u32 = 0x96C;
/// `OSFunctionType` of FS completion messages.
const MESSAGE_TYPE_FS: u32 = 8;

#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;

mod async_calls;
#[allow(unused_imports)]
pub(crate) use async_calls::*;
