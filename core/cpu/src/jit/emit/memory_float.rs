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
//! │ decoded            │   │ jit/emit/memory_float.rs │   │ code to compile    │
//! │ instructions       │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ PowerPC → Cranelift IR:  │   │                    │
//! │                    │   │ memory float             │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "memory float" share of emit.rs (PowerPC → Cranelift IR), moved here so that no file is
//! longer than 500 lines. It works exactly as if it were still written in that file. Turns each
//! PowerPC instruction into the intermediate language of Cranelift, the compiler that produces
//! native code. Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.

use super::*;

impl Emit<'_, '_> {
    /// [`Self::instruction`] for one group of opcodes. Loads and stores. `None` when `op` is not one
    /// of them.
    pub(super) fn memory_op(&mut self, op: Opcode, i: Instr, _pc: u32) -> Option<Flow> {
        use Opcode as O;
        let rd = i.rd();
        match op {
            // ---- Loads and stores ----
            O::Lwz | O::Lwzu | O::Lhz | O::Lhzu | O::Lha | O::Lhau | O::Lbz | O::Lbzu => {
                let update = matches!(op, O::Lwzu | O::Lhzu | O::Lhau | O::Lbzu);
                let ea = self.ea_d(i, update);
                let size = match op {
                    O::Lwz | O::Lwzu => 4,
                    O::Lhz | O::Lhzu | O::Lha | O::Lhau => 2,
                    _ => 1,
                };
                self.integer_load(i, ea, size, matches!(op, O::Lha | O::Lhau), update);
            }
            O::Lwzx | O::Lwzux | O::Lhzx | O::Lhzux | O::Lhax | O::Lhaux | O::Lbzx | O::Lbzux => {
                let update = matches!(op, O::Lwzux | O::Lhzux | O::Lhaux | O::Lbzux);
                let ea = self.ea_x(i, update);
                let size = match op {
                    O::Lwzx | O::Lwzux => 4,
                    O::Lhzx | O::Lhzux | O::Lhax | O::Lhaux => 2,
                    _ => 1,
                };
                self.integer_load(i, ea, size, matches!(op, O::Lhax | O::Lhaux), update);
            }
            O::Stw | O::Stwu | O::Sth | O::Sthu | O::Stb | O::Stbu => {
                let update = matches!(op, O::Stwu | O::Sthu | O::Stbu);
                let ea = self.ea_d(i, update);
                let size = match op {
                    O::Stw | O::Stwu => 4,
                    O::Sth | O::Sthu => 2,
                    _ => 1,
                };
                self.integer_store(i, ea, size, update);
            }
            O::Stwx | O::Stwux | O::Sthx | O::Sthux | O::Stbx | O::Stbux => {
                let update = matches!(op, O::Stwux | O::Sthux | O::Stbux);
                let ea = self.ea_x(i, update);
                let size = match op {
                    O::Stwx | O::Stwux => 4,
                    O::Sthx | O::Sthux => 2,
                    _ => 1,
                };
                self.integer_store(i, ea, size, update);
            }
            O::Lwbrx => {
                let ea = self.ea_x(i, false);
                let address = self.host_address(ea);
                let value = self.b.ins().load(I32, flags(), address, 0);
                self.set_gpr(rd, value);
            }
            O::Lhbrx => {
                let ea = self.ea_x(i, false);
                let address = self.host_address(ea);
                let value = self.b.ins().uload16(I32, flags(), address, 0);
                self.set_gpr(rd, value);
            }
            O::Stwbrx => {
                let ea = self.ea_x(i, false);
                let value = self.gpr(rd);
                let address = self.host_address(ea);
                self.b.ins().store(flags(), value, address, 0);
            }
            O::Sthbrx => {
                let ea = self.ea_x(i, false);
                let value = self.gpr(rd);
                let half = self.b.ins().ireduce(I16, value);
                let address = self.host_address(ea);
                self.b.ins().store(flags(), half, address, 0);
            }
            O::Lmw => {
                let base = self.ea_d(i, false);
                for (n, reg) in (rd..32).enumerate() {
                    let ea = self.b.ins().iadd_imm_s(base, 4 * n as i64);
                    let value = self.load32(ea);
                    self.set_gpr(reg, value);
                }
            }
            O::Stmw => {
                let base = self.ea_d(i, false);
                for (n, reg) in (rd..32).enumerate() {
                    let ea = self.b.ins().iadd_imm_s(base, 4 * n as i64);
                    let value = self.gpr(reg);
                    self.store32(ea, value);
                }
            }
            O::Lfs | O::Lfsu => {
                let update = op == O::Lfsu;
                let ea = self.ea_d(i, update);
                self.load_single(i, ea, update);
            }
            O::Lfsx | O::Lfsux => {
                let update = op == O::Lfsux;
                let ea = self.ea_x(i, update);
                self.load_single(i, ea, update);
            }
            O::Lfd | O::Lfdu => {
                let update = op == O::Lfdu;
                let ea = self.ea_d(i, update);
                self.load_double(i, ea, update);
            }
            O::Lfdx | O::Lfdux => {
                let update = op == O::Lfdux;
                let ea = self.ea_x(i, update);
                self.load_double(i, ea, update);
            }
            O::Stfs | O::Stfsu => {
                let update = op == O::Stfsu;
                let ea = self.ea_d(i, update);
                self.store_single(i, ea, update);
            }
            O::Stfsx | O::Stfsux => {
                let update = op == O::Stfsux;
                let ea = self.ea_x(i, update);
                self.store_single(i, ea, update);
            }
            O::Stfd | O::Stfdu => {
                let update = op == O::Stfdu;
                let ea = self.ea_d(i, update);
                self.store_double(i, ea, update);
            }
            O::Stfdx | O::Stfdux => {
                let update = op == O::Stfdux;
                let ea = self.ea_x(i, update);
                self.store_double(i, ea, update);
            }

            _ => return None,
        }
        Some(Flow::Next)
    }
    /// [`Self::instruction`] for one group of opcodes. Floating point and paired singles. `None` when `op` is not one
    /// of them.
    pub(super) fn float_op(&mut self, op: Opcode, i: Instr, pc: u32) -> Option<Flow> {
        use Opcode as O;
        let (rd, ra, rb) = (i.rd(), i.ra(), i.rb());
        match op {
            // ---- Floating point (record forms use the interpreter) ----
            O::Fadds
            | O::Fsubs
            | O::Fmuls
            | O::Fdivs
            | O::Fmadds
            | O::Fmsubs
            | O::Fnmadds
            | O::Fnmsubs
                if !i.rc() =>
            {
                if let Some(value) = self.arith(op, i, 0) {
                    self.set_single(i, value);
                }
            }
            O::Fadd | O::Fsub | O::Fmul | O::Fdiv | O::Fmadd | O::Fmsub | O::Fnmadd | O::Fnmsub
                if !i.rc() =>
            {
                if let Some(value) = self.arith(op, i, 0) {
                    self.set_fpr(rd, 0, value);
                }
            }
            O::Frsp if !i.rc() => {
                let b = self.fpr(rb, 0);
                self.set_single(i, b);
            }
            O::Fmr | O::Fneg | O::Fabs | O::Fnabs if !i.rc() => {
                let b = self.fpr(rb, 0);
                let value = self.sign_op(op, b);
                self.set_fpr(rd, 0, value);
            }
            O::Fcmpu | O::Fcmpo => {
                let a = self.fpr(ra, 0);
                let b = self.fpr(rb, 0);
                self.float_compare(i.crfd(), a, b);
            }

            O::Fctiwz if !i.rc() => {
                let b = self.fpr(rb, 0);
                let converted = self.b.ins().fcvt_to_sint_sat(I32, b);
                let nan = self.b.ins().fcmp(FloatCC::Unordered, b, b);
                let invalid = self.c32(0x8000_0000);
                let value = self.b.ins().select(nan, invalid, converted);
                let wide = self.b.ins().uextend(I64, value);
                let upper = self.b.ins().iconst(I64, 0xFFF8_0000_0000_0000_u64 as i64);
                let bits = self.b.ins().bor(wide, upper);
                let result = self.b.ins().bitcast(F64, MemFlagsData::new(), bits);
                self.set_fpr(rd, 0, result);
            }
            O::Stfiwx => {
                let ea = self.ea_x(i, false);
                let value = self.fpr(rd, 0);
                let bits = self.b.ins().bitcast(I64, MemFlagsData::new(), value);
                let low = self.b.ins().ireduce(I32, bits);
                self.store32(ea, low);
            }
            O::Fsel if !i.rc() => {
                let a = self.fpr(ra, 0);
                let b = self.fpr(rb, 0);
                let c = self.fpr(i.rc_reg(), 0);
                let zero = self.b.ins().f64const(0.0);
                let positive = self.b.ins().fcmp(FloatCC::GreaterThanOrEqual, a, zero);
                let value = self.b.ins().select(positive, c, b);
                self.set_fpr(rd, 0, value);
            }

            // ---- Paired singles ----
            O::PsqL | O::PsqLu | O::PsqLx | O::PsqLux => {
                let indexed = matches!(op, O::PsqLx | O::PsqLux);
                let (gqr, single) =
                    if indexed { (i.psqx_i(), i.psqx_w()) } else { (i.psq_i(), i.psq_w()) };
                // Bit 2 of the load type: the integer forms.
                self.leave_unless_float(pc, gqr, 0x4_0000);
                let (ea, update) = self.psq_ea(i, op);
                let raw = self.load32(ea);
                let first = self.b.ins().bitcast(F32, MemFlagsData::new(), raw);
                let ps0 = self.b.ins().fpromote(F64, first);
                let ps1 = if single {
                    self.b.ins().f64const(1.0)
                } else {
                    let at = self.b.ins().iadd_imm_s(ea, 4);
                    let raw = self.load32(at);
                    let second = self.b.ins().bitcast(F32, MemFlagsData::new(), raw);
                    self.b.ins().fpromote(F64, second)
                };
                self.set_fpr(rd, 0, ps0);
                self.set_fpr(rd, 1, ps1);
                if update {
                    self.set_gpr(ra, ea);
                }
            }
            O::PsqSt | O::PsqStu | O::PsqStx | O::PsqStux => {
                let indexed = matches!(op, O::PsqStx | O::PsqStux);
                let (gqr, single) =
                    if indexed { (i.psqx_i(), i.psqx_w()) } else { (i.psq_i(), i.psq_w()) };
                // Bit 2 of the store type: the integer forms.
                self.leave_unless_float(pc, gqr, 0x4);
                let (ea, update) = self.psq_ea(i, op);
                let slots: &[(usize, i64)] = if single { &[(0, 0)] } else { &[(0, 0), (1, 4)] };
                for &(slot, offset) in slots {
                    let value = self.fpr(rd, slot);
                    let narrow = self.b.ins().fdemote(F32, value);
                    let raw = self.b.ins().bitcast(I32, MemFlagsData::new(), narrow);
                    let at = self.b.ins().iadd_imm_s(ea, offset);
                    self.store32(at, raw);
                }
                if update {
                    self.set_gpr(ra, ea);
                }
            }
            O::PsAdd
            | O::PsSub
            | O::PsMul
            | O::PsDiv
            | O::PsMadd
            | O::PsMsub
            | O::PsNmadd
            | O::PsNmsub
                if !i.rc() =>
            {
                if let (Some(ps0), Some(ps1)) = (self.arith(op, i, 0), self.arith(op, i, 1)) {
                    self.set_paired(i, ps0, ps1);
                }
            }
            O::PsSel if !i.rc() => {
                let zero = self.b.ins().f64const(0.0);
                let mut slots = [zero; 2];
                for (slot, out) in slots.iter_mut().enumerate() {
                    let a = self.fpr(ra, slot);
                    let b = self.fpr(rb, slot);
                    let c = self.fpr(i.rc_reg(), slot);
                    let positive = self.b.ins().fcmp(FloatCC::GreaterThanOrEqual, a, zero);
                    *out = self.b.ins().select(positive, c, b);
                }
                self.set_paired(i, slots[0], slots[1]);
            }
            O::PsMuls0 if !i.rc() => self.scaled(i, 0, false),
            O::PsMuls1 if !i.rc() => self.scaled(i, 1, false),
            O::PsMadds0 if !i.rc() => self.scaled(i, 0, true),
            O::PsMadds1 if !i.rc() => self.scaled(i, 1, true),
            O::PsSum0 if !i.rc() => {
                let a = self.fpr(ra, 0);
                let b = self.fpr(rb, 1);
                let c = self.fpr(i.rc_reg(), 1);
                let sum = self.b.ins().fadd(a, b);
                self.set_paired(i, sum, c);
            }
            O::PsSum1 if !i.rc() => {
                let a = self.fpr(ra, 0);
                let b = self.fpr(rb, 1);
                let c = self.fpr(i.rc_reg(), 0);
                let sum = self.b.ins().fadd(a, b);
                self.set_paired(i, c, sum);
            }
            O::PsMerge00 | O::PsMerge01 | O::PsMerge10 | O::PsMerge11 if !i.rc() => {
                let first = self.fpr(ra, usize::from(matches!(op, O::PsMerge10 | O::PsMerge11)));
                let second = self.fpr(rb, usize::from(matches!(op, O::PsMerge01 | O::PsMerge11)));
                self.set_fpr(rd, 0, first);
                self.set_fpr(rd, 1, second);
            }
            O::PsMr | O::PsNeg | O::PsAbs | O::PsNabs if !i.rc() => {
                let ps0 = self.fpr(rb, 0);
                let ps1 = self.fpr(rb, 1);
                let ps0 = self.sign_op(op, ps0);
                let ps1 = self.sign_op(op, ps1);
                self.set_fpr(rd, 0, ps0);
                self.set_fpr(rd, 1, ps1);
            }

            _ => return None,
        }
        Some(Flow::Next)
    }
    /// [`Self::instruction`] for one group of opcodes. Condition register, special registers, cache hints and syncs. `None` when `op` is not one
    /// of them.
    pub(super) fn system_op(&mut self, op: Opcode, i: Instr, _pc: u32) -> Option<Flow> {
        use Opcode as O;
        let (rd, ra, rb) = (i.rd(), i.ra(), i.rb());
        match op {
            // ---- System ----
            O::Mfspr if i.spr() == spr::LR => {
                let value = self.lr();
                self.set_gpr(rd, value);
            }
            O::Mfspr if i.spr() == spr::CTR => {
                let value = self.ctr();
                self.set_gpr(rd, value);
            }
            O::Mtspr if i.spr() == spr::LR => {
                let value = self.gpr(rd);
                self.set_lr(value);
            }
            O::Mtspr if i.spr() == spr::CTR => {
                let value = self.gpr(rd);
                self.set_ctr(value);
            }
            O::Mfcr => {
                let value = self.cr();
                self.set_gpr(rd, value);
            }
            O::Mtcrf => {
                let mask = (0..8)
                    .filter(|field| i.crm() & (0x80 >> field) != 0)
                    .fold(0_u32, |mask, field| mask | (0xF000_0000 >> (4 * field)));
                let cr = self.cr();
                let s = self.gpr(rd);
                let kept = self.b.ins().band_imm_s(cr, i64::from(!mask as i32));
                let taken = self.b.ins().band_imm_s(s, i64::from(mask as i32));
                let value = self.b.ins().bor(kept, taken);
                self.set_cr(value);
            }
            O::Crand
            | O::Crandc
            | O::Creqv
            | O::Crnand
            | O::Crnor
            | O::Cror
            | O::Crorc
            | O::Crxor => {
                let a = self.cr_bit(ra);
                let b = self.cr_bit(rb);
                let value = match op {
                    O::Crand => self.b.ins().band(a, b),
                    O::Crandc => {
                        let inverse = self.b.ins().bxor_imm_s(b, 1);
                        self.b.ins().band(a, inverse)
                    }
                    O::Creqv => {
                        let x = self.b.ins().bxor(a, b);
                        self.b.ins().bxor_imm_s(x, 1)
                    }
                    O::Crnand => {
                        let x = self.b.ins().band(a, b);
                        self.b.ins().bxor_imm_s(x, 1)
                    }
                    O::Crnor => {
                        let x = self.b.ins().bor(a, b);
                        self.b.ins().bxor_imm_s(x, 1)
                    }
                    O::Cror => self.b.ins().bor(a, b),
                    O::Crorc => {
                        let inverse = self.b.ins().bxor_imm_s(b, 1);
                        self.b.ins().bor(a, inverse)
                    }
                    _ => self.b.ins().bxor(a, b),
                };
                let shift = 31 - rd as i64;
                let cr = self.cr();
                let cleared = self.b.ins().band_imm_s(cr, !(1_i64 << shift));
                let placed = self.b.ins().ishl_imm_s(value, shift);
                let merged = self.b.ins().bor(cleared, placed);
                self.set_cr(merged);
            }
            // Zeroes the 32-byte cache line: titles clear and copy memory with it.
            O::Dcbz => {
                let ea = self.ea_x(i, false);
                let line = self.b.ins().band_imm_s(ea, !31);
                let address = self.host_address(line);
                let zero = self.b.ins().iconst(I64, 0);
                for offset in [0, 8, 16, 24] {
                    self.b.ins().store(flags(), zero, address, offset);
                }
            }
            O::Sync
            | O::Isync
            | O::Eieio
            | O::Dcbf
            | O::Dcbst
            | O::Dcbt
            | O::Dcbtst
            | O::Dcbi
            | O::Icbi => {}

            _ => return None,
        }
        Some(Flow::Next)
    }

    pub(super) fn shift_right_algebraic(&mut self, i: Instr, s: Value, amount: Value) {
        let wide = self.b.ins().sextend(I64, s);
        let shifted = self.b.ins().sshr(wide, amount);
        let value = self.b.ins().ireduce(I32, shifted);
        // CA: negative source and one bits shifted out.
        let back = self.b.ins().ishl(shifted, amount);
        let lost = self.b.ins().icmp(IntCC::NotEqual, back, wide);
        let negative = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, s, 0);
        let carry = self.b.ins().band(lost, negative);
        let carry = self.b.ins().uextend(I32, carry);
        self.set_ca(carry);
        self.set_gpr(i.ra(), value);
        self.record(i, value);
    }
}
