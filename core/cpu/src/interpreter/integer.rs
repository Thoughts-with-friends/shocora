//! Integer arithmetic, logic, rotates, shifts and compares.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                         ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ integer            │   │ interpreter/integer.rs   │   │ results, flags     │
//! │ instructions       │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ integer math             │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Whole-number math, logic and comparisons. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.
#![allow(clippy::cast_possible_wrap, clippy::cast_sign_loss, clippy::cast_possible_truncation)]

use super::{ExecResult, unimplemented};
use crate::{CpuState, Instr, Opcode, StepEvent};

/// `a + b + carry_in` → (result, carry out, signed overflow).
const fn add3(a: u32, b: u32, carry_in: bool) -> (u32, bool, bool) {
    let sum = a as u64 + b as u64 + carry_in as u64;
    let result = sum as u32;
    (result, sum > u32::MAX as u64, ((a ^ result) & (b ^ result)) >> 31 != 0)
}

/// Rotate mask with PowerPC bit numbering (bit 0 = MSB); wraps when `mb > me`.
pub(crate) const fn rotate_mask(mb: u32, me: u32) -> u32 {
    let begin = u32::MAX >> mb;
    let end = u32::MAX << (31 - me);
    if mb <= me { begin & end } else { begin | end }
}

fn compare(s: &mut CpuState, field: usize, less: bool, greater: bool) {
    let value = if less {
        0b1000
    } else if greater {
        0b0100
    } else {
        0b0010
    };
    s.set_cr_field(field, value | u32::from(s.xer_so));
}

fn set_overflow(s: &mut CpuState, overflow: bool) {
    s.xer_ov = overflow;
    s.xer_so |= overflow;
}

/// XO-form arithmetic: returns (result, carry to store, overflow).
fn arithmetic(op: Opcode, s: &CpuState, a: u32, b: u32) -> (u32, Option<bool>, bool) {
    use Opcode as O;
    let ca = s.xer_ca;
    match op {
        O::Add => {
            let (r, _, ov) = add3(a, b, false);
            (r, None, ov)
        }
        O::Addc => {
            let (r, c, ov) = add3(a, b, false);
            (r, Some(c), ov)
        }
        O::Adde => {
            let (r, c, ov) = add3(a, b, ca);
            (r, Some(c), ov)
        }
        O::Addze => {
            let (r, c, ov) = add3(a, 0, ca);
            (r, Some(c), ov)
        }
        O::Addme => {
            let (r, c, ov) = add3(a, u32::MAX, ca);
            (r, Some(c), ov)
        }
        O::Subf => {
            let (r, _, ov) = add3(!a, b, true);
            (r, None, ov)
        }
        O::Subfc => {
            let (r, c, ov) = add3(!a, b, true);
            (r, Some(c), ov)
        }
        O::Subfe => {
            let (r, c, ov) = add3(!a, b, ca);
            (r, Some(c), ov)
        }
        O::Subfze => {
            let (r, c, ov) = add3(!a, 0, ca);
            (r, Some(c), ov)
        }
        O::Subfme => {
            let (r, c, ov) = add3(!a, u32::MAX, ca);
            (r, Some(c), ov)
        }
        O::Neg => (a.wrapping_neg(), None, a == 0x8000_0000),
        O::Mullw => {
            let product = i64::from(a as i32) * i64::from(b as i32);
            (product as u32, None, product != i64::from(product as i32))
        }
        O::Mulhw => (((i64::from(a as i32) * i64::from(b as i32)) >> 32) as u32, None, false),
        O::Mulhwu => (((u64::from(a) * u64::from(b)) >> 32) as u32, None, false),
        O::Divw => {
            let (a, b) = (a as i32, b as i32);
            if b == 0 || (a == i32::MIN && b == -1) {
                (if a < 0 { u32::MAX } else { 0 }, None, true)
            } else {
                ((a / b) as u32, None, false)
            }
        }
        O::Divwu => match a.checked_div(b) {
            Some(q) => (q, None, false),
            None => (0, None, true),
        },
        _ => (0, None, false),
    }
}

