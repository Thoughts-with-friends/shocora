//! Synchronous `FS*` exports.
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
    let target = path(ctx, client, raw_path)?;
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
    let mut bytes = vec![0; size.saturating_mul(count) as usize];
    ctx.memory.read_bytes(src.address(), &mut bytes)?;
    let result = ctx.iosu.fsa.write_file(handle, &bytes);
    Ok(finish(
        result.map(|written| (written as u32).checked_div(size).unwrap_or(0) as i32),
        "FSWriteFile",
        "",
    ))
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

/// Delivers `status` for an async command and returns `FS_STATUS_OK`.
pub(super) fn complete_async(
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
        crate::coreinit::thread::message::send(ctx, queue, result + 0x0C, 0, false)?;
    } else if callback != 0 {
        ctx.os.callbacks.push(crate::hle::callback::GuestCall {
            function: callback,
            args: vec![client, block, status.cast_unsigned(), user_context],
        });
    }
    Ok(status::OK)
}

#[hle_export(lib = "coreinit", name = "FSGetAsyncResult")]
fn fs_get_async_result(ctx: &mut HleContext<'_>, message: u32) -> Result<u32, HleError> {
    Ok(ctx.memory.read_u32(message)?)
}

#[hle_export(lib = "coreinit", name = "FSOpenFileAsync")]
fn fs_open_file_async(
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
fn fs_close_file_async(
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
fn fs_read_file_async(
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
fn fs_read_file_with_pos_async(
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
fn fs_get_stat_async(
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
fn fs_get_stat_file_async(
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
fn fs_set_pos_file_async(
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
fn fs_open_dir_async(
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
fn fs_read_dir_async(
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

#[hle_export(lib = "coreinit", name = "FSCloseDirAsync")]
fn fs_close_dir_async(
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

#[cfg(test)]
mod tests {
    use std::fs;

    use wiiu_fs::{HostDirectory, Vfs};
    use wiiu_iosu::Iosu;

    use crate::{
        coreinit::fs::status,
        hle::test_env::{SCRATCH, TestEnv},
    };

    #[test]
    fn open_read_stat_and_list() {
        let root = std::env::temp_dir().join(format!("shocora-fsapi-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("data")).unwrap();
        fs::write(root.join("data/a.bin"), b"0123456789").unwrap();
        let mut vfs = Vfs::new();
        vfs.mount("/vol/content", HostDirectory::read_only(&root)).unwrap();

        let mut env = TestEnv::new();
        env.iosu = Iosu::new(vfs);
        let (client, block, path, mode, out) =
            (SCRATCH, SCRATCH + 0x2000, SCRATCH + 0x3000, SCRATCH + 0x3100, SCRATCH + 0x3200);
        env.call("coreinit", "FSAddClient", &[client, 0]);
        env.memory.write_c_string(path, "/vol/content/data").unwrap();
        assert_eq!(env.call("coreinit", "FSChangeDir", &[client, block, path, 0]), 0);

        env.memory.write_c_string(path, "a.bin").unwrap();
        env.memory.write_c_string(mode, "r").unwrap();
        assert_eq!(env.call("coreinit", "FSOpenFile", &[client, block, path, mode, out, 0]), 0);
        let handle = env.memory.read_u32(out).unwrap();
        let buffer = SCRATCH + 0x4000;
        assert_eq!(
            env.call("coreinit", "FSReadFile", &[client, block, buffer, 4, 2, handle, 0, 0]),
            2
        );
        assert_eq!(env.memory.read_c_string(buffer, 8).unwrap(), "01234567");
        env.call("coreinit", "FSGetStatFile", &[client, block, handle, out, 0]);
        assert_eq!(env.memory.read_u32(out + 0x10).unwrap(), 10);
        assert_eq!(
            env.call(
                "coreinit",
                "FSReadFileWithPos",
                &[client, block, buffer, 1, 4, 8, handle, 1, 0]
            ),
            2
        );
        assert_eq!(
            env.call("coreinit", "FSIsEof", &[client, block, handle, 0]),
            status::END.cast_unsigned()
        );
        assert_eq!(env.call("coreinit", "FSCloseFile", &[client, block, handle, 0]), 0);

        env.memory.write_c_string(path, "missing.bin").unwrap();
        assert_eq!(
            env.call("coreinit", "FSOpenFile", &[client, block, path, mode, out, 0]),
            status::NOT_FOUND.cast_unsigned()
        );

        env.memory.write_c_string(path, "/vol/content/data").unwrap();
        env.call("coreinit", "FSOpenDir", &[client, block, path, out, 0]);
        let dir = env.memory.read_u32(out).unwrap();
        assert_eq!(env.call("coreinit", "FSReadDir", &[client, block, dir, buffer, 0]), 0);
        assert_eq!(env.memory.read_c_string(buffer + 0x64, 16).unwrap(), "a.bin");
        assert_eq!(
            env.call("coreinit", "FSReadDir", &[client, block, dir, buffer, 0]),
            status::END.cast_unsigned()
        );
        let _ = fs::remove_dir_all(&root);
    }
}
