//! Scalar floating point and FPSCR access.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use super::{ExecResult, unimplemented};
use crate::{CpuState, Fpr, Instr, Opcode, StepEvent};

/// Rounds to single precision, keeping the `f64` container.
pub(super) fn round_single(value: f64) -> f64 {
    f64::from(value as f32)
}

/// Bit pattern hardware leaves in the upper word of `fctiw` results.
const FCTIW_UPPER: u64 = 0xFFF8_0000_0000_0000;

/// Rounding modes of FPSCR[RN].
fn to_int32(value: f64, rounding: u32) -> u32 {
    if value.is_nan() {
        return 0x8000_0000;
    }
    let rounded = match rounding {
        0 => value.round_ties_even(),
        1 => value.trunc(),
        2 => value.ceil(),
        _ => value.floor(),
    };
    if rounded >= 2_147_483_647.0 {
        0x7FFF_FFFF
    } else if rounded <= -2_147_483_648.0 {
        0x8000_0000
    } else {
        rounded as i32 as u32
    }
}

/// Compares two values, writing CR field `field` and FPSCR[FPCC].
pub(super) fn compare(s: &mut CpuState, field: usize, a: f64, b: f64) {
    let result = if a.is_nan() || b.is_nan() {
        0b0001
    } else if a < b {
        0b1000
    } else if a > b {
        0b0100
    } else {
        0b0010
    };
    s.set_cr_field(field, result);
    s.fpscr = (s.fpscr & !0xF000) | (result << 12);
}

/// Sign-bit manipulation on raw bits (does not canonicalise NaNs).
pub(super) fn with_sign(value: f64, op: Opcode) -> f64 {
    const SIGN: u64 = 1 << 63;
    let bits = value.to_bits();
    f64::from_bits(match op {
        Opcode::Fneg | Opcode::PsNeg => bits ^ SIGN,
        Opcode::Fabs | Opcode::PsAbs => bits & !SIGN,
        Opcode::Fnabs | Opcode::PsNabs => bits | SIGN,
        _ => bits,
    })
}

/// `(offset, step)` per index of Espresso's `frsqrte` estimate (taken from Cemu).
const FRSQRTE_TABLE: [(u32, u32); 32] = [
    (0x1a7_e800, 0x568),
    (0x17c_b800, 0x4f3),
    (0x155_2800, 0x48d),
    (0x130_c000, 0x435),
    (0x10f_2000, 0x3e7),
    (0xef_f000, 0x3a2),
    (0xd2_e000, 0x365),
    (0xb7_c000, 0x32e),
    (0x9e_5000, 0x2fc),
    (0x86_7000, 0x2d0),
    (0x6f_f000, 0x2a8),
    (0x5a_b800, 0x283),
    (0x46_a000, 0x261),
    (0x33_9800, 0x243),
    (0x21_8800, 0x226),
    (0x10_5800, 0x20b),
    (0x3ff_a000, 0x7a4),
    (0x3c2_9000, 0x700),
    (0x38a_a000, 0x670),
    (0x357_2000, 0x5f2),
    (0x327_9000, 0x584),
    (0x2fb_7000, 0x524),
    (0x2d2_6000, 0x4cc),
    (0x2ac_0000, 0x47e),
    (0x288_1000, 0x43a),
    (0x266_5000, 0x3fa),
    (0x246_8000, 0x3c2),
    (0x228_7000, 0x38e),
    (0x20c_1000, 0x35e),
    (0x1f1_2000, 0x332),
    (0x1d7_9000, 0x30a),
    (0x1bf_4000, 0x2e6),
];

/// `(offset, step)` per index of Espresso's `fres` estimate (taken from Cemu).
const FRES_TABLE: [(u32, u32); 32] = [
    (0x7f_f800, 0x3e1),
    (0x78_3800, 0x3a7),
    (0x70_ea00, 0x371),
    (0x6a_0800, 0x340),
    (0x63_8800, 0x313),
    (0x5d_6200, 0x2ea),
    (0x57_9000, 0x2c4),
    (0x52_0800, 0x2a0),
    (0x4c_c800, 0x27f),
    (0x47_ca00, 0x261),
    (0x43_0800, 0x245),
    (0x3e_8000, 0x22a),
    (0x3a_2c00, 0x212),
    (0x36_0800, 0x1fb),
    (0x32_1400, 0x1e5),
    (0x2e_4a00, 0x1d1),
    (0x2a_a800, 0x1be),
    (0x27_2c00, 0x1ac),
    (0x23_d600, 0x19b),
    (0x20_9e00, 0x18b),
    (0x1d_8800, 0x17c),
    (0x1a_9000, 0x16e),
    (0x17_ae00, 0x15b),
    (0x14_f800, 0x15b),
    (0x12_4400, 0x143),
    (0xf_be00, 0x143),
    (0xd_3800, 0x12d),
    (0xa_de00, 0x12d),
    (0x8_8400, 0x11a),
    (0x6_5000, 0x11a),
    (0x4_1c00, 0x108),
    (0x2_0c00, 0x106),
];

