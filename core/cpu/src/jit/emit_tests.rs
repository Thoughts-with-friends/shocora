//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                         ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ jit/emit_tests.rs        │   │ pass / fail        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tests of emit.rs         │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for emit.rs (PowerPC → Cranelift IR). They run with `cargo test` only and are not
//! part of the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use crate::{
    Bus, CpuState, Fpr, INSTR_BLR, StepEvent, encode_hle_call,
    interpreter::{asm::*, step, test_bus::TestBus},
    jit::Jit,
};

/// Bus with a flat, directly addressable memory (little-endian host view of big-endian bytes).
struct FlatBus(TestBus);

impl Bus for FlatBus {
    fn read_u8(&mut self, a: u32) -> Result<u8, crate::BusError> {
        self.0.read_u8(a)
    }
    fn read_u16(&mut self, a: u32) -> Result<u16, crate::BusError> {
        self.0.read_u16(a)
    }
    fn read_u32(&mut self, a: u32) -> Result<u32, crate::BusError> {
        self.0.read_u32(a)
    }
    fn read_u64(&mut self, a: u32) -> Result<u64, crate::BusError> {
        self.0.read_u64(a)
    }
    fn write_u8(&mut self, a: u32, v: u8) -> Result<(), crate::BusError> {
        self.0.write_u8(a, v)
    }
    fn write_u16(&mut self, a: u32, v: u16) -> Result<(), crate::BusError> {
        self.0.write_u16(a, v)
    }
    fn write_u32(&mut self, a: u32, v: u32) -> Result<(), crate::BusError> {
        self.0.write_u32(a, v)
    }
    fn write_u64(&mut self, a: u32, v: u64) -> Result<(), crate::BusError> {
        self.0.write_u64(a, v)
    }
    fn timebase(&mut self) -> u64 {
        self.0.timebase()
    }
    fn fast_memory(&mut self) -> Option<*mut u8> {
        Some(self.0.mem.as_mut_ptr())
    }
}

/// A program exercising many translated forms: ends in `hle_call 1`.
fn program() -> Vec<u32> {
    let rlwinm = |rs: u32, ra: u32, sh: u32, mb: u32, me: u32| {
        (21 << 26) | (rs << 21) | (ra << 16) | (sh << 11) | (mb << 6) | (me << 1)
    };
    vec![
        li(3, 0x1234),
        d(15, 4, 3, 2),            // addis r4, r3, 2
        x(31, 3, 5, 4, 266, true), // add. r5, r3, r4
        d(36, 5, 0, 0x2000),       // stw r5, 0x2000(0)
        d(32, 6, 0, 0x2000),       // lwz r6, 0x2000(0)
        d(40, 7, 0, 0x2002),       // lhz r7, 0x2002(0)
        d(34, 8, 0, 0x2003),       // lbz r8, 0x2003(0)
        rlwinm(6, 9, 8, 16, 31),   // rlwinm r9, r6, 8, 16, 31
        li(10, -5),
        d(12, 11, 10, 7),             // addic r11, r10, 7 (carries)
        x(31, 3, 12, 10, 138, false), // adde r12, r3, r10
        d(11, 0, 10, 0),              // cmpwi r10, 0
        x(31, 10, 13, 2, 824, true),  // srawi. r13, r10, 2
        d(38, 8, 0, 0x2010),          // stb r8, 0x2010(0)
        li(14, 3),
        (31 << 26) | (14 << 21) | (9 << 16) | (467 << 1), // mtctr r14
        d(14, 15, 15, 1),      // loop: addi r15, r15, 1 (r15 starts 0, rA=15)
        0x4200_FFFC,           // bdnz loop
        d(48, 1, 0, 0x2020),   // lfs f1, 0x2020(0)
        a(63, 2, 1, 1, 0, 21), // fadd f2, f1, f1
        a(59, 3, 2, 0, 1, 25), // fmuls f3, f2, f1
        d(52, 3, 0, 0x2030),   // stfs f3, 0x2030(0)
        (4 << 26) | (4 << 21) | (1 << 16) | (2 << 11) | (21 << 1), // ps_add f4, f1, f2
        (63 << 26) | (5 << 21) | (2 << 11) | (40 << 1), // fneg f5, f2
        ((63 << 26) | (6 << 16) | (2 << 11)), // fcmpu cr0, f6, f2
        a(4, 7, 5, 2, 4, 23),  // ps_sel f7, f5, f4, f2
        li(16, 0x2035),
        x(31, 0, 0, 16, 1014, false), // dcbz 0, r16 (line 0x2020..0x2040)
        d(32, 17, 0, 0x2030),         // lwz r17, 0x2030(0)
        encode_hle_call(1),
        INSTR_BLR,
    ]
}

