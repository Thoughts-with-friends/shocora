//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                         ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ decode_tests.rs          │   │ pass / fail        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tests of decode.rs       │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for decode.rs (instruction decoder). They run with `cargo test` only and are not
//! part of the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;

#[test]
fn decodes_common_instructions() {
    let cases = [
        (0x3860_0005, Opcode::Addi),   // li r3, 5
        (0x7C63_2214, Opcode::Add),    // add r3, r3, r4
        (0x7C63_2615, Opcode::Add),    // addo. r3, r3, r4
        (0x7C08_02A6, Opcode::Mfspr),  // mflr r0
        (0x4E80_0020, Opcode::Bclr),   // blr
        (0x4E80_0420, Opcode::Bcctr),  // bctr
        (0x9421_FFF0, Opcode::Stwu),   // stwu r1, -16(r1)
        (0x5463_103A, Opcode::Rlwinm), // slwi r3, r3, 2
        (0x7C00_04AC, Opcode::Sync),
        (0xFC20_0890, Opcode::Fmr),   // fmr f1, f1
        (0xEC21_102A, Opcode::Fadds), // fadds f1, f1, f2
        (0x1021_102A, Opcode::PsAdd), // ps_add f1, f1, f2
        (0x1021_14A0, Opcode::PsMerge10),
        (0xE021_0000, Opcode::PsqL),
        (0x0400_0007, Opcode::HleCall),
        (0x0000_0000, Opcode::Invalid),
    ];
    for (raw, expected) in cases {
        assert_eq!(decode(raw), expected, "{raw:#010x}");
    }
}
