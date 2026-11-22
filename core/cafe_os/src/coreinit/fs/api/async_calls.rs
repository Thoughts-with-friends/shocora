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
//! │ FS* calls          │   │ coreinit/fs/api/async_ca │   │ file contents,     │
//! │                    │   │ lls.rs                   │   │ listings           │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ reading files & folders: │   │                    │
//! │                    │   │ async calls              │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "async calls" share of api.rs (reading files & folders), moved here so that no file is
//! longer than 500 lines. It works exactly as if it were still written in that file. The file
//! functions a game calls most: open, read, seek, list folders, get file sizes. Terms such as
//! HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.

use super::*;

/// Delivers `status` for an async command and returns `FS_STATUS_OK`.
pub(crate) fn complete_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    params: u32,
    status: i32,
) -> Result<i32, HleError> {
    let callback = ctx.memory.read_u32(params)?;
    let user_context = ctx.memory.read_u32(params + 4)?;
    let queue = ctx.memory.read_u32(params + 8)?;
    let block = block.address();
    if queue != 0 {
        let result = block + ASYNC_RESULT;
        for (offset, value) in [
            (0x00, callback),
            (0x04, user_context),
            (0x08, queue),
            (0x0C, result), // OSMessage: message = &FSAsyncResult
            (0x10, client),
            (0x14, block),
            (0x18, MESSAGE_TYPE_FS),
            (0x1C, client),
            (0x20, block),
            (0x24, status.cast_unsigned()),
        ] {
            ctx.memory.write_u32(result + offset, value)?;
        }
        if !crate::coreinit::thread::message::send(ctx, queue, result + 0x0C, 0, false)? {
            ctx.os.coreinit.fs.undelivered.push((queue, result + 0x0C));
        }
    } else if callback != 0 {
        ctx.os.callbacks.push(crate::hle::callback::GuestCall {
            function: callback,
            args: vec![client, block, status.cast_unsigned(), user_context],
        });
    }
    Ok(status::OK)
}

#[hle_export(lib = "coreinit", name = "FSGetAsyncResult")]
pub(super) fn fs_get_async_result(ctx: &mut HleContext<'_>, message: u32) -> Result<u32, HleError> {
    Ok(ctx.memory.read_u32(message)?)
}

#[hle_export(lib = "coreinit", name = "FSOpenFileAsync")]
pub(super) fn fs_open_file_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    raw_path: GuestPtr<u8>,
    mode: GuestPtr<u8>,
    handle_out: GuestPtr<u32>,
    error_mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let status = fs_open_file(ctx, client, block, raw_path, mode, handle_out, error_mask)?;
    complete_async(ctx, client, block, params, status)
}

#[hle_export(lib = "coreinit", name = "FSCloseFileAsync")]
pub(super) fn fs_close_file_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    handle: u32,
    error_mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let status = fs_close_file(ctx, client, block, handle, error_mask);
    complete_async(ctx, client, block, params, status)
}

#[hle_export(lib = "coreinit", name = "FSReadFileAsync")]
pub(super) fn fs_read_file_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    dst: GuestPtr<u8>,
    size: u32,
    count: u32,
    handle: u32,
    _flag: u32,
    _error_mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let status = read(ctx, dst, size, count, None, handle)?;
    complete_async(ctx, client, block, params, status)
}

#[hle_export(lib = "coreinit", name = "FSReadFileWithPosAsync")]
pub(super) fn fs_read_file_with_pos_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    dst: GuestPtr<u8>,
    size: u32,
    count: u32,
    position: u32,
    handle: u32,
    _flag: u32,
    _error_mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let status = read(ctx, dst, size, count, Some(position), handle)?;
    complete_async(ctx, client, block, params, status)
}

#[hle_export(lib = "coreinit", name = "FSGetStatAsync")]
pub(super) fn fs_get_stat_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    raw_path: GuestPtr<u8>,
    stat_out: GuestPtr<u8>,
    error_mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let status = fs_get_stat(ctx, client, block, raw_path, stat_out, error_mask)?;
    complete_async(ctx, client, block, params, status)
}

#[hle_export(lib = "coreinit", name = "FSGetStatFileAsync")]
pub(super) fn fs_get_stat_file_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    handle: u32,
    stat_out: GuestPtr<u8>,
    error_mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let status = fs_get_stat_file(ctx, client, block, handle, stat_out, error_mask)?;
    complete_async(ctx, client, block, params, status)
}

#[hle_export(lib = "coreinit", name = "FSSetPosFileAsync")]
pub(super) fn fs_set_pos_file_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    handle: u32,
    position: u32,
    error_mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let status = fs_set_pos_file(ctx, client, block, handle, position, error_mask);
    complete_async(ctx, client, block, params, status)
}

#[hle_export(lib = "coreinit", name = "FSOpenDirAsync")]
pub(super) fn fs_open_dir_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    raw_path: GuestPtr<u8>,
    handle_out: GuestPtr<u32>,
    error_mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let status = fs_open_dir(ctx, client, block, raw_path, handle_out, error_mask)?;
    complete_async(ctx, client, block, params, status)
}

#[hle_export(lib = "coreinit", name = "FSReadDirAsync")]
pub(super) fn fs_read_dir_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    handle: u32,
    entry_out: GuestPtr<u8>,
    error_mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let status = fs_read_dir(ctx, client, block, handle, entry_out, error_mask)?;
    complete_async(ctx, client, block, params, status)
}

// Xenoblade Chronicles X writes its saves with these and waits for their completion
// message, so a missing one froze it on every save.
#[hle_export(lib = "coreinit", name = "FSWriteFileAsync")]
pub(super) fn fs_write_file_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    src: GuestPtr<u8>,
    size: u32,
    count: u32,
    handle: u32,
    _flag: u32,
    _error_mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let status = write(ctx, src, size, count, None, handle)?;
    complete_async(ctx, client, block, params, status)
}

#[hle_export(lib = "coreinit", name = "FSWriteFileWithPosAsync")]
pub(super) fn fs_write_file_with_pos_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    src: GuestPtr<u8>,
    size: u32,
    count: u32,
    position: u32,
    handle: u32,
    _flag: u32,
    _error_mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let status = write(ctx, src, size, count, Some(position), handle)?;
    complete_async(ctx, client, block, params, status)
}

/// Every command completes at once, so none is left to cancel.
#[hle_export(lib = "coreinit", name = "FSCancelAllCommands")]
pub(super) fn fs_cancel_all_commands(_ctx: &mut HleContext<'_>, _client: u32) {}

#[hle_export(lib = "coreinit", name = "FSGetLastError")]
pub(super) fn fs_get_last_error(_ctx: &mut HleContext<'_>, _client: u32) -> i32 {
    0
}

#[hle_export(lib = "coreinit", name = "FSGetErrorCodeForViewer")]
pub(super) fn fs_get_error_code_for_viewer(
    _ctx: &mut HleContext<'_>,
    _client: u32,
    _block: u32,
) -> u32 {
    0
}

#[hle_export(lib = "coreinit", name = "FSCloseDirAsync")]
pub(super) fn fs_close_dir_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    handle: u32,
    error_mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let status = fs_close_dir(ctx, client, block, handle, error_mask);
    complete_async(ctx, client, block, params, status)
}
