//! CR logic, SPR/MSR/SR access, traps, syscalls, cache/sync hints and HLE calls.
#![allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]

use super::{ExecResult, unimplemented};
use crate::{Bus, CpuState, Instr, Opcode, StepEvent, state::spr};

/// TO field of `tw`/`twi`.
const fn trap_fires(to: usize, a: u32, b: u32) -> bool {
    let (sa, sb) = (a as i32, b as i32);
    (to & 0b10000 != 0 && sa < sb)
        || (to & 0b01000 != 0 && sa > sb)
        || (to & 0b00100 != 0 && a == b)
        || (to & 0b00010 != 0 && a < b)
        || (to & 0b00001 != 0 && a > b)
}

fn read_spr<B: Bus>(s: &CpuState, bus: &mut B, number: u32) -> u32 {
    match number {
        spr::XER => s.xer(),
        spr::LR => s.lr,
        spr::CTR => s.ctr,
        spr::GQR0..=spr::GQR7 => s.gqr[(number - spr::GQR0) as usize],
        spr::UGQR0..=spr::UGQR7 => s.gqr[(number - spr::UGQR0) as usize],
        spr::TBL_READ => bus.timebase() as u32,
        spr::TBU_READ => (bus.timebase() >> 32) as u32,
        _ => s.spr[number as usize],
    }
}

fn write_spr(s: &mut CpuState, number: u32, value: u32) {
    match number {
        spr::XER => s.set_xer(value),
        spr::LR => s.lr = value,
        spr::CTR => s.ctr = value,
        spr::GQR0..=spr::GQR7 => s.gqr[(number - spr::GQR0) as usize] = value,
        // Xenoblade X sets UGQR7 to s16 before converting its mixed sound with
        // `psq_st`; ignored, the PCM16 ring buffers got float32 and played as noise.
        spr::UGQR0..=spr::UGQR7 => s.gqr[(number - spr::UGQR0) as usize] = value,
        // Read-only identification registers.
        spr::PVR | spr::UPIR => {}
        _ => s.spr[number as usize] = value,
    }
}

pub(super) fn execute<B: Bus>(op: Opcode, i: Instr, s: &mut CpuState, bus: &mut B) -> ExecResult {
    use Opcode as O;
    let (rd, ra, rb) = (i.rd(), i.ra(), i.rb());
    match op {
        O::HleCall => return Ok(StepEvent::HleCall(i.0 & 0x03FF_FFFF)),
        O::Sc => return Ok(StepEvent::SystemCall),
        O::Tw if trap_fires(rd, s.gpr[ra], s.gpr[rb]) => return Ok(StepEvent::Trap),
        O::Twi if trap_fires(rd, s.gpr[ra], i.simm() as u32) => return Ok(StepEvent::Trap),
        O::Tw | O::Twi => {}

        O::Mcrf => s.set_cr_field(i.crfd(), s.cr_field(i.crfs())),
        O::Mcrxr => {
            s.set_cr_field(i.crfd(), s.xer() >> 28);
            s.xer_so = false;
            s.xer_ov = false;
            s.xer_ca = false;
        }
        O::Mfcr => s.gpr[rd] = s.cr,
        O::Mtcrf => {
            let mask = (0..8)
                .filter(|field| i.crm() & (0x80 >> field) != 0)
                .fold(0_u32, |mask, field| mask | (0xF000_0000 >> (4 * field)));
            s.cr = (s.cr & !mask) | (s.gpr[rd] & mask);
        }
        O::Crand | O::Crandc | O::Creqv | O::Crnand | O::Crnor | O::Cror | O::Crorc | O::Crxor => {
            let (a, b) = (s.cr_bit(ra), s.cr_bit(rb));
            let value = match op {
                O::Crand => a & b,
                O::Crandc => a & !b,
                O::Creqv => a == b,
                O::Crnand => !(a & b),
                O::Crnor => !(a | b),
                O::Cror => a | b,
                O::Crorc => a | !b,
                _ => a ^ b,
            };
            s.set_cr_bit(rd, value);
        }

        O::Mfspr | O::Mftb => s.gpr[rd] = read_spr(s, bus, i.spr()),
        O::Mtspr => write_spr(s, i.spr(), s.gpr[rd]),
        O::Mfmsr => s.gpr[rd] = s.msr,
        O::Mtmsr => s.msr = s.gpr[rd],
        O::Mfsr => s.gpr[rd] = s.sr[i.sr()],
        O::Mtsr => s.sr[i.sr()] = s.gpr[rd],
        O::Mfsrin => s.gpr[rd] = s.sr[(s.gpr[rb] >> 28) as usize],
        O::Mtsrin => s.sr[(s.gpr[rb] >> 28) as usize] = s.gpr[rd],

        // No caches or MMU to maintain.
        O::Sync
        | O::Isync
        | O::Eieio
        | O::Dcbf
        | O::Dcbst
        | O::Dcbt
        | O::Dcbtst
        | O::Dcbi
        | O::Icbi
        | O::Tlbie
        | O::Tlbsync => {}

        _ => return unimplemented(op),
    }
    Ok(StepEvent::Continue)
}

