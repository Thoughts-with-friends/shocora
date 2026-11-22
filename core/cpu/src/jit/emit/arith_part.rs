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
//! │ decoded            │   │ jit/emit/arith_part.rs   │   │ code to compile    │
//! │ instructions       │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ PowerPC → Cranelift IR:  │   │                    │
//! │                    │   │ arith part               │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "arith part" share of emit.rs (PowerPC → Cranelift IR), moved here so that no file is
//! longer than 500 lines. It works exactly as if it were still written in that file. Turns each
//! PowerPC instruction into the intermediate language of Cranelift, the compiler that produces
//! native code. Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.

use super::*;

impl Emit<'_, '_> {
    /// `a + b + carry` → (result, carry out as `i32`).
    pub(super) fn add_with_carry(
        &mut self,
        a: Value,
        b: Value,
        carry: Option<Value>,
    ) -> (Value, Value) {
        let wide_a = self.b.ins().uextend(I64, a);
        let wide_b = self.b.ins().uextend(I64, b);
        let mut sum = self.b.ins().iadd(wide_a, wide_b);
        if let Some(carry) = carry {
            let wide_carry = self.b.ins().uextend(I64, carry);
            sum = self.b.ins().iadd(sum, wide_carry);
        }
        let result = self.b.ins().ireduce(I32, sum);
        let high = self.b.ins().ushr_imm_s(sum, 32);
        (result, self.b.ins().ireduce(I32, high))
    }

    pub(super) fn not(&mut self, value: Value) -> Value {
        self.b.ins().bnot(value)
    }

    pub(super) fn round_single(&mut self, value: Value) -> Value {
        let single = self.b.ins().fdemote(F32, value);
        self.b.ins().fpromote(F64, single)
    }

    pub(super) fn cr_bit(&mut self, bit: usize) -> Value {
        let cr = self.cr();
        let shifted = self.b.ins().ushr_imm_s(cr, 31 - bit as i64);
        self.b.ins().band_imm_s(shifted, 1)
    }

    /// Effective address of a D-form access; `update` ignores the `(rA|0)` rule.
    pub(super) fn ea_d(&mut self, i: Instr, update: bool) -> Value {
        let base = if update { self.gpr(i.ra()) } else { self.ra_or_zero(i.ra()) };
        self.b.ins().iadd_imm_s(base, i64::from(i.simm()))
    }

    pub(super) fn ea_x(&mut self, i: Instr, update: bool) -> Value {
        let base = if update { self.gpr(i.ra()) } else { self.ra_or_zero(i.ra()) };
        let index = self.gpr(i.rb());
        self.b.ins().iadd(base, index)
    }

    pub(super) fn integer_load(
        &mut self,
        i: Instr,
        ea: Value,
        size: u32,
        signed: bool,
        update: bool,
    ) {
        let address = self.host_address(ea);
        let value = match size {
            4 => {
                let raw = self.b.ins().load(I32, flags(), address, 0);
                self.b.ins().bswap(raw)
            }
            2 => {
                let raw = self.b.ins().load(I16, flags(), address, 0);
                let swapped = self.b.ins().bswap(raw);
                if signed {
                    self.b.ins().sextend(I32, swapped)
                } else {
                    self.b.ins().uextend(I32, swapped)
                }
            }
            _ => self.b.ins().uload8(I32, flags(), address, 0),
        };
        self.set_gpr(i.rd(), value);
        if update {
            self.set_gpr(i.ra(), ea);
        }
    }

    pub(super) fn integer_store(&mut self, i: Instr, ea: Value, size: u32, update: bool) {
        let value = self.gpr(i.rd());
        let address = self.host_address(ea);
        match size {
            4 => {
                let raw = self.b.ins().bswap(value);
                self.b.ins().store(flags(), raw, address, 0);
            }
            2 => {
                let half = self.b.ins().ireduce(I16, value);
                let raw = self.b.ins().bswap(half);
                self.b.ins().store(flags(), raw, address, 0);
            }
            _ => {
                let byte = self.b.ins().ireduce(I8, value);
                self.b.ins().store(flags(), byte, address, 0);
            }
        }
        if update {
            self.set_gpr(i.ra(), ea);
        }
    }

    pub(super) fn load_single(&mut self, i: Instr, ea: Value, update: bool) {
        let raw = self.load32(ea);
        let single = self.b.ins().bitcast(F32, MemFlagsData::new(), raw);
        let value = self.b.ins().fpromote(F64, single);
        self.set_fpr(i.rd(), 0, value);
        self.set_fpr(i.rd(), 1, value);
        if update {
            self.set_gpr(i.ra(), ea);
        }
    }

