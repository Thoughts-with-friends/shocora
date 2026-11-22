//! `FS*` calls that change the file system, plus the save-data library
//! (`nn_save`), which is a thin path-rewriting layer over them.
#![allow(clippy::too_many_arguments)]

use wiiu_hle_macros::hle_export;
use wiiu_iosu::FsaError;
use wiiu_memory::GuestPtr;

use super::{
    api::{complete_async, finish, path},
    status,
};
use crate::hle::{HleContext, HleError, HleExport};

fn remove(ctx: &mut HleContext<'_>, file: &str) -> i32 {
    finish(ctx.iosu.fsa.remove(file).map(|()| status::OK), "remove", file)
}

fn rename(ctx: &mut HleContext<'_>, from: &str, to: &str) -> i32 {
    finish(ctx.iosu.fsa.rename(from, to).map(|()| status::OK), "rename", from)
}

fn make_dir(ctx: &mut HleContext<'_>, dir: &str) -> i32 {
    match ctx.iosu.fsa.make_dir(dir) {
        Ok(()) => status::OK,
        Err(FsaError::PermissionDenied) => status::PERMISSION_ERROR,
        Err(error) => finish(Err(error), "make_dir", dir),
    }
}

#[hle_export(lib = "coreinit", name = "FSRemove")]
fn fs_remove(
    ctx: &mut HleContext<'_>,
    client: u32,
    _block: u32,
    raw: GuestPtr<u8>,
    _mask: u32,
) -> Result<i32, HleError> {
    let file = path(ctx, client, raw)?;
    Ok(remove(ctx, &file))
}

#[hle_export(lib = "coreinit", name = "FSRemoveAsync")]
fn fs_remove_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    raw: GuestPtr<u8>,
    _mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let file = path(ctx, client, raw)?;
    let status = remove(ctx, &file);
    complete_async(ctx, client, block, params, status)
}

#[hle_export(lib = "coreinit", name = "FSRename")]
fn fs_rename(
    ctx: &mut HleContext<'_>,
    client: u32,
    _block: u32,
    from: GuestPtr<u8>,
    to: GuestPtr<u8>,
    _mask: u32,
) -> Result<i32, HleError> {
    let (from, to) = (path(ctx, client, from)?, path(ctx, client, to)?);
    Ok(rename(ctx, &from, &to))
}

#[hle_export(lib = "coreinit", name = "FSRenameAsync")]
fn fs_rename_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    from: GuestPtr<u8>,
    to: GuestPtr<u8>,
    _mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let (from, to) = (path(ctx, client, from)?, path(ctx, client, to)?);
    let status = rename(ctx, &from, &to);
    complete_async(ctx, client, block, params, status)
}

#[hle_export(lib = "coreinit", name = "FSMakeDir")]
fn fs_make_dir(
    ctx: &mut HleContext<'_>,
    client: u32,
    _block: u32,
    raw: GuestPtr<u8>,
    _mask: u32,
) -> Result<i32, HleError> {
    let dir = path(ctx, client, raw)?;
    Ok(make_dir(ctx, &dir))
}

#[hle_export(lib = "coreinit", name = "FSMakeDirAsync")]
fn fs_make_dir_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    raw: GuestPtr<u8>,
    _mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let dir = path(ctx, client, raw)?;
    let status = make_dir(ctx, &dir);
    complete_async(ctx, client, block, params, status)
}

#[hle_export(lib = "coreinit", name = "FSFlushFile")]
fn fs_flush_file(
    _ctx: &mut HleContext<'_>,
    _client: u32,
    _block: u32,
    _handle: u32,
    _mask: u32,
) -> i32 {
    status::OK
}

#[hle_export(lib = "coreinit", name = "FSFlushFileAsync")]
fn fs_flush_file_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    _handle: u32,
    _mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    complete_async(ctx, client, block, params, status::OK)
}

#[hle_export(lib = "coreinit", name = "FSTruncateFile")]
fn fs_truncate_file(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: u32,
    handle: u32,
    _mask: u32,
) -> i32 {
    finish(ctx.iosu.fsa.truncate(handle).map(|()| status::OK), "FSTruncateFile", "")
}

