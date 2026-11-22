//! Part of [`super`]: see its documentation.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                         ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ decoded            │   │ jit/emit/integer.rs      │   │ code to compile    │
//! │ instructions       │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ PowerPC → Cranelift IR:  │   │                    │
//! │                    │   │ integer                  │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "integer" share of emit.rs (PowerPC → Cranelift IR), moved here so that no file is
//! longer than 500 lines. It works exactly as if it were still written in that file. Turns each
//! PowerPC instruction into the intermediate language of Cranelift, the compiler that produces
//! native code. Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.

use super::*;

impl Emit<'_, '_> {
    /// [`Self::instruction`] for one group of opcodes. Integer immediates, rotates and XO-form arithmetic. `None` when `op` is not one
    /// of them.
    pub(super) fn integer_op(&mut self, op: Opcode, i: Instr, _pc: u32) -> Option<Flow> {
        use Opcode as O;
        let (rd, ra, rb) = (i.rd(), i.ra(), i.rb());
        match op {
            // ---- Integer immediates ----
            O::Addi => {
                let base = self.ra_or_zero(ra);
                let value = self.b.ins().iadd_imm_s(base, i64::from(i.simm()));
                self.set_gpr(rd, value);
            }
            O::Addis => {
                let base = self.ra_or_zero(ra);
                let value = self.b.ins().iadd_imm_s(base, i64::from(i.simm() << 16));
                self.set_gpr(rd, value);
            }
            O::Addic | O::AddicRc => {
                let a = self.gpr(ra);
                let imm = self.c32(i.simm() as u32);
                let (result, carry) = self.add_with_carry(a, imm, None);
                self.set_gpr(rd, result);
                self.set_ca(carry);
                if op == O::AddicRc {
                    self.update_cr0(result);
                }
            }
            O::Subfic => {
                let a = self.gpr(ra);
                let inverted = self.not(a);
                let imm = self.c32(i.simm() as u32);
                let one = self.c32(1);
                let (result, carry) = self.add_with_carry(inverted, imm, Some(one));
                self.set_gpr(rd, result);
                self.set_ca(carry);
            }
            O::Mulli => {
                let a = self.gpr(ra);
                let imm = self.c32(i.simm() as u32);
                let value = self.b.ins().imul(a, imm);
                self.set_gpr(rd, value);
            }
            O::Cmpi => {
                let a = self.gpr(ra);
                let imm = self.c32(i.simm() as u32);
                self.compare(i.crfd(), a, imm, true);
            }
            O::Cmpli => {
                let a = self.gpr(ra);
                let imm = self.c32(i.uimm());
                self.compare(i.crfd(), a, imm, false);
            }
            O::Ori | O::Oris | O::Xori | O::Xoris | O::AndiRc | O::AndisRc => {
                let s = self.gpr(rd);
                let imm = if matches!(op, O::Oris | O::Xoris | O::AndisRc) {
                    i.uimm() << 16
                } else {
                    i.uimm()
                };
                let imm = i64::from(imm as i32);
                let value = match op {
                    O::Ori | O::Oris => self.b.ins().bor_imm_s(s, imm),
                    O::Xori | O::Xoris => self.b.ins().bxor_imm_s(s, imm),
                    _ => self.b.ins().band_imm_s(s, imm),
                };
                self.set_gpr(ra, value);
                if matches!(op, O::AndiRc | O::AndisRc) {
                    self.update_cr0(value);
                }
            }

            // ---- Rotates ----
            O::Rlwinm => {
                let s = self.gpr(rd);
                let rotated =
                    if i.sh() == 0 { s } else { self.b.ins().rotl_imm_s(s, i64::from(i.sh())) };
                let value =
                    self.b.ins().band_imm_s(rotated, i64::from(rotate_mask(i.mb(), i.me()) as i32));
                self.set_gpr(ra, value);
                self.record(i, value);
            }
            O::Rlwnm => {
                let s = self.gpr(rd);
                let amount = self.gpr(rb);
                let amount = self.b.ins().band_imm_s(amount, 31);
                let rotated = self.b.ins().rotl(s, amount);
                let value =
                    self.b.ins().band_imm_s(rotated, i64::from(rotate_mask(i.mb(), i.me()) as i32));
                self.set_gpr(ra, value);
                self.record(i, value);
            }
            O::Rlwimi => {
                let s = self.gpr(rd);
                let old = self.gpr(ra);
                let mask = rotate_mask(i.mb(), i.me());
                let rotated =
                    if i.sh() == 0 { s } else { self.b.ins().rotl_imm_s(s, i64::from(i.sh())) };
                let inserted = self.b.ins().band_imm_s(rotated, i64::from(mask as i32));
                let kept = self.b.ins().band_imm_s(old, i64::from(!mask as i32));
                let value = self.b.ins().bor(inserted, kept);
                self.set_gpr(ra, value);
                self.record(i, value);
            }

            // ---- XO-form arithmetic (overflow-enabled forms use the interpreter) ----
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
                if i.oe() =>
            {
                return Some(Flow::Fallback);
            }
            O::Mullw if i.oe() => return Some(Flow::Fallback),
            O::Add => {
                let a = self.gpr(ra);
                let b = self.gpr(rb);
                let value = self.b.ins().iadd(a, b);
                self.set_gpr(rd, value);
                self.record(i, value);
            }
            O::Subf => {
                let a = self.gpr(ra);
                let b = self.gpr(rb);
                let value = self.b.ins().isub(b, a);
                self.set_gpr(rd, value);
                self.record(i, value);
            }
            O::Neg => {
                let a = self.gpr(ra);
                let value = self.b.ins().ineg(a);
                self.set_gpr(rd, value);
                self.record(i, value);
            }
            O::Addc
            | O::Adde
            | O::Addze
            | O::Addme
            | O::Subfc
            | O::Subfe
            | O::Subfze
            | O::Subfme => {
                let a = self.gpr(ra);
                let first = if matches!(op, O::Addc | O::Adde | O::Addze | O::Addme) {
                    a
                } else {
                    self.not(a)
                };
                let second = match op {
                    O::Addc | O::Adde | O::Subfc | O::Subfe => self.gpr(rb),
                    O::Addze | O::Subfze => self.c32(0),
                    _ => self.c32(u32::MAX),
                };
                let carry_in = match op {
                    O::Addc => None,
                    O::Subfc => Some(self.c32(1)),
                    _ => Some(self.ca()),
                };
                let (result, carry) = self.add_with_carry(first, second, carry_in);
                self.set_gpr(rd, result);
                self.set_ca(carry);
                self.record(i, result);
            }
            O::Mullw => {
                let a = self.gpr(ra);
                let b = self.gpr(rb);
                let value = self.b.ins().imul(a, b);
                self.set_gpr(rd, value);
                self.record(i, value);
            }
            O::Mulhw => {
                let a = self.gpr(ra);
                let b = self.gpr(rb);
                let value = self.b.ins().smulhi(a, b);
                self.set_gpr(rd, value);
                self.record(i, value);
            }
            O::Mulhwu => {
                let a = self.gpr(ra);
                let b = self.gpr(rb);
                let value = self.b.ins().umulhi(a, b);
                self.set_gpr(rd, value);
                self.record(i, value);
            }
            O::Cmp => {
                let a = self.gpr(ra);
                let b = self.gpr(rb);
                self.compare(i.crfd(), a, b, true);
            }
            O::Cmpl => {
                let a = self.gpr(ra);
                let b = self.gpr(rb);
                self.compare(i.crfd(), a, b, false);
            }

            _ => return None,
        }
        Some(Flow::Next)
    }
    /// [`Self::instruction`] for one group of opcodes. Logic and branches. `None` when `op` is not one
    /// of them.
    pub(super) fn logic_branch_op(&mut self, op: Opcode, i: Instr, pc: u32) -> Option<Flow> {
        use Opcode as O;
        let (rd, ra, rb) = (i.rd(), i.ra(), i.rb());
        match op {
            // ---- Logic ----
            O::And | O::Andc | O::Or | O::Orc | O::Xor | O::Nor | O::Nand | O::Eqv => {
                let s = self.gpr(rd);
                let b = self.gpr(rb);
                let value = match op {
                    O::And => self.b.ins().band(s, b),
                    O::Andc => self.b.ins().band_not(s, b),
                    O::Or => self.b.ins().bor(s, b),
                    O::Orc => self.b.ins().bor_not(s, b),
                    O::Xor => self.b.ins().bxor(s, b),
                    O::Nor => {
                        let v = self.b.ins().bor(s, b);
                        self.not(v)
                    }
                    O::Nand => {
                        let v = self.b.ins().band(s, b);
                        self.not(v)
                    }
                    _ => self.b.ins().bxor_not(s, b),
                };
                self.set_gpr(ra, value);
                self.record(i, value);
            }
            O::Slw | O::Srw => {
                let s = self.gpr(rd);
                let amount = self.gpr(rb);
                let amount = self.b.ins().band_imm_s(amount, 0x3F);
                let wide = self.b.ins().uextend(I64, s);
                let wide_amount = self.b.ins().uextend(I64, amount);
                let shifted = if op == O::Slw {
                    self.b.ins().ishl(wide, wide_amount)
                } else {
                    self.b.ins().ushr(wide, wide_amount)
                };
                let value = self.b.ins().ireduce(I32, shifted);
                self.set_gpr(ra, value);
                self.record(i, value);
            }
            O::Sraw => {
                let s = self.gpr(rd);
                let amount = self.gpr(rb);
                let amount = self.b.ins().band_imm_s(amount, 0x3F);
                let wide_amount = self.b.ins().uextend(I64, amount);
                self.shift_right_algebraic(i, s, wide_amount);
            }
            O::Srawi => {
                let s = self.gpr(rd);
                let wide_amount = self.b.ins().iconst(I64, i64::from(i.sh()));
                self.shift_right_algebraic(i, s, wide_amount);
            }
            O::Cntlzw => {
                let s = self.gpr(rd);
                let value = self.b.ins().clz(s);
                self.set_gpr(ra, value);
                self.record(i, value);
            }
            O::Extsb => {
                let s = self.gpr(rd);
                let byte = self.b.ins().ireduce(I8, s);
                let value = self.b.ins().sextend(I32, byte);
                self.set_gpr(ra, value);
                self.record(i, value);
            }
            O::Extsh => {
                let s = self.gpr(rd);
                let half = self.b.ins().ireduce(I16, s);
                let value = self.b.ins().sextend(I32, half);
                self.set_gpr(ra, value);
                self.record(i, value);
            }

            // ---- Branches ----
            O::B => {
                let target = if i.aa() { i.li() as u32 } else { pc.wrapping_add(i.li() as u32) };
                return Some(self.branch(i, pc, Target::Known(target), None));
            }
            O::Bc => {
                let target = if i.aa() { i.bd() as u32 } else { pc.wrapping_add(i.bd() as u32) };
                let condition = self.branch_condition(i, true);
                return Some(self.branch(i, pc, Target::Known(target), condition));
            }
            O::Bclr => {
                let lr = self.lr();
                let target = self.b.ins().band_imm_s(lr, !3_i64);
                let condition = self.branch_condition(i, true);
                return Some(self.branch(i, pc, Target::Computed(target), condition));
            }
            O::Bcctr => {
                let ctr = self.ctr();
                let target = self.b.ins().band_imm_s(ctr, !3_i64);
                let condition = self.branch_condition(i, false);
                return Some(self.branch(i, pc, Target::Computed(target), condition));
            }

            _ => return None,
        }
        Some(Flow::Next)
    }
}
