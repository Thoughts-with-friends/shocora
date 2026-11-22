//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ zlib125_tests.rs         │   │ pass / fail        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tests of zlib125.rs      │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for zlib125.rs (zlib (de)compression). They run with `cargo test` only and are
//! not part of the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use std::io::Write;

use super::*;
use crate::hle::test_env::{SCRATCH, TestEnv};

#[test]
fn missing_buffers_are_an_error_not_a_crash() {
    let mut env = TestEnv::new();
    let stream = SCRATCH;
    env.call("zlib125", "inflateInit_", &[stream, 0, 0x38]);
    // No input although some is promised, as when a title's allocation failed.
    for (field, value) in [(0x00, 0), (0x04, 0x480_2000), (0x0C, SCRATCH + 0x400), (0x10, 0x100)] {
        env.memory.write_u32(stream + field, value).unwrap();
    }
    assert_eq!(env.call("zlib125", "inflate", &[stream, Z_FINISH]), Z_STREAM_ERROR.cast_unsigned());
}

#[test]
fn inflate_streams_into_guest_memory() {
    let mut env = TestEnv::new();
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(b"hello hello hello shocora").unwrap();
    let packed = encoder.finish().unwrap();
    let (stream, input, output) = (SCRATCH, SCRATCH + 0x100, SCRATCH + 0x400);
    env.memory.write_bytes(input, &packed).unwrap();
    env.call("zlib125", "inflateInit_", &[stream, 0, 0x38]);
    for (field, value) in
        [(0x00, input), (0x04, packed.len() as u32), (0x0C, output), (0x10, 0x100)]
    {
        env.memory.write_u32(stream + field, value).unwrap();
    }
    assert_eq!(env.call("zlib125", "inflate", &[stream, Z_FINISH]), Z_STREAM_END.cast_unsigned());
    assert_eq!(env.memory.read_u32(stream + 0x14).unwrap(), 25);
    assert_eq!(env.memory.read_c_string(output, 25).unwrap(), "hello hello hello shocora");
    env.memory.write_bytes(SCRATCH + 0x800, b"123456789").unwrap();
    assert_eq!(env.call("zlib125", "crc32", &[0, SCRATCH + 0x800, 9]), 0xCBF4_3926);
    assert_eq!(env.call("zlib125", "crc32", &[0xCBF4_3926, SCRATCH + 0x800, 0]), 0xCBF4_3926);
}

#[test]
fn gzip_trailer_is_consumed_even_across_calls() {
    let mut env = TestEnv::new();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(b"gzip data for shocora").unwrap();
    let packed = encoder.finish().unwrap();
    let (stream, input, output) = (SCRATCH, SCRATCH + 0x100, SCRATCH + 0x400);
    env.memory.write_bytes(input, &packed).unwrap();
    env.call("zlib125", "inflateInit2_", &[stream, 31, 0, 0x38]);
    // Withhold the last three bytes: the end is only reported once they arrive.
    let first = packed.len() as u32 - 3;
    for (field, value) in [(0x00, input), (0x04, first), (0x0C, output), (0x10, 0x100)] {
        env.memory.write_u32(stream + field, value).unwrap();
    }
    assert_eq!(env.call("zlib125", "inflate", &[stream, 0]), Z_OK.cast_unsigned());
    assert_eq!(env.memory.read_u32(stream + 0x04).unwrap(), 0);
    env.memory.write_u32(stream + 0x04, 3).unwrap();
    assert_eq!(env.call("zlib125", "inflate", &[stream, 0]), Z_STREAM_END.cast_unsigned());
    assert_eq!(env.memory.read_u32(stream + 0x04).unwrap(), 0);
    assert_eq!(env.memory.read_c_string(output, 21).unwrap(), "gzip data for shocora");
}