/// No external storage: nothing can be mounted.
#[hle_export(lib = "coreinit", name = "FSGetMountSource")]
fn fs_get_mount_source(
    _ctx: &mut HleContext<'_>,
    _client: u32,
    _block: u32,
    _kind: u32,
    _out: u32,
    _mask: u32,
) -> i32 {
    status::NOT_FOUND
}

#[hle_export(lib = "coreinit", name = "FSMount")]
fn fs_mount(
    _ctx: &mut HleContext<'_>,
    _client: u32,
    _block: u32,
    _source: u32,
    _target: u32,
    _size: u32,
    _mask: u32,
) -> i32 {
    status::NOT_FOUND
}

#[hle_export(lib = "coreinit", name = "FSGetMountSourceAsync")]
fn fs_get_mount_source_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    _kind: u32,
    _out: u32,
    _mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    complete_async(ctx, client, block, params, status::NOT_FOUND)
}

#[hle_export(lib = "coreinit", name = "FSMountAsync")]
fn fs_mount_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    _source: u32,
    _target: u32,
    _size: u32,
    _mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    complete_async(ctx, client, block, params, status::NOT_FOUND)
}

#[hle_export(lib = "coreinit", name = "FSUnmountAsync")]
fn fs_unmount_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    _target: u32,
    _mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    complete_async(ctx, client, block, params, status::OK)
}

#[hle_export(lib = "coreinit", name = "FSUnmount")]
fn fs_unmount(
    _ctx: &mut HleContext<'_>,
    _client: u32,
    _block: u32,
    _target: u32,
    _mask: u32,
) -> i32 {
    status::OK
}

#[hle_export(lib = "coreinit", name = "FSGetFreeSpaceSize")]
fn fs_get_free_space_size(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: u32,
    _path: u32,
    out: u32,
    _mask: u32,
) -> Result<i32, HleError> {
    ctx.memory.write_u64(out, FREE_SPACE)?;
    Ok(status::OK)
}

pub const EXPORTS: &[HleExport] = &[
    FS_REMOVE,
    FS_REMOVE_ASYNC,
    FS_RENAME,
    FS_RENAME_ASYNC,
    FS_MAKE_DIR,
    FS_MAKE_DIR_ASYNC,
    FS_FLUSH_FILE,
    FS_FLUSH_FILE_ASYNC,
    FS_TRUNCATE_FILE,
    FS_GET_MOUNT_SOURCE,
    FS_MOUNT,
    FS_UNMOUNT,
    FS_GET_MOUNT_SOURCE_ASYNC,
    FS_MOUNT_ASYNC,
    FS_UNMOUNT_ASYNC,
    FS_GET_FREE_SPACE_SIZE,
    SAVE_INIT,
    SAVE_SHUTDOWN,
    SAVE_INIT_SAVE_DIR,
    SAVE_GET_FREE_SPACE_SIZE,
    SAVE_GET_FREE_SPACE_SIZE_ASYNC,
    SAVE_MAKE_DIR,
    SAVE_MAKE_DIR_ASYNC,
    SAVE_REMOVE,
    SAVE_REMOVE_ASYNC,
    SAVE_RENAME,
    SAVE_RENAME_ASYNC,
    SAVE_FLUSH_QUOTA,
    SAVE_ROLLBACK_QUOTA,
    SAVE_ROLLBACK_QUOTA_ASYNC,
    SAVE_CHANGE_DIR,
    SAVE_FLUSH_QUOTA_ASYNC,
    SAVE_GET_STAT,
    SAVE_GET_STAT_ASYNC,
    SAVE_OPEN_FILE,
    SAVE_OPEN_FILE_ASYNC,
    SAVE_OPEN_DIR,
    SAVE_OPEN_DIR_ASYNC,
];

// ---- nn_save ---------------------------------------------------------------

