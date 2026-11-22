//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ registry_tests.rs        │   │ pass / fail        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tests of registry.rs     │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for registry.rs (table of OS functions). They run with `cargo test` only and are
//! not part of the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;

#[test]
fn a_state_brings_back_stubs_and_refuses_other_builds() {
    const EXPORT: HleExport = HleExport {
        library: "coreinit",
        name: "Known",
        function: |_| unreachable!("never called"),
    };
    let mut registry = HleRegistry::new();
    registry.register_functions(&[EXPORT]);
    let known = registry.function_stub("coreinit", "Known");
    let missing = registry.function_stub("coreinit", "Nope");
    let state = registry.save_state();

    let mut same = HleRegistry::new();
    same.register_functions(&[EXPORT]);
    same.restore_state(state).unwrap();
    assert_eq!(same.function_stub("coreinit", "Known"), known);
    assert_eq!(same.function_stub("coreinit", "Nope"), missing);

    let other = HleRegistry::new();
    assert!(HleRegistry::new().restore_state(registry.save_state()).is_err());
    assert_eq!(other.entries.len(), 0);
}

#[test]
fn stubs_are_shared_and_committed() {
    let mut registry = HleRegistry::new();
    let a = registry.function_stub("coreinit.rpl", "Nope");
    let b = registry.function_stub("coreinit", "Nope");
    assert_eq!(a, b);
    let mut memory = Memory::new();
    memory.map(HLE).unwrap();
    registry.commit(&mut memory).unwrap();
    assert_eq!(memory.read_u32(a + 4).unwrap(), INSTR_BLR);
    let index = memory.read_u32(a).unwrap() & 0x03FF_FFFF;
    assert!(matches!(registry.entry(index), Some(Entry::Missing { .. })));
    assert!(registry.note_missing_call(index));
    assert!(!registry.note_missing_call(index));
}

#[test]
fn function_pointer_data_exports() {
    let mut registry = HleRegistry::new();
    registry.register_data(&[HleDataExport {
        library: "coreinit",
        name: "Ptr",
        init: DataInit::FunctionPointer { library: "coreinit", name: "Target" },
    }]);
    let slot = registry.data_address("coreinit", "Ptr");
    let stub = registry.function_stub("coreinit", "Target");
    let mut memory = Memory::new();
    memory.map(HLE).unwrap();
    registry.commit(&mut memory).unwrap();
    assert_eq!(memory.read_u32(slot).unwrap(), stub);
    let handle = registry.library_handle("gx2.rpl");
    assert_eq!(registry.library_name(handle), Some("gx2"));
}