pub(super) fn execute(op: Opcode, i: Instr, s: &mut CpuState) -> ExecResult {
    use Opcode as O;
    let (rd, ra, rb) = (i.rd(), i.ra(), i.rb());
    match op {
        // D-form arithmetic
        O::Addi => s.gpr[rd] = s.ra_or_zero(ra).wrapping_add(i.simm() as u32),
        O::Addis => s.gpr[rd] = s.ra_or_zero(ra).wrapping_add(i.uimm() << 16),
        O::Addic | O::AddicRc => {
            let (r, ca, _) = add3(s.gpr[ra], i.simm() as u32, false);
            s.gpr[rd] = r;
            s.xer_ca = ca;
            if op == O::AddicRc {
                s.update_cr0(r);
            }
        }
        O::Subfic => {
            let (r, ca, _) = add3(!s.gpr[ra], i.simm() as u32, true);
            s.gpr[rd] = r;
            s.xer_ca = ca;
        }
        O::Mulli => s.gpr[rd] = (s.gpr[ra] as i32).wrapping_mul(i.simm()) as u32,

        // Compares
        O::Cmpi => {
            let a = s.gpr[ra] as i32;
            compare(s, i.crfd(), a < i.simm(), a > i.simm());
        }
        O::Cmpli => {
            let a = s.gpr[ra];
            compare(s, i.crfd(), a < i.uimm(), a > i.uimm());
        }
        O::Cmp => {
            let (a, b) = (s.gpr[ra] as i32, s.gpr[rb] as i32);
            compare(s, i.crfd(), a < b, a > b);
        }
        O::Cmpl => {
            let (a, b) = (s.gpr[ra], s.gpr[rb]);
            compare(s, i.crfd(), a < b, a > b);
        }

        // Logical immediates (rS → rA)
        O::Ori => s.gpr[ra] = s.gpr[rd] | i.uimm(),
        O::Oris => s.gpr[ra] = s.gpr[rd] | (i.uimm() << 16),
        O::Xori => s.gpr[ra] = s.gpr[rd] ^ i.uimm(),
        O::Xoris => s.gpr[ra] = s.gpr[rd] ^ (i.uimm() << 16),
        O::AndiRc => {
            s.gpr[ra] = s.gpr[rd] & i.uimm();
            s.update_cr0(s.gpr[ra]);
        }
        O::AndisRc => {
            s.gpr[ra] = s.gpr[rd] & (i.uimm() << 16);
            s.update_cr0(s.gpr[ra]);
        }

        // Rotates
        O::Rlwinm | O::Rlwnm | O::Rlwimi => {
            let sh = if op == O::Rlwnm { s.gpr[rb] & 31 } else { i.sh() };
            let mask = rotate_mask(i.mb(), i.me());
            let rotated = s.gpr[rd].rotate_left(sh);
            s.gpr[ra] = if op == O::Rlwimi {
                (rotated & mask) | (s.gpr[ra] & !mask)
            } else {
                rotated & mask
            };
            if i.rc() {
                s.update_cr0(s.gpr[ra]);
            }
        }

        // XO-form arithmetic (rA, rB → rD)
        O::Add
        | O::Addc
        | O::Adde
        | O::Addze
        | O::Addme
        | O::Subf
        | O::Subfc
        | O::Subfe
        | O::Subfze
        | O::Subfme
        | O::Neg
        | O::Mullw
        | O::Mulhw
        | O::Mulhwu
        | O::Divw
        | O::Divwu => {
            let (result, carry, overflow) = arithmetic(op, s, s.gpr[ra], s.gpr[rb]);
            s.gpr[rd] = result;
            if let Some(carry) = carry {
                s.xer_ca = carry;
            }
            if i.oe() && !matches!(op, O::Mulhw | O::Mulhwu) {
                set_overflow(s, overflow);
            }
            if i.rc() {
                s.update_cr0(result);
            }
        }

        // X-form logic and shifts (rS, rB → rA)
        O::And
        | O::Andc
        | O::Or
        | O::Orc
        | O::Xor
        | O::Nor
        | O::Nand
        | O::Eqv
        | O::Slw
        | O::Srw
        | O::Sraw
        | O::Srawi
        | O::Cntlzw
        | O::Extsb
        | O::Extsh => {
            let (rs, b) = (s.gpr[rd], s.gpr[rb]);
            let result = match op {
                O::And => rs & b,
                O::Andc => rs & !b,
                O::Or => rs | b,
                O::Orc => rs | !b,
                O::Xor => rs ^ b,
                O::Nor => !(rs | b),
                O::Nand => !(rs & b),
                O::Eqv => !(rs ^ b),
                O::Slw => rs.checked_shl(b & 0x3F).unwrap_or(0),
                O::Srw => rs.checked_shr(b & 0x3F).unwrap_or(0),
                O::Sraw | O::Srawi => {
                    let n = if op == O::Srawi { i.sh() } else { b & 0x3F };
                    let negative = (rs as i32) < 0;
                    if n >= 32 {
                        s.xer_ca = negative;
                        if negative { u32::MAX } else { 0 }
                    } else {
                        s.xer_ca = negative && n != 0 && rs & ((1 << n) - 1) != 0;
                        ((rs as i32) >> n) as u32
                    }
                }
                O::Cntlzw => rs.leading_zeros(),
                O::Extsb => rs as u8 as i8 as i32 as u32,
                O::Extsh => rs as u16 as i16 as i32 as u32,
                _ => return unimplemented(op),
            };
            s.gpr[ra] = result;
            if i.rc() {
                s.update_cr0(result);
            }
        }
        _ => return unimplemented(op),
    }
    Ok(StepEvent::Continue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::asm::{d, x};

    fn exec(s: &mut CpuState, raw: u32) {
        let op = crate::decode(raw);
        execute(op, Instr(raw), s).unwrap();
    }

    #[test]
    fn rotate_masks() {
        assert_eq!(rotate_mask(0, 31), u32::MAX);
        assert_eq!(rotate_mask(16, 31), 0x0000_FFFF);
        assert_eq!(rotate_mask(30, 1), 0xC000_0003);
    }

    #[test]
    fn carries_and_overflow() {
        let mut s = CpuState::default();
        s.gpr[4] = u32::MAX;
        exec(&mut s, d(12, 3, 4, 1)); // addic r3, r4, 1
        assert_eq!((s.gpr[3], s.xer_ca), (0, true));

        s.gpr[4] = 0x7FFF_FFFF;
        s.gpr[5] = 1;
        exec(&mut s, x(31, 3, 4, 5, 266 | 0x200, true)); // addo. r3, r4, r5
        assert_eq!(s.gpr[3], 0x8000_0000);
        assert!(s.xer_ov && s.xer_so);
        assert_eq!(s.cr_field(0), 0b1001);

        // subfic r3, r4, 0 with r4 = 0 → carry set
        s.gpr[4] = 0;
        exec(&mut s, d(8, 3, 4, 0));
        assert_eq!((s.gpr[3], s.xer_ca), (0, true));
    }

    #[test]
    fn shifts() {
        let mut s = CpuState::default();
        s.gpr[4] = 0x8000_0001;
        exec(&mut s, x(31, 4, 3, 1, 824, false)); // srawi r3, r4, 1
        assert_eq!((s.gpr[3], s.xer_ca), (0xC000_0000, true));
        s.gpr[5] = 40;
        exec(&mut s, x(31, 4, 3, 5, 24, false)); // slw r3, r4, r5
        assert_eq!(s.gpr[3], 0);
        // rlwinm r3, r4, 8, 24, 31  (extract top byte)
        exec(&mut s, (21 << 26) | (4 << 21) | (3 << 16) | (8 << 11) | (24 << 6) | (31 << 1));
        assert_eq!(s.gpr[3], 0x80);
    }

    #[test]
    fn division_edge_cases() {
        let mut s = CpuState::default();
        s.gpr[4] = (-7_i32) as u32;
        s.gpr[5] = 2;
        exec(&mut s, x(31, 3, 4, 5, 491, false)); // divw
        assert_eq!(s.gpr[3] as i32, -3);
        s.gpr[5] = 0;
        exec(&mut s, x(31, 3, 4, 5, 491, false));
        assert_eq!(s.gpr[3], u32::MAX);
        exec(&mut s, x(31, 3, 4, 5, 459, false)); // divwu by 0
        assert_eq!(s.gpr[3], 0);
    }

    #[test]
    fn compares() {
        let mut s = CpuState::default();
        s.gpr[3] = (-1_i32) as u32;
        exec(&mut s, d(11, 7 << 2, 3, 0)); // cmpwi cr7, r3, 0
        assert_eq!(s.cr_field(7), 0b1000);
        exec(&mut s, d(10, 0, 3, 0)); // cmplwi cr0, r3, 0
        assert_eq!(s.cr_field(0), 0b0100);
    }
}
