//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ coreinit/fs/mutate_tests │   │ pass / fail        │
//! │                    │──►│ .rs                      │──►│                    │
//! │                    │   │ ──────────────────────── │   │                    │
//! │                    │   │ tests of mutate.rs       │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for mutate.rs (changing files, saves). They run with `cargo test` only and are
//! not part of the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

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