/// Float and integer quantization, conversions, and branches taken both ways
/// inside one block. GQR1 quantizes to `s16`, which leaves the block.
fn second_program() -> Vec<u32> {
    let psq = |op: u32, rt: u32, ra: u32, w: u32, gqr: u32, offset: u32| {
        (op << 26) | (rt << 21) | (ra << 16) | (w << 15) | (gqr << 12) | offset
    };
    vec![
        psq(56, 1, 0, 0, 0, 0x400), // psq_l f1, 0x400(0), 0, qr0
        psq(56, 2, 0, 1, 0, 0x404), // psq_l f2, 0x404(0), 1, qr0
        li(5, 0x400),
        psq(57, 3, 5, 0, 0, 8),     // psq_lu f3, 8(r5), 0, qr0
        psq(60, 1, 0, 0, 0, 0x410), // psq_st f1, 0x410(0), 0, qr0
        psq(60, 2, 0, 1, 0, 0x418), // psq_st f2, 0x418(0), 1, qr0
        psq(56, 4, 0, 0, 1, 0x420), // psq_l f4, 0x420(0), 0, qr1
        psq(60, 4, 0, 0, 1, 0x428), // psq_st f4, 0x428(0), 0, qr1
        (63 << 26) | (6 << 21) | (3 << 11) | (15 << 1), // fctiwz f6, f3
        li(7, 0x430),
        x(31, 6, 0, 7, 983, false), // stfiwx f6, 0, r7
        a(63, 8, 1, 2, 3, 23),      // fsel f8, f1, f3, f2
        li(9, 5),
        d(11, 0, 9, 5),                                   // cmpwi r9, 5
        0x4182_0008,                                      // beq +8 (taken)
        li(10, 111),                                      // skipped
        d(11, 0, 9, 6),                                   // cmpwi r9, 6
        0x4182_0008,                                      // beq +8 (not taken)
        li(11, 222),                                      // runs
        0x4800_0005,                                      // bl +4
        (31 << 26) | (12 << 21) | (8 << 16) | (339 << 1), // mflr r12
        encode_hle_call(2),
        INSTR_BLR,
    ]
}

/// Runs `program` both ways and compares everything observable.
fn assert_same_as_interpreter(program: &[u32], set: impl Fn(&mut TestBus, &mut CpuState)) {
    let prepare = || {
        let mut bus = TestBus::new();
        bus.load_program(0x100, program);
        let mut state = CpuState { pc: 0x100, gqr: [0; 8], ..CpuState::default() };
        set(&mut bus, &mut state);
        (bus, state)
    };
    let (mut reference_bus, mut reference) = prepare();
    let event = loop {
        let event = step(&mut reference, &mut reference_bus).unwrap();
        if event != StepEvent::Continue {
            break event;
        }
    };
    for (synchronous, regions) in [(true, false), (true, true), (false, false)] {
        let (bus, mut state) = prepare();
        let mut bus = FlatBus(bus);
        let mut jit = Jit::new();
        jit.set_synchronous(synchronous);
        if regions {
            jit.set_regions_first();
        }
        let (jit_event, executed) = jit.run(&mut state, &mut bus, 1000).unwrap();
        assert_eq!(jit_event, event);
        assert!(executed > 0);
        assert!(!synchronous || jit.compiled_blocks() > 0);
        assert_eq!(state.gpr, reference.gpr);
        for (index, (got, want)) in state.fpr.iter().zip(&reference.fpr).enumerate() {
            assert!(
                got.ps0.to_bits() == want.ps0.to_bits() && got.ps1.to_bits() == want.ps1.to_bits(),
                "f{index}: {got:?} vs {want:?}"
            );
        }
        assert_eq!(
            (state.pc, state.lr, state.ctr, state.cr),
            (reference.pc, reference.lr, reference.ctr, reference.cr)
        );
        assert_eq!(bus.0.mem[0x400..0x440], reference_bus.mem[0x400..0x440]);
    }
}

