//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                                        ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ worker_tests.rs          │   │ pass / fail        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tests of worker.rs       │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for worker.rs (GPU thread). They run with `cargo test` only and are not part of
//! the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;
use crate::command::{opcode, pm4::type3_header};

fn memory() -> Memory {
    let mut memory = Memory::new();
    memory.map(wiiu_mem_addr::Region::new("MEM2", wiiu_mem_addr::MEM2.start, 0x10_0000)).unwrap();
    memory
}

#[test]
fn the_thread_executes_in_order_and_retires_timestamps() {
    let mut memory = memory();
    let mut gpu = Gpu::threaded();
    let words = [type3_header(opcode::SET_CONFIG_REG, 2), 0x10, 0xABCD];
    gpu.submit(&mut memory, &words).unwrap();
    gpu.submit(&mut memory, &words).unwrap();
    assert_eq!(gpu.submitted_timestamp(), 2);
    gpu.wait_timestamp(2);
    assert_eq!(gpu.retired_timestamp(), 2);
    assert_eq!(gpu.registers().get(crate::regs::base::CONFIG + 0x10), 0xABCD);
}

#[test]
fn the_thread_writes_guest_memory_the_cpu_sees() {
    let mut memory = memory();
    let mut gpu = Gpu::threaded();
    let address = wiiu_mem_addr::MEM2.start + 0x100;
    let physical = wiiu_mem_addr::effective_to_physical(address).unwrap();
    // MEM_WRITE: address, then 32-bit data.
    let words = [type3_header(opcode::MEM_WRITE, 4), physical, 0, 0x1234_5678, 0];
    gpu.submit(&mut memory, &words).unwrap();
    gpu.wait_idle();
    assert_eq!(memory.read_u32(address).unwrap(), 0x1234_5678);
}
