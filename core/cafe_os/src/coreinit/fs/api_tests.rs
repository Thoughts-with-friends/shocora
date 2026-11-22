//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ coreinit/fs/api_tests.rs │   │ pass / fail        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tests of api.rs          │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for api.rs (reading files & folders). They run with `cargo test` only and are not
//! part of the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

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
    assert_eq!(env.call("coreinit", "FSReadFile", &[client, block, buffer, 4, 2, handle, 0, 0]), 2);
    assert_eq!(env.memory.read_c_string(buffer, 8).unwrap(), "01234567");
    env.call("coreinit", "FSGetStatFile", &[client, block, handle, out, 0]);
    assert_eq!(env.memory.read_u32(out + 0x10).unwrap(), 10);
    assert_eq!(
        env.call("coreinit", "FSReadFileWithPos", &[client, block, buffer, 1, 4, 8, handle, 1, 0]),
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