#[test]
fn quantized_accesses_and_inner_branches_match_the_interpreter() {
    assert_same_as_interpreter(&second_program(), |bus, state| {
        state.gqr[1] = 0x0007_0007;
        bus.load_program(
            0x400,
            &[1.5_f32.to_bits(), (-2.25_f32).to_bits(), 70_000.75_f32.to_bits(), 2.5_f32.to_bits()],
        );
        bus.load_program(0x420, &[0x1234_FFFE]);
    });
}

/// A counted loop calling a function: the call returns into the middle of the region.
#[test]
fn calls_return_into_a_looping_region() {
    let program = [
        li(3, 0),
        li(4, 5),
        (31 << 26) | (4 << 21) | (9 << 16) | (467 << 1), // mtctr r4
        0x4800_0015,                                     // loop: bl func
        d(14, 3, 3, 1),                                  // addi r3, r3, 1
        0x4200_FFF8,                                     // bdnz loop
        encode_hle_call(3),
        INSTR_BLR,
        d(14, 5, 5, 2), // func: addi r5, r5, 2
        INSTR_BLR,
    ];
    assert_same_as_interpreter(&program, |_, _| {});
}

#[test]
fn the_first_program_matches_the_interpreter_as_a_region() {
    assert_same_as_interpreter(&program(), |bus, _| {
        bus.load_program(0x2020, &[1.5_f32.to_bits()]);
    });
}

#[test]
fn a_used_up_budget_stops_a_chain_of_blocks() {
    // b +4 forever.
    let (mut bus, mut state) = (TestBus::new(), CpuState { pc: 0x100, ..CpuState::default() });
    bus.load_program(0x100, &[0x4800_0000]);
    let mut bus = FlatBus(bus);
    let mut jit = Jit::new();
    jit.set_synchronous(true);
    let (event, executed) = jit.run(&mut state, &mut bus, 100).unwrap();
    assert_eq!(event, StepEvent::Continue);
    assert_eq!(executed, 100);
    assert_eq!(jit.compiled_blocks(), 1);
}

fn setup() -> (TestBus, CpuState) {
    let mut bus = TestBus::new();
    bus.load_program(0x100, &program());
    bus.load_program(0x2020, &[1.5_f32.to_bits()]);
    (bus, CpuState { pc: 0x100, gqr: [0; 8], ..CpuState::default() })
}

/// The JIT must leave exactly the state the reference interpreter leaves.
#[test]
fn matches_the_interpreter() {
    let (mut reference_bus, mut reference) = setup();
    let event = loop {
        let event = step(&mut reference, &mut reference_bus).unwrap();
        if event != StepEvent::Continue {
            break event;
        }
    };
    assert_eq!(event, StepEvent::HleCall(1));

    let (bus, mut state) = setup();
    let mut bus = FlatBus(bus);
    let mut jit = Jit::new();
    jit.set_synchronous(true);
    let (jit_event, _) = jit.run(&mut state, &mut bus, 1000).unwrap();
    assert_eq!(jit_event, event);
    assert!(jit.compiled_blocks() > 0, "the blocks must really be translated");
    assert_eq!(state.gpr, reference.gpr);
    for (index, (got, want)) in state.fpr.iter().zip(&reference.fpr).enumerate() {
        let same = |a: f64, b: f64| a.to_bits() == b.to_bits();
        assert!(
            same(got.ps0, want.ps0) && same(got.ps1, want.ps1),
            "f{index}: {got:?} vs {want:?}"
        );
    }
    assert_eq!(
        (state.pc, state.lr, state.ctr, state.cr),
        (reference.pc, reference.lr, reference.ctr, reference.cr)
    );
    assert_eq!(
        (state.xer_ca, state.xer_so, state.fpscr),
        (reference.xer_ca, reference.xer_so, reference.fpscr)
    );
    assert_eq!(bus.0.mem[0x2000..0x2040], reference_bus.mem[0x2000..0x2040]);
    assert!(state.fpr[3] != Fpr::default());
}