const MANTISSA: u64 = (1 << 52) - 1;
const EXPONENT: u64 = 0x7FF << 52;

/// `frsqrte` as Espresso computes it: a table estimate, not the exact `1/sqrt(x)`.
/// Titles' physics (BotW's cloth) is tuned to these values, as Cemu found.
pub(crate) fn frsqrte_espresso(input: f64) -> f64 {
    let x = input.to_bits();
    if x << 1 == 0 {
        return f64::from_bits((x & !(MANTISSA | EXPONENT)) | EXPONENT);
    }
    let e = ((x >> 52) & 0x7FF) as u32;
    if e == 0x7FF {
        if x & MANTISSA == 0 {
            return if (x as i64) < 0 { f64::from_bits(0x7FF8_0000_0000_0000) } else { 0.0 };
        }
        return input;
    }
    if (x as i64) < 0 {
        return f64::from_bits(0x7FF8_0000_0000_0000);
    }
    let index = ((x >> 48) & 0x1F) as usize;
    let step = ((x >> 37) & 0x7FF) as u32;
    let (offset, slope) = FRSQRTE_TABLE[index];
    let sum = offset.wrapping_sub(slope.wrapping_mul(step)) as i32;
    let e = u64::from(1023_u32.wrapping_sub(e.wrapping_sub(1021) >> 1) & 0x7FF);
    let x = (x & !(EXPONENT | MANTISSA)) | (e << 52);
    f64::from_bits(x.wrapping_add((i64::from(sum) as u64) << 26))
}

/// `fres` as Espresso computes it: a table estimate from 15 mantissa bits.
pub(crate) fn fres_espresso(input: f64) -> f64 {
    let x = input.to_bits();
    let index = ((x >> 47) & 0x1F) as usize;
    let step = ((x >> 37) & 0x3FF) as u32;
    let (offset, slope) = FRES_TABLE[index];
    let sum = offset - (slope * step).div_ceil(2);
    let e = ((x >> 52) & 0x7FF) as u32;
    if e == 0 {
        return f64::from_bits(x | EXPONENT);
    }
    if e == 0x7FF {
        if x & MANTISSA == 0 {
            return if (x as i64) < 0 { -0.0 } else { 0.0 };
        }
        return input;
    }
    let e = u64::from(2045_u32.wrapping_sub(e) & 0x7FF);
    let x = (x & !(EXPONENT | MANTISSA)) | (e << 52);
    f64::from_bits(x + (u64::from(sum) << 29))
}

/// Arithmetic shared by double, single and paired-single forms.
pub(super) fn arith(op: Opcode, a: f64, b: f64, c: f64) -> f64 {
    use Opcode as O;
    match op {
        O::Fadd | O::Fadds | O::PsAdd => a + b,
        O::Fsub | O::Fsubs | O::PsSub => a - b,
        O::Fmul | O::Fmuls | O::PsMul => a * c,
        O::Fdiv | O::Fdivs | O::PsDiv => a / b,
        O::Fmadd | O::Fmadds | O::PsMadd => a.mul_add(c, b),
        O::Fmsub | O::Fmsubs | O::PsMsub => a.mul_add(c, -b),
        O::Fnmadd | O::Fnmadds | O::PsNmadd => -a.mul_add(c, b),
        O::Fnmsub | O::Fnmsubs | O::PsNmsub => -a.mul_add(c, -b),
        O::Fres | O::PsRes => fres_espresso(b),
        O::Frsqrte | O::PsRsqrte => frsqrte_espresso(b),
        O::Fsel | O::PsSel => {
            if a >= 0.0 {
                c
            } else {
                b
            }
        }
        _ => f64::NAN,
    }
}

