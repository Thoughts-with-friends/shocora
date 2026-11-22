//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ gx2/misc_tests.rs        │   │ pass / fail        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tests of misc.rs         │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for misc.rs (other GX2 functions). They run with `cargo test` only and are not
//! part of the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use crate::hle::test_env::{SCRATCH, TestEnv};

/// Intent: a CPU occlusion query is "not ready" until the GPU ends it, then reports
/// the samples written between begin and end.
/// Pass: before the end the result call returns 0; after the executor's end packet is
/// emulated by writing the counters, it returns 1 with end - start.
#[test]
fn cpu_occlusion_query_is_pending_until_ended() {
    let mut env = TestEnv::new();
    let query = SCRATCH;
    let out = SCRATCH + 0x100;
    env.call("gx2", "GX2QueryBegin", &[0, query]);
    assert_eq!(env.call("gx2", "GX2QueryGetOcclusionResult", &[query, out]), 0);
    env.memory.write_bytes(query + 8, &50_u64.to_le_bytes()).unwrap();
    env.memory.write_u32(query + 0x20, 1).unwrap();
    assert_eq!(env.call("gx2", "GX2QueryGetOcclusionResult", &[query, out]), 1);
    assert_eq!(env.memory.read_u64(out).unwrap(), 50);
}

/// Intent: point registers hold sizes in 1/8 pixel units, clamped to 16 bits.
/// Pass: width 2 and height 1 give 0x0010_0008.
#[test]
fn point_size_is_in_eighths() {
    let mut env = TestEnv::new();
    env.cpu.fpr[1].ps0 = 2.0;
    env.cpu.fpr[2].ps0 = 1.0;
    env.call("gx2", "GX2InitPointSizeReg", &[SCRATCH]);
    assert_eq!(env.memory.read_u32(SCRATCH).unwrap(), 0x0010_0008);
}
