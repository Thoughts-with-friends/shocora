//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ gx2/resource_tests.rs    │   │ pass / fail        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tests of resource.rs     │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for resource.rs (GX2R resources). They run with `cargo test` only and are not
//! part of the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::flag;
use crate::hle::test_env::{SCRATCH, TestEnv};

/// Intent: `GX2RCreateBuffer` gets its memory from the title's allocator, called with
/// (flags, size rounded to 64 bytes, alignment), and stores the result in the buffer
/// before returning success to the caller (Xenoblade Chronicles X crashed on a null
/// buffer while the export was missing).
/// Pass: the call jumps to the allocator with those arguments, and the return stub
/// writes the pointer, returns 1 and goes back to the original `lr`.
#[test]
fn create_buffer_calls_the_title_allocator() {
    let mut env = TestEnv::new();
    let (alloc, free) = (0x0200_0000, 0x0200_0100);
    env.call("gx2", "GX2RSetAllocator", &[alloc, free]);
    let buffer = SCRATCH;
    let flags = flag::VERTEX_BUFFER | flag::CPU_WRITE;
    for (offset, value) in [(0, flags), (4, 12), (8, 10), (12, 0)] {
        env.memory.write_u32(buffer + offset, value).unwrap();
    }
    env.cpu.lr = 0x0300_0000;
    env.call("gx2", "GX2RCreateBuffer", &[buffer]);
    assert_eq!(env.cpu.pc, alloc);
    assert_eq!(env.cpu.gpr[3..6], [flags, 0x80, 0x40]);

    let ok = env.call("shocora", "__shocora_GX2RAllocReturn", &[SCRATCH + 0x1000]);
    assert_eq!(ok, 1);
    assert_eq!(env.cpu.lr, 0x0300_0000);
    assert_eq!(env.memory.read_u32(buffer + 12).unwrap(), SCRATCH + 0x1000);
    assert_ne!(env.memory.read_u32(buffer).unwrap() & flag::ALLOCATED_BY_GX2R, 0);
    assert_eq!(env.call("gx2", "GX2RLockBufferEx", &[buffer, 0]), SCRATCH + 0x1000);

    env.call("gx2", "GX2RDestroyBufferEx", &[buffer, 0]);
    assert_eq!(env.cpu.pc, free);
    assert_eq!(env.cpu.gpr[4], SCRATCH + 0x1000);
    assert_eq!(env.call("gx2", "GX2RBufferExists", &[buffer]), 0);
}

/// Intent: without `GX2RSetAllocator`, GX2R allocates from the default heap through
/// `MEMAllocFromDefaultHeapEx`, as GX2 sets up in `GX2Init` (Xenoblade Chronicles X
/// creates buffers without setting an allocator).
/// Pass: creation jumps to the default GX2R stub, which moves (size, align) into r3/r4
/// and jumps to the default heap's allocation function.
#[test]
fn create_buffer_defaults_to_the_default_heap() {
    let mut env = TestEnv::new();
    let buffer = SCRATCH;
    let flags = flag::UNIFORM_BLOCK;
    for (offset, value) in [(0, flags), (4, 4), (8, 16), (12, 0)] {
        env.memory.write_u32(buffer + offset, value).unwrap();
    }
    env.call("gx2", "GX2RCreateBuffer", &[buffer]);
    let default_alloc = env.os.registry.function_stub("shocora", "__shocora_GX2RDefaultAlloc");
    assert_eq!(env.cpu.pc, default_alloc);
    env.call("shocora", "__shocora_GX2RDefaultAlloc", &[flags, 0x40, 0x100]);
    assert_eq!(env.cpu.gpr[3..5], [0x40, 0x100]);
    let heap_alloc = env.os.registry.function_stub("coreinit", "__shocora_DefaultHeapAllocEx");
    assert_eq!(env.cpu.pc, heap_alloc);
}