    pub(super) fn load_double(&mut self, i: Instr, ea: Value, update: bool) {
        let address = self.host_address(ea);
        let raw = self.b.ins().load(I64, flags(), address, 0);
        let swapped = self.b.ins().bswap(raw);
        let value = self.b.ins().bitcast(F64, MemFlagsData::new(), swapped);
        self.set_fpr(i.rd(), 0, value);
        if update {
            self.set_gpr(i.ra(), ea);
        }
    }

    pub(super) fn store_single(&mut self, i: Instr, ea: Value, update: bool) {
        let value = self.fpr(i.rd(), 0);
        let single = self.b.ins().fdemote(F32, value);
        let raw = self.b.ins().bitcast(I32, MemFlagsData::new(), single);
        self.store32(ea, raw);
        if update {
            self.set_gpr(i.ra(), ea);
        }
    }

    pub(super) fn store_double(&mut self, i: Instr, ea: Value, update: bool) {
        let value = self.fpr(i.rd(), 0);
        let raw = self.b.ins().bitcast(I64, MemFlagsData::new(), value);
        let swapped = self.b.ins().bswap(raw);
        let address = self.host_address(ea);
        self.b.ins().store(flags(), swapped, address, 0);
        if update {
            self.set_gpr(i.ra(), ea);
        }
    }

    /// BO/BI of conditional branches as an `i8`-like value, `None` when always taken.
    pub(super) fn branch_condition(&mut self, i: Instr, use_ctr: bool) -> Option<Value> {
        let (bo, bi) = (i.rd(), i.ra());
        let ctr_ok = (use_ctr && bo & 0b00100 == 0).then(|| {
            let ctr = self.ctr();
            let decremented = self.b.ins().iadd_imm_s(ctr, -1);
            self.set_ctr(decremented);
            let cc = if bo & 0b00010 != 0 { IntCC::Equal } else { IntCC::NotEqual };
            self.b.ins().icmp_imm_s(cc, decremented, 0)
        });
        let cond_ok = (bo & 0b10000 == 0).then(|| {
            let bit = self.cr_bit(bi);
            let want = i64::from(bo & 0b01000 != 0);
            self.b.ins().icmp_imm_s(IntCC::Equal, bit, want)
        });
        match (ctr_ok, cond_ok) {
            (Some(a), Some(b)) => Some(self.b.ins().band(a, b)),
            (a, b) => a.or(b),
        }
    }

    /// A branch: the block ends if it is always taken, and goes on with the next
    /// instruction after a conditional one.
    pub(super) fn branch(
        &mut self,
        i: Instr,
        pc: u32,
        target: Target,
        condition: Option<Value>,
    ) -> Flow {
        if i.lk() {
            let next = self.c32(pc.wrapping_add(4));
            self.set_lr(next);
        }
        let destination = self.destination(target, pc, i.lk());
        let Some(condition) = condition else {
            self.b.ins().jump(destination, &[]);
            return Flow::Ended;
        };
        let stay = self.b.create_block();
        self.b.ins().brif(condition, destination, &[], stay, &[]);
        self.b.seal_block(stay);
        self.b.switch_to_block(stay);
        Flow::Next
    }

    /// `psq_l`/`psq_st` are translated for the common float case. With an integer
    /// type in the GQR (`type_bit` set) the interpreter runs the instruction instead.
    pub(super) fn leave_unless_float(&mut self, pc: u32, gqr: usize, type_bit: i64) {
        let offset = (offset_of!(CpuState, gqr) + 4 * gqr) as i32;
        let quant = self.load_state(offset);
        let integer = self.b.ins().band_imm_s(quant, type_bit);
        let float = self.b.create_block();
        let then = Then::Exit(pc.wrapping_add(4));
        let fallback = self.stub(StubKind::Fallback { pc, then });
        self.b.ins().brif(integer, fallback, &[], float, &[]);
        self.b.seal_block(float);
        self.b.switch_to_block(float);
    }

    /// Effective address of a `psq_*`, and whether the form updates `rA`.
    pub(super) fn psq_ea(&mut self, i: Instr, op: Opcode) -> (Value, bool) {
        use Opcode as O;
        let update = matches!(op, O::PsqLu | O::PsqStu | O::PsqLux | O::PsqStux);
        if matches!(op, O::PsqL | O::PsqSt | O::PsqLu | O::PsqStu) {
            let base = if update { self.gpr(i.ra()) } else { self.ra_or_zero(i.ra()) };
            (self.b.ins().iadd_imm_s(base, i64::from(i.psq_d())), update)
        } else {
            (self.ea_x(i, update), update)
        }
    }