pub(super) fn execute(op: Opcode, i: Instr, s: &mut CpuState) -> ExecResult {
    use Opcode as O;
    let (d, a, b, c) = (i.rd(), i.ra(), i.rb(), i.rc_reg());
    let (fa, fb, fc) = (s.fpr[a].ps0, s.fpr[b].ps0, s.fpr[c].ps0);
    match op {
        // Single precision: result rounded and written to both slots.
        O::Fadds
        | O::Fsubs
        | O::Fmuls
        | O::Fdivs
        | O::Fmadds
        | O::Fmsubs
        | O::Fnmadds
        | O::Fnmsubs
        | O::Fres => {
            s.fpr[d] = Fpr::splat(round_single(arith(op, fa, fb, fc)));
        }
        // Double precision: ps0 only.
        O::Fadd
        | O::Fsub
        | O::Fmul
        | O::Fdiv
        | O::Fmadd
        | O::Fmsub
        | O::Fnmadd
        | O::Fnmsub
        | O::Frsqrte
        | O::Fsel => {
            s.fpr[d].ps0 = arith(op, fa, fb, fc);
        }
        O::Frsp => s.fpr[d] = Fpr::splat(round_single(fb)),
        O::Fctiw | O::Fctiwz => {
            let rounding = if op == O::Fctiwz { 1 } else { s.fpscr & 3 };
            s.fpr[d].ps0 = f64::from_bits(FCTIW_UPPER | u64::from(to_int32(fb, rounding)));
        }
        O::Fmr | O::Fneg | O::Fabs | O::Fnabs => s.fpr[d].ps0 = with_sign(fb, op),
        O::Fcmpu | O::Fcmpo => compare(s, i.crfd(), fa, fb),
        O::Mffs => s.fpr[d].ps0 = f64::from_bits(u64::from(s.fpscr)),
        O::Mtfsf => {
            let value = s.fpr[b].ps0.to_bits() as u32;
            let mask = (0..8)
                .filter(|field| i.fm() & (0x80 >> field) != 0)
                .fold(0_u32, |mask, field| mask | (0xF000_0000 >> (4 * field)));
            s.fpscr = (s.fpscr & !mask) | (value & mask);
        }
        O::Mtfsfi => {
            let shift = 28 - 4 * i.crfd() as u32;
            s.fpscr = (s.fpscr & !(0xF << shift)) | (i.fpscr_imm() << shift);
        }
        O::Mtfsb0 => s.fpscr &= !(0x8000_0000 >> d),
        O::Mtfsb1 => s.fpscr |= 0x8000_0000 >> d,
        O::Mcrfs => {
            let shift = 28 - 4 * i.crfs() as u32;
            s.set_cr_field(i.crfd(), (s.fpscr >> shift) & 0xF);
        }
        _ => return unimplemented(op),
    }
    if i.rc() && !matches!(op, O::Fcmpu | O::Fcmpo | O::Mcrfs) {
        s.update_cr1();
    }
    Ok(StepEvent::Continue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::asm::{a, x};

    fn exec(s: &mut CpuState, raw: u32) {
        execute(crate::decode(raw), Instr(raw), s).unwrap();
    }

    #[test]
    fn single_ops_fill_both_slots() {
        let mut s = CpuState::default();
        s.fpr[2].ps0 = 1.0;
        s.fpr[3].ps0 = 0.1;
        exec(&mut s, a(59, 1, 2, 3, 0, 21)); // fadds f1, f2, f3
        assert_eq!(s.fpr[1].ps0, f64::from(1.1_f32));
        assert_eq!(s.fpr[1].ps0, s.fpr[1].ps1);
    }

    #[test]
    fn fused_multiply_add() {
        let mut s = CpuState::default();
        s.fpr[2].ps0 = 2.0;
        s.fpr[3].ps0 = 3.0;
        s.fpr[4].ps0 = 4.0;
        exec(&mut s, a(63, 1, 2, 4, 3, 29)); // fmadd f1, f2, f3, f4 = 2*3+4
        assert_eq!(s.fpr[1].ps0, 10.0);
    }

    #[test]
    fn convert_to_integer() {
        let mut s = CpuState::default();
        s.fpr[2].ps0 = -2.7;
        exec(&mut s, x(63, 1, 0, 2, 15, false)); // fctiwz f1, f2
        assert_eq!(s.fpr[1].ps0.to_bits() as u32 as i32, -2);
        s.fpr[2].ps0 = 1e20;
        exec(&mut s, x(63, 1, 0, 2, 14, false)); // fctiw (round to nearest)
        assert_eq!(s.fpr[1].ps0.to_bits() as u32, 0x7FFF_FFFF);
    }

    #[test]
    fn compare_sets_cr() {
        let mut s = CpuState::default();
        s.fpr[1].ps0 = f64::NAN;
        exec(&mut s, x(63, 6 << 2, 1, 2, 0, false)); // fcmpu cr6, f1, f2
        assert_eq!(s.cr_field(6), 0b0001);
    }
}

#[cfg(test)]
mod estimate_tests {
    use super::{fres_espresso, frsqrte_espresso};

    #[test]
    fn estimates_stay_close_to_the_exact_values() {
        for v in [0.001_f64, 0.5, 1.0, 2.0, 3.7, 100.0, 12345.0] {
            assert!(frsqrte_espresso(v).mul_add(v.sqrt(), -1.0).abs() < 1e-3, "frsqrte {v}");
            assert!(fres_espresso(v).mul_add(v, -1.0).abs() < 1e-3, "fres {v}");
        }
        assert_eq!(frsqrte_espresso(0.0), f64::INFINITY);
        assert!(frsqrte_espresso(-1.0).is_nan());
        assert_eq!(fres_espresso(f64::INFINITY), 0.0);
    }
}