/// Free space reported to titles (they refuse to save when it looks full).
const FREE_SPACE: u64 = 0x4000_0000;
/// `persistent id` of the only account; 0xFF selects the shared `common` folder.
const COMMON_SLOT: u8 = 0xFF;
/// Persistent id of account slot 1, as `nn_act` reports it.
pub(crate) const PERSISTENT_ID: u32 = 0x8000_0001;

fn save_root(slot: u8) -> String {
    if slot == COMMON_SLOT {
        "/vol/save/common".to_owned()
    } else {
        format!("/vol/save/{PERSISTENT_ID:08x}")
    }
}

fn save_path(ctx: &HleContext<'_>, slot: u8, raw: GuestPtr<u8>) -> Result<String, HleError> {
    let relative =
        if raw.is_null() { String::new() } else { ctx.memory.read_c_string(raw.address(), 640)? };
    Ok(format!("{}/{}", save_root(slot), relative.trim_start_matches('/')))
}

#[hle_export(lib = "nn_save", name = "SAVEInit")]
const fn save_init(_ctx: &mut HleContext<'_>) -> i32 {
    status::OK
}

#[hle_export(lib = "nn_save", name = "SAVEShutdown")]
const fn save_shutdown(_ctx: &mut HleContext<'_>) -> i32 {
    status::OK
}

/// Creates the account's and the common save folder.
#[hle_export(lib = "nn_save", name = "SAVEInitSaveDir")]
fn save_init_save_dir(ctx: &mut HleContext<'_>, slot: u8) -> i32 {
    let root = save_root(slot);
    make_dir(ctx, "/vol/save/common");
    make_dir(ctx, &root)
}

#[hle_export(lib = "nn_save", name = "SAVEGetFreeSpaceSize")]
fn save_get_free_space_size(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: u32,
    _slot: u8,
    out: u32,
    _mask: u32,
) -> Result<i32, HleError> {
    ctx.memory.write_u64(out, FREE_SPACE)?;
    Ok(status::OK)
}

#[hle_export(lib = "nn_save", name = "SAVEGetFreeSpaceSizeAsync")]
fn save_get_free_space_size_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    _slot: u8,
    out: u32,
    _mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    ctx.memory.write_u64(out, FREE_SPACE)?;
    complete_async(ctx, client, block, params, status::OK)
}

#[hle_export(lib = "nn_save", name = "SAVEMakeDir")]
fn save_make_dir(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: u32,
    slot: u8,
    raw: GuestPtr<u8>,
    _mask: u32,
) -> Result<i32, HleError> {
    let dir = save_path(ctx, slot, raw)?;
    Ok(make_dir(ctx, &dir))
}

#[hle_export(lib = "nn_save", name = "SAVEMakeDirAsync")]
fn save_make_dir_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    slot: u8,
    raw: GuestPtr<u8>,
    _mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let dir = save_path(ctx, slot, raw)?;
    let status = make_dir(ctx, &dir);
    complete_async(ctx, client, block, params, status)
}

#[hle_export(lib = "nn_save", name = "SAVERemove")]
fn save_remove(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: u32,
    slot: u8,
    raw: GuestPtr<u8>,
    _mask: u32,
) -> Result<i32, HleError> {
    let file = save_path(ctx, slot, raw)?;
    Ok(remove(ctx, &file))
}

#[hle_export(lib = "nn_save", name = "SAVERemoveAsync")]
fn save_remove_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    slot: u8,
    raw: GuestPtr<u8>,
    _mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let file = save_path(ctx, slot, raw)?;
    let status = remove(ctx, &file);
    complete_async(ctx, client, block, params, status)
}

#[hle_export(lib = "nn_save", name = "SAVERename")]
fn save_rename(
    ctx: &mut HleContext<'_>,
    _client: u32,
    _block: u32,
    slot: u8,
    from: GuestPtr<u8>,
    to: GuestPtr<u8>,
    _mask: u32,
) -> Result<i32, HleError> {
    let (from, to) = (save_path(ctx, slot, from)?, save_path(ctx, slot, to)?);
    Ok(rename(ctx, &from, &to))
}