#[cfg(test)]
mod tests {
    use crate::{
        CpuState, StepEvent,
        interpreter::{asm::*, step, test_bus::TestBus},
    };

    fn run_one(s: &mut CpuState, bus: &mut TestBus, raw: u32) -> StepEvent {
        bus.load_program(0x100, &[raw]);
        s.pc = 0x100;
        step(s, bus).unwrap()
    }

    #[test]
    fn spr_moves() {
        let mut bus = TestBus::new();
        let mut s = CpuState::new(2);
        s.gpr[3] = 0x1234;
        run_one(&mut s, &mut bus, x(31, 3, 8, 0, 467, false)); // mtlr r3
        assert_eq!(s.lr, 0x1234);
        run_one(&mut s, &mut bus, x(31, 4, 15, 31, 339, false)); // mfspr r4, UPIR (1007)
        assert_eq!(s.gpr[4], 2);
        bus.timebase = 0x1_0000_0005;
        run_one(&mut s, &mut bus, x(31, 5, 13, 8, 371, false)); // mftbu r5
        assert_eq!(s.gpr[5], 1);
    }

    /// Intent: the user-mode GQR aliases (SPR 896..=903) are the GQRs.
    /// Pass: `mtspr 903, r30` (Xenoblade X's sound mixer) sets GQR7, and `mfspr` from
    /// UGQR7 reads it back.
    #[test]
    fn user_gqrs_are_the_gqrs() {
        let mut bus = TestBus::new();
        let mut s = CpuState::new(0);
        s.gpr[30] = 0x0007_0007;
        run_one(&mut s, &mut bus, 0x7fc7_e3a6); // mtspr 903, r30
        assert_eq!(s.gqr[7], 0x0007_0007);
        run_one(&mut s, &mut bus, 0x7fa7_e2a6); // mfspr r29, 903
        assert_eq!(s.gpr[29], 0x0007_0007);
    }

    #[test]
    fn cr_logic_and_moves() {
        let mut bus = TestBus::new();
        let mut s = CpuState::default();
        s.set_cr_bit(2, true);
        run_one(&mut s, &mut bus, x(19, 6, 2, 2, 193, false)); // crxor 6, 2, 2  (crclr)
        assert!(!s.cr_bit(6));
        run_one(&mut s, &mut bus, x(19, 6, 2, 3, 449, false)); // cror 6, 2, 3
        assert!(s.cr_bit(6));
        s.gpr[3] = 0xABCD_EF01;
        run_one(&mut s, &mut bus, (31 << 26) | (3 << 21) | (0x01 << 12) | (144 << 1)); // mtcrf 0x01, r3
        assert_eq!(s.cr_field(7), 0x1);
    }

    #[test]
    fn traps() {
        let mut bus = TestBus::new();
        let mut s = CpuState::default();
        assert_eq!(run_one(&mut s, &mut bus, x(31, 31, 0, 0, 4, false)), StepEvent::Trap); // trap
        s.gpr[3] = 1;
        assert_eq!(run_one(&mut s, &mut bus, d(3, 4, 3, 0)), StepEvent::Continue); // tweqi r3, 0
    }
}
