//! Branches. `s.pc` already points past the branch when these run.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                         ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ branch             │   │ interpreter/branch.rs    │   │ next instruction   │
//! │ instructions       │──►│ ──────────────────────── │──►│ address            │
//! │                    │   │ jumps and calls          │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Instructions that jump elsewhere: calls, returns, loops, ifs. Terms such as HLE, JIT or
//! shader are explained in `docs/guide/wiiu_term.md`.
#![allow(clippy::cast_sign_loss)]

use super::{ExecResult, unimplemented};
use crate::{CpuState, Instr, Opcode, StepEvent};

/// Evaluates BO/BI. Decrements CTR first unless BO[2] is set or `use_ctr` is false.
fn condition(s: &mut CpuState, bo: usize, bi: usize, use_ctr: bool) -> bool {
    let ctr_ok = if !use_ctr || bo & 0b00100 != 0 {
        true
    } else {
        s.ctr = s.ctr.wrapping_sub(1);
        (s.ctr != 0) != (bo & 0b00010 != 0)
    };
    let cond_ok = bo & 0b10000 != 0 || s.cr_bit(bi) == (bo & 0b01000 != 0);
    ctr_ok && cond_ok
}

pub(super) fn execute(op: Opcode, i: Instr, s: &mut CpuState, pc: u32) -> ExecResult {
    let next = pc.wrapping_add(4);
    let (taken, target) = match op {
        Opcode::B => {
            let target = if i.aa() { i.li() as u32 } else { pc.wrapping_add(i.li() as u32) };
            (true, target)
        }
        Opcode::Bc => {
            let target = if i.aa() { i.bd() as u32 } else { pc.wrapping_add(i.bd() as u32) };
            (condition(s, i.rd(), i.ra(), true), target)
        }
        Opcode::Bclr => {
            let target = s.lr & !3;
            (condition(s, i.rd(), i.ra(), true), target)
        }
        Opcode::Bcctr => (condition(s, i.rd(), i.ra(), false), s.ctr & !3),
        _ => return unimplemented(op),
    };
    if i.lk() {
        s.lr = next;
    }
    if taken {
        s.pc = target;
    }
    Ok(StepEvent::Continue)
}

#[cfg(test)]
mod tests {
    use crate::{
        CpuState, INSTR_BLR,
        interpreter::{asm::*, step, test_bus::TestBus},
    };

    #[test]
    fn call_and_return() {
        let mut bus = TestBus::new();
        // 0x100: bl +0x10 ; 0x110: blr
        bus.load_program(0x100, &[0x4800_0011]);
        bus.load_program(0x110, &[INSTR_BLR]);
        let mut s = CpuState { pc: 0x100, ..CpuState::default() };
        step(&mut s, &mut bus).unwrap();
        assert_eq!((s.pc, s.lr), (0x110, 0x104));
        step(&mut s, &mut bus).unwrap();
        assert_eq!(s.pc, 0x104);
    }

    #[test]
    fn bdnz_loop() {
        let mut bus = TestBus::new();
        // li r3,0 ; li r4,5 ; mtctr r4 ; addi r3,r3,1 ; bdnz -4
        let mtctr_r4 = x(31, 4, 9, 0, 467, false);
        bus.load_program(0x100, &[li(3, 0), li(4, 5), mtctr_r4, d(14, 3, 3, 1), 0x4200_FFFC]);
        let mut s = CpuState { pc: 0x100, ..CpuState::default() };
        while s.pc != 0x114 {
            step(&mut s, &mut bus).unwrap();
        }
        assert_eq!((s.gpr[3], s.ctr), (5, 0));
    }

    #[test]
    fn conditional_on_cr() {
        let mut bus = TestBus::new();
        // beq +8
        bus.load_program(0x100, &[0x4182_0008]);
        let mut s = CpuState { pc: 0x100, ..CpuState::default() };
        s.set_cr_field(0, 0b0010);
        step(&mut s, &mut bus).unwrap();
        assert_eq!(s.pc, 0x108);
        s.pc = 0x100;
        s.set_cr_field(0, 0b0100);
        step(&mut s, &mut bus).unwrap();
        assert_eq!(s.pc, 0x104);
    }
}