    /// Single-precision arithmetic: rounded result in both slots.
    pub(super) fn set_single(&mut self, i: Instr, value: Value) {
        let rounded = self.round_single(value);
        self.set_fpr(i.rd(), 0, rounded);
        self.set_fpr(i.rd(), 1, rounded);
    }

    pub(super) fn set_paired(&mut self, i: Instr, ps0: Value, ps1: Value) {
        let (ps0, ps1) = (self.round_single(ps0), self.round_single(ps1));
        self.set_fpr(i.rd(), 0, ps0);
        self.set_fpr(i.rd(), 1, ps1);
    }

    /// Arithmetic shared by the double, single and paired forms, on slot `slot`.
    pub(super) fn arith(&mut self, op: Opcode, i: Instr, slot: usize) -> Option<Value> {
        use Opcode as O;
        let a = self.fpr(i.ra(), slot);
        let b = self.fpr(i.rb(), slot);
        let c = self.fpr(i.rc_reg(), slot);
        Some(match op {
            O::Fadd | O::Fadds | O::PsAdd => self.b.ins().fadd(a, b),
            O::Fsub | O::Fsubs | O::PsSub => self.b.ins().fsub(a, b),
            O::Fmul | O::Fmuls | O::PsMul => self.b.ins().fmul(a, c),
            O::Fdiv | O::Fdivs | O::PsDiv => self.b.ins().fdiv(a, b),
            O::Fmadd | O::Fmadds | O::PsMadd => self.b.ins().fma(a, c, b),
            O::Fmsub | O::Fmsubs | O::PsMsub => {
                let negated = self.b.ins().fneg(b);
                self.b.ins().fma(a, c, negated)
            }
            O::Fnmadd | O::Fnmadds | O::PsNmadd => {
                let sum = self.b.ins().fma(a, c, b);
                self.b.ins().fneg(sum)
            }
            O::Fnmsub | O::Fnmsubs | O::PsNmsub => {
                let negated = self.b.ins().fneg(b);
                let sum = self.b.ins().fma(a, c, negated);
                self.b.ins().fneg(sum)
            }
            _ => return None,
        })
    }

    /// `ps_muls*`/`ps_madds*`: both slots of `frA` scale by one slot of `frC`.
    pub(super) fn scaled(&mut self, i: Instr, scale_slot: usize, add: bool) {
        let c = self.fpr(i.rc_reg(), scale_slot);
        let mut results = [None; 2];
        for (slot, result) in results.iter_mut().enumerate() {
            let a = self.fpr(i.ra(), slot);
            *result = Some(if add {
                let b = self.fpr(i.rb(), slot);
                self.b.ins().fma(a, c, b)
            } else {
                self.b.ins().fmul(a, c)
            });
        }
        if let [Some(ps0), Some(ps1)] = results {
            self.set_paired(i, ps0, ps1);
        }
    }

    pub(super) fn sign_op(&mut self, op: Opcode, value: Value) -> Value {
        use Opcode as O;
        match op {
            O::Fneg | O::PsNeg => self.b.ins().fneg(value),
            O::Fabs | O::PsAbs => self.b.ins().fabs(value),
            O::Fnabs | O::PsNabs => {
                let absolute = self.b.ins().fabs(value);
                self.b.ins().fneg(absolute)
            }
            _ => value,
        }
    }

    pub(super) fn float_compare(&mut self, field: usize, a: Value, b: Value) {
        let lt = self.b.ins().fcmp(FloatCC::LessThan, a, b);
        let gt = self.b.ins().fcmp(FloatCC::GreaterThan, a, b);
        let eq = self.b.ins().fcmp(FloatCC::Equal, a, b);
        let unordered = self.b.ins().fcmp(FloatCC::Unordered, a, b);
        let unordered = self.b.ins().uextend(I32, unordered);
        let value = self.compare_field(lt, gt, eq, unordered);
        self.set_cr_field(field, value);
        let fpscr = self.load_state(offset_of!(CpuState, fpscr) as i32);
        let cleared = self.b.ins().band_imm_s(fpscr, !0xF000_i64);
        let placed = self.b.ins().ishl_imm_s(value, 12);
        let merged = self.b.ins().bor(cleared, placed);
        self.store_state(offset_of!(CpuState, fpscr) as i32, merged);
    }

    /// Translates one instruction.
    pub(super) fn instruction(&mut self, op: Opcode, i: Instr, pc: u32) -> Flow {
        // Each group answers for its own opcodes; an opcode belongs to one group only.
        self.integer_op(op, i, pc)
            .or_else(|| self.logic_branch_op(op, i, pc))
            .or_else(|| self.memory_op(op, i, pc))
            .or_else(|| self.float_op(op, i, pc))
            .or_else(|| self.system_op(op, i, pc))
            .unwrap_or(Flow::Fallback)
    }
}
