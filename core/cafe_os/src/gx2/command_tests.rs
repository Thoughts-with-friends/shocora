//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ gx2/command_tests.rs     │   │ pass / fail        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tests of command.rs      │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for command.rs (command submission). They run with `cargo test` only and are not
//! part of the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use crate::hle::test_env::{SCRATCH, TestEnv};

#[test]
fn display_lists_record_into_guest_memory() {
    let mut env = TestEnv::new();
    let list = SCRATCH + 0x100;
    env.call("gx2", "GX2BeginDisplayListEx", &[list, 0x100, 0]);
    env.call("gx2", "GX2SetPrimitiveRestartIndex", &[7]);
    let size = env.call("gx2", "GX2EndDisplayList", &[list]);
    assert_eq!(size, 12);
    assert_eq!(env.memory.read_u32(list + 8).unwrap(), 7);
    env.call("gx2", "GX2CallDisplayList", &[list, size]);
    env.call("gx2", "GX2DrawDone", &[]);
    assert_eq!(env.gpu.registers().get(wiiu_gpu::regs::context::VGT_MULTI_PRIM_IB_RESET_INDX), 7);
}

#[test]
fn context_state_list_restores_the_state_it_belongs_to() {
    let mut env = TestEnv::new();
    let state = SCRATCH + 0x1000;
    let (list_out, size_out) = (SCRATCH, SCRATCH + 4);
    env.call("gx2", "GX2SetupContextStateEx", &[state, 0]);
    env.call("gx2", "GX2GetContextStateDisplayList", &[state, list_out, size_out]);
    let list = env.memory.read_u32(list_out).unwrap();
    assert_eq!(list, state + super::CONTEXT_LOAD_LIST);
    assert_eq!(env.memory.read_u32(size_out).unwrap(), 12);
    assert_eq!(env.memory.read_u32(list + 8).unwrap(), state);
}

#[test]
fn current_display_list_is_known_only_while_recording() {
    let mut env = TestEnv::new();
    let (address, size) = (SCRATCH, SCRATCH + 4);
    assert_eq!(env.call("gx2", "GX2GetCurrentDisplayList", &[address, size]), 0);
    env.call("gx2", "GX2BeginDisplayListEx", &[SCRATCH + 0x100, 0x80, 0]);
    assert_eq!(env.call("gx2", "GX2GetCurrentDisplayList", &[address, size]), 1);
    assert_eq!(env.memory.read_u32(address).unwrap(), SCRATCH + 0x100);
    assert_eq!(env.memory.read_u32(size).unwrap(), 0x80);
}

#[test]
fn swap_status_counts_requests_and_vsync_retires_them() {
    let mut env = TestEnv::new();
    env.call("gx2", "GX2SwapScanBuffers", &[]);
    let (requests, executed) = (SCRATCH, SCRATCH + 4);
    env.call("gx2", "GX2GetSwapStatus", &[requests, executed, SCRATCH + 8, SCRATCH + 16]);
    assert_eq!(env.memory.read_u32(requests).unwrap(), 1);
    assert_eq!(env.memory.read_u32(executed).unwrap(), 0);
    env.os.vsync();
    env.call("gx2", "GX2GetSwapStatus", &[requests, executed, SCRATCH + 8, SCRATCH + 16]);
    assert_eq!(env.memory.read_u32(executed).unwrap(), 1);
}

#[test]
fn the_least_interval_slows_fast_titles_only() {
    let mut env = TestEnv::new();
    env.os.gx2.min_swap_interval = 2;
    env.call("gx2", "GX2SetSwapInterval", &[1]);
    assert_eq!(env.call("gx2", "GX2GetSwapInterval", &[]), 2, "60 fps becomes 30");
    env.call("gx2", "GX2SetSwapInterval", &[3]);
    assert_eq!(env.call("gx2", "GX2GetSwapInterval", &[]), 3, "20 fps stays");
    env.os.gx2.forced_swap_interval = Some(1);
    assert_eq!(env.call("gx2", "GX2GetSwapInterval", &[]), 1, "forcing wins");
}

#[test]
fn one_flip_per_swap_interval() {
    let mut env = TestEnv::new();
    env.call("gx2", "GX2SetSwapInterval", &[2]);
    for _ in 0..3 {
        env.call("gx2", "GX2SwapScanBuffers", &[]);
    }
    let executed: Vec<u32> = (0..6)
        .map(|_| {
            env.os.vsync();
            env.os.gx2.flips_executed
        })
        .collect();
    assert_eq!(executed, [0, 1, 1, 2, 2, 3]);
}