#[hle_export(lib = "nn_save", name = "SAVERenameAsync")]
fn save_rename_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    slot: u8,
    from: GuestPtr<u8>,
    to: GuestPtr<u8>,
    _mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    let (from, to) = (save_path(ctx, slot, from)?, save_path(ctx, slot, to)?);
    let status = rename(ctx, &from, &to);
    complete_async(ctx, client, block, params, status)
}

#[hle_export(lib = "nn_save", name = "SAVEChangeDir")]
fn save_change_dir(
    ctx: &mut HleContext<'_>,
    client: u32,
    _block: u32,
    slot: u8,
    raw: GuestPtr<u8>,
    _mask: u32,
) -> Result<i32, HleError> {
    let dir = save_path(ctx, slot, raw)?;
    ctx.os.coreinit.fs.clients.entry(client).or_default().cwd = dir;
    Ok(status::OK)
}

/// Quotas are not enforced, so there is nothing to commit.
#[hle_export(lib = "nn_save", name = "SAVEFlushQuotaAsync")]
fn save_flush_quota_async(
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
const fn save_flush_quota(
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
const fn save_rollback_quota(
    _ctx: &mut HleContext<'_>,
    _client: u32,
    _block: u32,
    _slot: u8,
    _mask: u32,
) -> i32 {
    status::OK
}

#[hle_export(lib = "nn_save", name = "SAVERollbackQuotaAsync")]
fn save_rollback_quota_async(
    ctx: &mut HleContext<'_>,
    client: u32,
    block: GuestPtr<u8>,
    _slot: u8,
    _mask: u32,
    params: u32,
) -> Result<i32, HleError> {
    complete_async(ctx, client, block, params, status::OK)
}

fn stat_into(ctx: &mut HleContext<'_>, file: &str, out: u32) -> Result<i32, HleError> {
    let result = ctx.iosu.fsa.stat(file);
    if let Ok(stat) = result {
        super::write_stat(ctx.memory, out, stat)?;
    }
    Ok(finish(result.map(|_| status::OK), "SAVEGetStat", file))
}

#[hle_export(lib = "nn_save", name = "SAVEGetStat")]
fn save_get_stat(
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
fn save_get_stat_async(
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

fn open_file(
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
fn save_open_file(
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
fn save_open_file_async(
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

fn open_dir(
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
fn save_open_dir(
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
fn save_open_dir_async(
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

#[cfg(test)]
mod tests {
    use wiiu_fs::{HostDirectory, Vfs};
    use wiiu_iosu::Iosu;

    use super::*;
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn save_files_are_written_below_the_account_folder() {
        let root = std::env::temp_dir().join(format!("shocora-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut vfs = Vfs::new();
        vfs.mount("/vol/save", HostDirectory::writable(&root)).unwrap();
        let mut env = TestEnv::new();
        env.iosu = Iosu::new(vfs);
        let (name, mode, out, data) = (SCRATCH, SCRATCH + 0x100, SCRATCH + 0x200, SCRATCH + 0x300);
        env.memory.write_c_string(name, "slot.bin").unwrap();
        env.memory.write_c_string(mode, "w").unwrap();
        env.memory.write_c_string(data, "abc").unwrap();
        assert_eq!(env.call("nn_save", "SAVEInitSaveDir", &[1]), 0);
        assert_eq!(env.call("nn_save", "SAVEOpenFile", &[0, 0, 1, name, mode, out, 0]), 0);
        let handle = env.memory.read_u32(out).unwrap();
        assert_eq!(env.call("coreinit", "FSWriteFile", &[0, 0, data, 1, 3, handle, 0, 0]), 3);
        env.call("coreinit", "FSCloseFile", &[0, 0, handle, 0]);
        let file = root.join(format!("{PERSISTENT_ID:08x}")).join("slot.bin");
        assert_eq!(std::fs::read(file).unwrap(), b"abc");
        assert_eq!(env.call("nn_save", "SAVERemove", &[0, 0, 1, name, 0]), 0);
        assert_eq!(
            env.call("nn_save", "SAVEGetStat", &[0, 0, 1, name, out, 0]),
            status::NOT_FOUND.cast_unsigned()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Intent: the asynchronous save calls Xenoblade Chronicles X uses do their work and
    /// report it to the callback (a missing one left the title waiting forever).
    #[test]
    fn async_saves_write_rename_and_complete() {
        let root = std::env::temp_dir().join(format!("shocora-save-async-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut vfs = Vfs::new();
        vfs.mount("/vol/save", HostDirectory::writable(&root)).unwrap();
        let mut env = TestEnv::new();
        env.iosu = Iosu::new(vfs);
        let (name, mode, out, data) = (SCRATCH, SCRATCH + 0x100, SCRATCH + 0x200, SCRATCH + 0x300);
        let (renamed, params, block) = (SCRATCH + 0x400, SCRATCH + 0x500, SCRATCH + 0x1000);
        env.memory.write_c_string(name, "tmp.bin").unwrap();
        env.memory.write_c_string(renamed, "slot.bin").unwrap();
        env.memory.write_c_string(mode, "w").unwrap();
        env.memory.write_c_string(data, "abcdef").unwrap();
        env.memory.write_u32(params, 0x0100_0000).unwrap(); // callback
        assert_eq!(env.call("nn_save", "SAVEInitSaveDir", &[1]), 0);
        assert_eq!(env.call("nn_save", "SAVEOpenFile", &[0, 0, 1, name, mode, out, 0]), 0);
        let handle = env.memory.read_u32(out).unwrap();
        let write = [0, block, data, 1, 3, handle, 0, 0, params];
        assert_eq!(env.call("coreinit", "FSWriteFileAsync", &write), 0);
        let write = [0, block, data + 3, 1, 3, 3, handle, 0, 0, params];
        assert_eq!(env.call("coreinit", "FSWriteFileWithPosAsync", &write), 0);
        env.call("coreinit", "FSCloseFile", &[0, 0, handle, 0]);
        let rename = [0, block, 1, name, renamed, 0, params];
        assert_eq!(env.call("nn_save", "SAVERenameAsync", &rename), 0);
        assert_eq!(env.call("nn_save", "SAVERollbackQuotaAsync", &[0, block, 1, 0, params]), 0);
        assert_eq!(env.os.callbacks.pending(), 4);
        let file = root.join(format!("{PERSISTENT_ID:08x}")).join("slot.bin");
        assert_eq!(std::fs::read(file).unwrap(), b"abcdef");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Intent: completions sent to a full message queue are not lost but arrive as the
    /// title takes earlier ones out (Cafe OS's FS thread blocks until there is room).
    #[test]
    fn completions_wait_for_room_in_a_full_queue() {
        let root = std::env::temp_dir().join(format!("shocora-save-queue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut vfs = Vfs::new();
        vfs.mount("/vol/save", HostDirectory::writable(&root)).unwrap();
        let mut env = TestEnv::new();
        env.iosu = Iosu::new(vfs);
        let (queue, array, params, message) =
            (SCRATCH, SCRATCH + 0x100, SCRATCH + 0x200, SCRATCH + 0x300);
        let blocks = [SCRATCH + 0x1000, SCRATCH + 0x2000];
        env.call("coreinit", "OSInitMessageQueue", &[queue, array, 1]);
        env.memory.write_u32(params + 8, queue).unwrap();
        assert_eq!(env.call("nn_save", "SAVEInitSaveDir", &[1]), 0);
        for block in blocks {
            assert_eq!(env.call("nn_save", "SAVEMakeDirAsync", &[0, block, 1, 0, 0, params]), 0);
        }
        for block in blocks {
            assert_eq!(env.call("coreinit", "OSReceiveMessage", &[queue, message, 0]), 1);
            let result = env.memory.read_u32(message).unwrap();
            assert_eq!(result, block + 0x96C);
        }
        assert_eq!(env.call("coreinit", "OSReceiveMessage", &[queue, message, 0]), 0);
        let _ = std::fs::remove_dir_all(&root);
    }
}
