//! Guest instructions → Cranelift IR.
//!
//! A region is the code reachable from an entry through nearby branches, like a
//! function of Cemu's recompiler. Registers live in Cranelift variables, loaded once
//! on entry and written back only where code leaves the region or calls the
//! interpreter, and only those the paths there may have changed. Loops stay inside,
//! with the registers in host registers.
//!
//! Translation runs on worker threads and only needs the instructions, which
//! the owner of the bus decodes up front ([`decode_block`]).
#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::too_many_lines
)]

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    mem::offset_of,
};

use cranelift_codegen::{
    Context,
    control::ControlPlane,
    ir::{
        AbiParam, Block as IrBlock, InstBuilder, MemFlagsData, SigRef, Signature, Type, Value,
        condcodes::{FloatCC, IntCC},
        types::{F32, F64, I8, I16, I32, I64},
    },
    isa::OwnedTargetIsa,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch, Variable};

use super::{
    HelperContext, MAX_BLOCK_INSTRUCTIONS, MAX_REGION_INSTRUCTIONS, PAGE_BITS, PAGE_ENTRIES,
    REGION_REACH, block_signature, interpret_one,
};
use crate::{
    Bus, CpuError, CpuState, Fpr, Instr, Opcode, decode, interpreter::integer::rotate_mask,
    state::spr,
};

/// Whether translation of the block goes on after an instruction.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    Next,
    /// `state.pc` is already written and the block returned.
    Ended,
    /// Run the interpreter for this instruction.
    Fallback,
}

const fn gpr_offset(index: usize) -> i32 {
    (offset_of!(CpuState, gpr) + 4 * index) as i32
}

const fn fpr_offset(index: usize, slot: usize) -> i32 {
    let field = if slot == 0 { offset_of!(Fpr, ps0) } else { offset_of!(Fpr, ps1) };
    (offset_of!(CpuState, fpr) + size_of::<Fpr>() * index + field) as i32
}

const fn field_offset_lr() -> i32 {
    offset_of!(CpuState, lr) as i32
}

/// Registers kept in Cranelift variables while the region runs: the GPRs, both slots of
/// the FPRs, LR, CTR, CR and XER[CA]. Bit `n` of a [`Mask`] stands for register `n`.
const GPR: usize = 0;
const FPR: [usize; 2] = [32, 64];
const LR: usize = 96;
const CTR: usize = 97;
const CR: usize = 98;
const CA: usize = 99;
const REGISTERS: usize = 100;

type Mask = u128;

/// Where code leaves the region to.
#[derive(Clone, Copy)]
enum Target {
    Known(u32),
    Computed(Value),
}

/// What comes after an instruction the interpreter runs.
#[derive(Clone, Copy)]
enum Then {
    /// Goes on with the translation at this block.
    Resume(IrBlock),
    /// Returns: the interpreter may have redirected `pc`.
    Leave,
    /// Leaves the region for this address.
    Exit(u32),
}

#[derive(Clone, Copy)]
enum StubKind {
    Exit(Target),
    Fallback {
        pc: u32,
        then: Then,
    },
    /// A loop's way back to a leader, past a look at the budget.
    BackEdge {
        leader: usize,
        address: u32,
    },
}

/// Which registers may differ from `state` at a point of the region: what the leader's
/// predecessors left unwritten (unless everything was `flushed` since) and `local`.
#[derive(Clone, Copy)]
struct Dirty {
    leader: usize,
    local: Mask,
    flushed: bool,
}

/// Code that writes registers back. It is emitted after the region, once it is known
/// which registers the paths to it can change.
#[derive(Clone, Copy)]
struct Stub {
    block: IrBlock,
    at: Dirty,
    kind: StubKind,
}

struct Emit<'a, 'b> {
    b: &'a mut FunctionBuilder<'b>,
    state: Value,
    memory: Value,
    helper_context: Value,
    /// Address of `interpret_one` for the bus in use.
    interpret: i64,
    interpret_sig: SigRef,
    block_sig: SigRef,
    /// Address of the first-level table of translations.
    pages: usize,
    /// Where the quick translation counts its runs (0: it does not count).
    counter: usize,
    registers: [Variable; REGISTERS],
    /// Instructions left in the time slice, as an `i64`.
    remaining: Variable,
    /// Registers the region reads or writes: loaded on entry and after the interpreter.
    touched: Mask,
    dirty: Dirty,
    /// The blocks starting at a branch target or after a branch, by index.
    leaders: Vec<IrBlock>,
    leader_of: HashMap<u32, usize>,
    /// Ways from one leader to another, for finding out what each one may have changed.
    edges: Vec<(Dirty, usize)>,
    stubs: Vec<Stub>,
}

fn flags() -> MemFlagsData {
    MemFlagsData::new().with_notrap()
}

/// Where a register lives in [`CpuState`] and its type there.
const fn register_slot(register: usize) -> (i32, Type) {
    match register {
        LR => (field_offset_lr(), I32),
        CTR => (offset_of!(CpuState, ctr) as i32, I32),
        CR => (offset_of!(CpuState, cr) as i32, I32),
        CA => (offset_of!(CpuState, xer_ca) as i32, I8),
        _ if register >= FPR[1] => (fpr_offset(register - FPR[1], 1), F64),
        _ if register >= FPR[0] => (fpr_offset(register - FPR[0], 0), F64),
        _ => (gpr_offset(register), I32),
    }
}

/// The registers of `mask`, lowest first.
fn each(mut mask: Mask) -> impl Iterator<Item = usize> {
    std::iter::from_fn(move || {
        (mask != 0).then(|| {
            let register = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            register
        })
    })
}

impl Emit<'_, '_> {
    fn c32(&mut self, value: u32) -> Value {
        self.b.ins().iconst(I32, i64::from(value as i32))
    }

    fn load_state(&mut self, offset: i32) -> Value {
        self.b.ins().load(I32, flags(), self.state, offset)
    }

    fn store_state(&mut self, offset: i32, value: Value) {
        self.b.ins().store(flags(), value, self.state, offset);
    }

    fn read(&mut self, register: usize) -> Value {
        self.touched |= 1 << register;
        self.b.use_var(self.registers[register])
    }

    fn write(&mut self, register: usize, value: Value) {
        self.touched |= 1 << register;
        self.dirty.local |= 1 << register;
        self.b.def_var(self.registers[register], value);
    }

    fn gpr(&mut self, index: usize) -> Value {
        self.read(GPR + index)
    }

    fn set_gpr(&mut self, index: usize, value: Value) {
        self.write(GPR + index, value);
    }

    /// `(rA|0)`.
    fn ra_or_zero(&mut self, index: usize) -> Value {
        if index == 0 { self.c32(0) } else { self.gpr(index) }
    }

    fn fpr(&mut self, index: usize, slot: usize) -> Value {
        self.read(FPR[slot] + index)
    }

    fn set_fpr(&mut self, index: usize, slot: usize, value: Value) {
        self.write(FPR[slot] + index, value);
    }

    fn lr(&mut self) -> Value {
        self.read(LR)
    }

    fn set_lr(&mut self, value: Value) {
        self.write(LR, value);
    }

    fn ctr(&mut self) -> Value {
        self.read(CTR)
    }

    fn set_ctr(&mut self, value: Value) {
        self.write(CTR, value);
    }

    fn cr(&mut self) -> Value {
        self.read(CR)
    }

    fn set_cr(&mut self, value: Value) {
        self.write(CR, value);
    }

    /// XER[CA] as an `i32` 0/1.
    fn ca(&mut self) -> Value {
        self.read(CA)
    }

    fn set_ca(&mut self, value: Value) {
        self.write(CA, value);
    }

    fn so(&mut self) -> Value {
        self.b.ins().uload8(I32, flags(), self.state, offset_of!(CpuState, xer_so) as i32)
    }

    /// Writes the registers of `mask` back to `state`.
    fn flush(&mut self, mask: Mask) {
        for register in each(mask) {
            let value = self.b.use_var(self.registers[register]);
            let (offset, ty) = register_slot(register);
            let value = if ty == I8 { self.b.ins().ireduce(I8, value) } else { value };
            self.b.ins().store(flags(), value, self.state, offset);
        }
        let remaining = self.b.use_var(self.remaining);
        let offset = offset_of!(HelperContext, remaining) as i32;
        self.b.ins().store(flags(), remaining, self.helper_context, offset);
    }

    /// Takes the registers of `mask` from `state` again: a helper may have changed them.
    fn reload(&mut self, mask: Mask) {
        for register in each(mask) {
            let (offset, ty) = register_slot(register);
            let value = if ty == I8 {
                self.b.ins().uload8(I32, flags(), self.state, offset)
            } else {
                self.b.ins().load(ty, flags(), self.state, offset)
            };
            self.b.def_var(self.registers[register], value);
        }
    }

    fn set_pc(&mut self, pc: Value) {
        self.store_state(offset_of!(CpuState, pc) as i32, pc);
    }

    /// Accounts for the `count` instructions of a leader's block.
    fn charge(&mut self, count: i64) {
        let remaining = self.b.use_var(self.remaining);
        let left = self.b.ins().iadd_imm_s(remaining, -count);
        self.b.def_var(self.remaining, left);
    }

    /// Counts a run of the quick translation, when it counts them.
    fn count_run(&mut self) {
        if self.counter != 0 {
            let counter = self.b.ins().iconst(I64, self.counter as i64);
            let runs = self.b.ins().load(I32, flags(), counter, 0);
            let runs = self.b.ins().iadd_imm_u(runs, 1);
            self.b.ins().store(flags(), runs, counter, 0);
        }
    }

    /// A block with code to emit after the region.
    fn stub(&mut self, kind: StubKind) -> IrBlock {
        let block = self.b.create_block();
        self.stubs.push(Stub { block, at: self.dirty, kind });
        block
    }

    /// Where a branch to `target` goes: straight to a leader of the region, or out.
    fn destination(&mut self, target: Target, from: u32, link: bool) -> IrBlock {
        if let Target::Known(address) = target
            && !link
            && let Some(&leader) = self.leader_of.get(&address)
        {
            self.edges.push((self.dirty, leader));
            // A loop must still give up the host thread when the time slice ends.
            return if address > from {
                self.leaders[leader]
            } else {
                self.stub(StubKind::BackEdge { leader, address })
            };
        }
        self.stub(StubKind::Exit(target))
    }

    /// Returns to [`Jit::run`](super::Jit::run) when `condition` holds.
    fn leave_if(&mut self, condition: Value, stop: Value) {
        let out = self.b.create_block();
        let stay = self.b.create_block();
        self.b.ins().brif(condition, out, &[], stay, &[]);
        self.b.seal_block(out);
        self.b.seal_block(stay);
        self.b.set_cold_block(out);
        self.b.switch_to_block(out);
        self.b.ins().return_(&[stop]);
        self.b.switch_to_block(stay);
    }

    /// Leaves the region with `pc` as the next address, jumping to its translation
    /// when there is one. `known` is the address, when it is a constant.
    fn exit(&mut self, pc: Value, known: Option<u32>, mask: Mask) {
        self.flush(mask);
        self.set_pc(pc);
        let done = self.c32(0);
        let left = self.b.use_var(self.remaining);
        let used_up = self.b.ins().icmp_imm_s(IntCC::SignedLessThanOrEqual, left, 0);
        self.leave_if(used_up, done);

        let page_entry = match known {
            Some(address) => {
                let entry = self.pages + 8 * (address >> PAGE_BITS) as usize;
                self.b.ins().iconst(I64, entry as i64)
            }
            None => {
                let page = self.b.ins().ushr_imm_s(pc, i64::from(PAGE_BITS));
                let page = self.b.ins().uextend(I64, page);
                let scaled = self.b.ins().ishl_imm_s(page, 3);
                let table = self.b.ins().iconst(I64, self.pages as i64);
                self.b.ins().iadd(table, scaled)
            }
        };
        let page = self.b.ins().load(I64, flags(), page_entry, 0);
        let missing = self.b.ins().icmp_imm_s(IntCC::Equal, page, 0);
        self.leave_if(missing, done);
        let slot = match known {
            Some(address) => {
                let slot = ((address >> 2) as usize & (PAGE_ENTRIES - 1)) * 8;
                self.b.ins().iadd_imm_s(page, slot as i64)
            }
            None => {
                let index = self.b.ins().ushr_imm_s(pc, 2);
                let index = self.b.ins().band_imm_s(index, (PAGE_ENTRIES - 1) as i64);
                let index = self.b.ins().uextend(I64, index);
                let scaled = self.b.ins().ishl_imm_s(index, 3);
                self.b.ins().iadd(page, scaled)
            }
        };
        let code = self.b.ins().load(I64, flags(), slot, 0);
        let missing = self.b.ins().icmp_imm_s(IntCC::Equal, code, 0);
        self.leave_if(missing, done);
        self.b.ins().return_call_indirect(
            self.block_sig,
            code,
            &[self.state, self.memory, self.helper_context],
        );
    }

    /// One step of the interpreter at `pc`; returns from the region if it reports an event.
    fn interpret(&mut self, pc: u32, mask: Mask, then: Then) {
        self.flush(mask);
        let address = self.c32(pc);
        self.set_pc(address);
        let helper = self.b.ins().iconst(I64, self.interpret);
        let call = self.b.ins().call_indirect(
            self.interpret_sig,
            helper,
            &[self.state, self.helper_context],
        );
        let stop = self.b.inst_results(call)[0];
        self.leave_if(stop, stop);
        match then {
            Then::Resume(block) => {
                self.reload(self.touched);
                self.b.ins().jump(block, &[]);
            }
            Then::Leave => {
                let done = self.c32(0);
                self.b.ins().return_(&[done]);
            }
            Then::Exit(address) => {
                let next = self.c32(address);
                self.exit(next, Some(address), 0);
            }
        }
    }

    /// Emits the stubs, now that the whole region is known.
    fn finish_stubs(&mut self) {
        // What each leader may find changed: what flows into it along every way there.
        let mut incoming: Vec<Mask> = vec![0; self.leaders.len()];
        let out = |incoming: &[Mask], at: Dirty| {
            if at.flushed { at.local } else { incoming[at.leader] | at.local }
        };
        loop {
            let mut changed = false;
            for &(at, to) in &self.edges {
                let flowing = out(&incoming, at);
                if flowing & !incoming[to] != 0 {
                    incoming[to] |= flowing;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        while let Some(Stub { block, at, kind }) = self.stubs.pop() {
            self.b.switch_to_block(block);
            self.b.seal_block(block);
            if !matches!(kind, StubKind::BackEdge { .. }) {
                self.b.set_cold_block(block);
            }
            let mask = out(&incoming, at);
            match kind {
                StubKind::Exit(Target::Known(address)) => {
                    let pc = self.c32(address);
                    self.exit(pc, Some(address), mask);
                }
                StubKind::Exit(Target::Computed(pc)) => self.exit(pc, None, mask),
                StubKind::Fallback { pc, then } => self.interpret(pc, mask, then),
                StubKind::BackEdge { leader, address } => {
                    // A loop's rounds count as runs: hot loops are what optimizing is for.
                    self.count_run();
                    let left = self.b.use_var(self.remaining);
                    let used_up = self.b.ins().icmp_imm_s(IntCC::SignedLessThanOrEqual, left, 0);
                    let exit = self.b.create_block();
                    self.stubs.push(Stub {
                        block: exit,
                        at,
                        kind: StubKind::Exit(Target::Known(address)),
                    });
                    self.b.ins().brif(used_up, exit, &[], self.leaders[leader], &[]);
                }
            }
        }
    }

    fn host_address(&mut self, ea: Value) -> Value {
        let offset = self.b.ins().uextend(I64, ea);
        self.b.ins().iadd(self.memory, offset)
    }

    fn load32(&mut self, ea: Value) -> Value {
        let address = self.host_address(ea);
        let raw = self.b.ins().load(I32, flags(), address, 0);
        self.b.ins().bswap(raw)
    }

    fn store32(&mut self, ea: Value, value: Value) {
        let address = self.host_address(ea);
        let raw = self.b.ins().bswap(value);
        self.b.ins().store(flags(), raw, address, 0);
    }

    /// `crf = (lt, gt, eq, so)` from `i8` booleans.
    fn set_cr_field(&mut self, field: usize, value: Value) {
        let shift = 28 - 4 * field as i64;
        let cr = self.cr();
        let cleared = self.b.ins().band_imm_s(cr, !(0xF_i64 << shift));
        let placed = self.b.ins().ishl_imm_s(value, shift);
        let merged = self.b.ins().bor(cleared, placed);
        self.set_cr(merged);
    }

    fn compare_field(&mut self, lt: Value, gt: Value, eq: Value, low: Value) -> Value {
        let lt = self.b.ins().uextend(I32, lt);
        let gt = self.b.ins().uextend(I32, gt);
        let eq = self.b.ins().uextend(I32, eq);
        let lt = self.b.ins().ishl_imm_s(lt, 3);
        let gt = self.b.ins().ishl_imm_s(gt, 2);
        let eq = self.b.ins().ishl_imm_s(eq, 1);
        let high = self.b.ins().bor(lt, gt);
        let middle = self.b.ins().bor(high, eq);
        self.b.ins().bor(middle, low)
    }

    fn compare(&mut self, field: usize, a: Value, b: Value, signed: bool) {
        let (less, greater) = if signed {
            (IntCC::SignedLessThan, IntCC::SignedGreaterThan)
        } else {
            (IntCC::UnsignedLessThan, IntCC::UnsignedGreaterThan)
        };
        let lt = self.b.ins().icmp(less, a, b);
        let gt = self.b.ins().icmp(greater, a, b);
        let eq = self.b.ins().icmp(IntCC::Equal, a, b);
        let so = self.so();
        let value = self.compare_field(lt, gt, eq, so);
        self.set_cr_field(field, value);
    }

    fn update_cr0(&mut self, result: Value) {
        let zero = self.c32(0);
        self.compare(0, result, zero, true);
    }

    fn record(&mut self, i: Instr, result: Value) {
        if i.rc() {
            self.update_cr0(result);
        }
    }

    /// `a + b + carry` → (result, carry out as `i32`).
    fn add_with_carry(&mut self, a: Value, b: Value, carry: Option<Value>) -> (Value, Value) {
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

    fn not(&mut self, value: Value) -> Value {
        self.b.ins().bnot(value)
    }

    fn round_single(&mut self, value: Value) -> Value {
        let single = self.b.ins().fdemote(F32, value);
        self.b.ins().fpromote(F64, single)
    }

    fn cr_bit(&mut self, bit: usize) -> Value {
        let cr = self.cr();
        let shifted = self.b.ins().ushr_imm_s(cr, 31 - bit as i64);
        self.b.ins().band_imm_s(shifted, 1)
    }

    /// Effective address of a D-form access; `update` ignores the `(rA|0)` rule.
    fn ea_d(&mut self, i: Instr, update: bool) -> Value {
        let base = if update { self.gpr(i.ra()) } else { self.ra_or_zero(i.ra()) };
        self.b.ins().iadd_imm_s(base, i64::from(i.simm()))
    }

    fn ea_x(&mut self, i: Instr, update: bool) -> Value {
        let base = if update { self.gpr(i.ra()) } else { self.ra_or_zero(i.ra()) };
        let index = self.gpr(i.rb());
        self.b.ins().iadd(base, index)
    }

    fn integer_load(&mut self, i: Instr, ea: Value, size: u32, signed: bool, update: bool) {
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

    fn integer_store(&mut self, i: Instr, ea: Value, size: u32, update: bool) {
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

    fn load_single(&mut self, i: Instr, ea: Value, update: bool) {
        let raw = self.load32(ea);
        let single = self.b.ins().bitcast(F32, MemFlagsData::new(), raw);
        let value = self.b.ins().fpromote(F64, single);
        self.set_fpr(i.rd(), 0, value);
        self.set_fpr(i.rd(), 1, value);
        if update {
            self.set_gpr(i.ra(), ea);
        }
    }

    fn load_double(&mut self, i: Instr, ea: Value, update: bool) {
        let address = self.host_address(ea);
        let raw = self.b.ins().load(I64, flags(), address, 0);
        let swapped = self.b.ins().bswap(raw);
        let value = self.b.ins().bitcast(F64, MemFlagsData::new(), swapped);
        self.set_fpr(i.rd(), 0, value);
        if update {
            self.set_gpr(i.ra(), ea);
        }
    }

    fn store_single(&mut self, i: Instr, ea: Value, update: bool) {
        let value = self.fpr(i.rd(), 0);
        let single = self.b.ins().fdemote(F32, value);
        let raw = self.b.ins().bitcast(I32, MemFlagsData::new(), single);
        self.store32(ea, raw);
        if update {
            self.set_gpr(i.ra(), ea);
        }
    }

    fn store_double(&mut self, i: Instr, ea: Value, update: bool) {
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
    fn branch_condition(&mut self, i: Instr, use_ctr: bool) -> Option<Value> {
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
    fn branch(&mut self, i: Instr, pc: u32, target: Target, condition: Option<Value>) -> Flow {
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
    fn leave_unless_float(&mut self, pc: u32, gqr: usize, type_bit: i64) {
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
    fn psq_ea(&mut self, i: Instr, op: Opcode) -> (Value, bool) {
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
    fn set_single(&mut self, i: Instr, value: Value) {
        let rounded = self.round_single(value);
        self.set_fpr(i.rd(), 0, rounded);
        self.set_fpr(i.rd(), 1, rounded);
    }

    fn set_paired(&mut self, i: Instr, ps0: Value, ps1: Value) {
        let (ps0, ps1) = (self.round_single(ps0), self.round_single(ps1));
        self.set_fpr(i.rd(), 0, ps0);
        self.set_fpr(i.rd(), 1, ps1);
    }

    /// Arithmetic shared by the double, single and paired forms, on slot `slot`.
    fn arith(&mut self, op: Opcode, i: Instr, slot: usize) -> Option<Value> {
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
    fn scaled(&mut self, i: Instr, scale_slot: usize, add: bool) {
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

    fn sign_op(&mut self, op: Opcode, value: Value) -> Value {
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

    fn float_compare(&mut self, field: usize, a: Value, b: Value) {
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
    fn instruction(&mut self, op: Opcode, i: Instr, pc: u32) -> Flow {
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
                return Flow::Fallback;
            }
            O::Mullw if i.oe() => return Flow::Fallback,
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
                return self.branch(i, pc, Target::Known(target), None);
            }
            O::Bc => {
                let target = if i.aa() { i.bd() as u32 } else { pc.wrapping_add(i.bd() as u32) };
                let condition = self.branch_condition(i, true);
                return self.branch(i, pc, Target::Known(target), condition);
            }
            O::Bclr => {
                let lr = self.lr();
                let target = self.b.ins().band_imm_s(lr, !3_i64);
                let condition = self.branch_condition(i, true);
                return self.branch(i, pc, Target::Computed(target), condition);
            }
            O::Bcctr => {
                let ctr = self.ctr();
                let target = self.b.ins().band_imm_s(ctr, !3_i64);
                let condition = self.branch_condition(i, false);
                return self.branch(i, pc, Target::Computed(target), condition);
            }

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

            _ => return Flow::Fallback,
        }
        Flow::Next
    }

    fn shift_right_algebraic(&mut self, i: Instr, s: Value, amount: Value) {
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

/// Instructions after which the rest of the region must not run natively:
/// the interpreter may have redirected `pc`.
const fn ends_block_after_fallback(op: Opcode) -> bool {
    matches!(
        op,
        Opcode::HleCall | Opcode::Sc | Opcode::Rfi | Opcode::Invalid | Opcode::Tw | Opcode::Twi
    )
}

/// A region to translate, decoded where the guest's memory can be read.
///
/// Like a function of Cemu's recompiler, a region is the code reachable from its entry
/// through branches that stay close by: loops run inside it with the registers in host
/// registers, and only the way back of a loop looks at the budget. Calls leave it, and
/// the address they return to is another entry of the same code.
#[derive(Clone)]
pub(super) struct Job {
    pub(super) pc: u32,
    /// The region's instructions, by address.
    pub(super) instructions: Vec<(u32, Opcode, Instr)>,
    /// Addresses that start a block: entries, branch targets, and what follows a branch.
    leaders: Vec<u32>,
    /// Where code may come in: `pc`, then the addresses calls of the region return to.
    pub(super) entries: Vec<u32>,
    /// The lowest address and the one after the highest instruction.
    range: (u32, u32),
    /// `interpret_one` for the bus in use.
    helper: usize,
    generation: u64,
    /// Worth the time to make it run as fast as it can.
    optimized: bool,
    /// Where the quick translation counts its runs (0: it does not count).
    counter: usize,
}

/// How much code a [`Job`] takes in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Reach {
    /// A straight run up to the first jump, quick to translate.
    Block,
    /// Everything nearby branches reach: loops run inside. For code that runs often.
    Region,
}

impl Job {
    /// The result of a translation that went wrong.
    pub(super) fn failed(&self) -> Translated {
        Translated {
            pc: self.pc,
            entries: self.entries.clone(),
            range: self.range,
            code: None,
            alignment: 1,
            generation: self.generation,
            nanos: 0,
            optimized: self.optimized,
        }
    }

    /// The same region again, to be made as fast as it can run.
    pub(super) fn into_optimized(self) -> Self {
        Self { optimized: true, counter: 0, ..self }
    }

    /// Where the quick translation counts its runs.
    pub(super) fn counter_address(&self) -> *const u32 {
        self.counter as *const u32
    }

    /// Has the quick translation count its runs at `counter`, a `u32` that outlives the code.
    pub(super) fn count_runs_at(&mut self, counter: *mut u32) {
        self.counter = counter as usize;
    }

    /// Whether the region has code in `[start, end)`.
    pub(super) const fn overlaps(&self, start: u32, end: u32) -> bool {
        self.range.0 < end && start < self.range.1
    }

    /// A translation of it is good for the code as of `generation`.
    pub(super) const fn set_generation(&mut self, generation: u64) {
        self.generation = generation;
    }

    /// Leaders not to seal on reaching them: ways in from later code (loops, the entry).
    fn late_sealed(&self, leaders: &HashMap<u32, usize>) -> Vec<bool> {
        let mut late = vec![false; leaders.len()];
        for entry in &self.entries {
            late[leaders[entry]] = true;
        }
        for &(at, op, i) in &self.instructions {
            let target = match op {
                Opcode::B => {
                    if i.aa() {
                        i.li() as u32
                    } else {
                        at.wrapping_add(i.li() as u32)
                    }
                }
                Opcode::Bc => {
                    if i.aa() {
                        i.bd() as u32
                    } else {
                        at.wrapping_add(i.bd() as u32)
                    }
                }
                _ => continue,
            };
            if target <= at
                && !i.lk()
                && let Some(&leader) = leaders.get(&target)
            {
                late[leader] = true;
            }
        }
        late
    }

    /// Instructions of the block starting at `index`.
    fn block_length(&self, index: usize, leaders: &HashMap<u32, usize>) -> i64 {
        let mut end = index + 1;
        while let Some(&(address, ..)) = self.instructions.get(end)
            && address == self.instructions[end - 1].0.wrapping_add(4)
            && !leaders.contains_key(&address)
        {
            end += 1;
        }
        (end - index) as i64
    }
}

/// What became of a [`Job`].
pub(super) struct Translated {
    pub(super) pc: u32,
    pub(super) entries: Vec<u32>,
    pub(super) range: (u32, u32),
    /// Machine code without relocations, `None` if translating failed.
    pub(super) code: Option<Vec<u8>>,
    pub(super) alignment: u64,
    pub(super) generation: u64,
    pub(super) nanos: u64,
    /// Comes from an [optimizing job](Job::into_optimized).
    pub(super) optimized: bool,
}

/// Whether nothing after `op` can run: it always branches away.
const fn ends_block(op: Opcode, i: Instr) -> bool {
    let bo = i.rd();
    match op {
        Opcode::B => true,
        Opcode::Bc | Opcode::Bclr => bo & 0b1_0100 == 0b1_0100,
        Opcode::Bcctr => bo & 0b1_0000 != 0,
        _ => ends_block_after_fallback(op),
    }
}

/// Reads the instructions of the region entered at `pc`.
#[allow(clippy::fn_to_numeric_cast_any, reason = "native code calls it by its address")]
pub(super) fn decode_block<B: Bus>(
    pc: u32,
    bus: &mut B,
    generation: u64,
    reach: Reach,
) -> Result<Job, CpuError> {
    let limit = match reach {
        Reach::Block => MAX_BLOCK_INSTRUCTIONS,
        Reach::Region => MAX_REGION_INSTRUCTIONS,
    };
    // Decode first so an unreadable first instruction reports a clean error.
    let first = bus.fetch(pc).map_err(|_| CpuError::Bus { pc, address: pc })?;
    let near = |address: u32| {
        address.wrapping_sub(pc.wrapping_sub(REGION_REACH)) < 2 * REGION_REACH && address & 3 == 0
    };
    let mut found: BTreeMap<u32, (Opcode, Instr)> = BTreeMap::new();
    let mut leaders = BTreeSet::new();
    let mut entries = vec![pc];
    let mut work = VecDeque::from([pc]);
    while let Some(start) = work.pop_front() {
        let mut at = start;
        let mut leader = true;
        loop {
            if found.contains_key(&at) {
                leaders.insert(at);
                break;
            }
            if found.len() >= limit {
                break;
            }
            let raw = if at == pc {
                first
            } else {
                match bus.fetch(at) {
                    Ok(raw) => raw,
                    Err(_) => break,
                }
            };
            let (op, i) = (decode(raw), Instr(raw));
            found.insert(at, (op, i));
            if leader {
                leaders.insert(at);
            }
            let next = at.wrapping_add(4);
            let branch = matches!(op, Opcode::B | Opcode::Bc | Opcode::Bclr | Opcode::Bcctr);
            let target = match op {
                Opcode::B => {
                    Some(if i.aa() { i.li() as u32 } else { at.wrapping_add(i.li() as u32) })
                }
                Opcode::Bc => {
                    Some(if i.aa() { i.bd() as u32 } else { at.wrapping_add(i.bd() as u32) })
                }
                _ => None,
            };
            if branch && i.lk() {
                // The call leaves the region; where it returns to is another way in.
                if reach == Reach::Region && near(next) {
                    work.push_back(next);
                    entries.push(next);
                }
                break;
            }
            if let Some(target) = target
                && reach == Reach::Region
                && near(target)
            {
                work.push_back(target);
            }
            if ends_block(op, i) {
                break;
            }
            leader = branch;
            at = next;
        }
    }
    let mut seen = HashSet::new();
    entries.retain(|entry| found.contains_key(entry) && seen.insert(*entry));
    let range = (
        found.keys().next().copied().unwrap_or(pc),
        found.keys().next_back().map_or(pc, |last| last.wrapping_add(4)),
    );
    let helper =
        interpret_one::<B> as unsafe extern "C" fn(*mut CpuState, *mut HelperContext) -> u32;
    Ok(Job {
        pc,
        instructions: found.into_iter().map(|(at, (op, i))| (at, op, i)).collect(),
        leaders: leaders.into_iter().collect(),
        entries,
        range,
        helper: helper as usize,
        generation,
        optimized: false,
        counter: 0,
    })
}

/// One translation thread.
pub(super) struct Worker {
    /// Translates quickly.
    isa: OwnedTargetIsa,
    /// Makes faster code.
    optimizing_isa: OwnedTargetIsa,
    context: Context,
    builder_context: FunctionBuilderContext,
    pages: usize,
}

impl Worker {
    pub(super) fn new(isa: OwnedTargetIsa, optimizing_isa: OwnedTargetIsa, pages: usize) -> Self {
        Self {
            isa,
            optimizing_isa,
            context: Context::new(),
            builder_context: FunctionBuilderContext::new(),
            pages,
        }
    }

    pub(super) fn translate(&mut self, job: &Job) -> Translated {
        let timer = std::time::Instant::now();
        let code = self.compile(job);
        self.context.clear();
        let (code, alignment) = code.map_or((None, 1), |(code, alignment)| (Some(code), alignment));
        Translated {
            pc: job.pc,
            entries: job.entries.clone(),
            range: job.range,
            code,
            alignment,
            generation: job.generation,
            nanos: timer.elapsed().as_nanos() as u64,
            optimized: job.optimized,
        }
    }

    fn compile(&mut self, job: &Job) -> Option<(Vec<u8>, u64)> {
        let pointer = self.isa.pointer_type();
        let call_conv = self.isa.default_call_conv();
        self.context.func.signature = block_signature(pointer);
        {
            let mut b = FunctionBuilder::new(&mut self.context.func, &mut self.builder_context);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            b.seal_block(entry);
            let (state, memory, helper_context) =
                (b.block_params(entry)[0], b.block_params(entry)[1], b.block_params(entry)[2]);
            // What the entry loads is only known once the region is translated.
            let prologue = b.create_block();
            b.ins().jump(prologue, &[]);

            let mut signature = Signature::new(call_conv);
            signature.params.extend([AbiParam::new(pointer); 2]);
            signature.returns.push(AbiParam::new(I32));
            let interpret_sig = b.import_signature(signature);
            let block_sig = b.import_signature(block_signature(pointer));
            let registers = std::array::from_fn(|register| {
                let ty = match register_slot(register).1 {
                    I8 => I32,
                    ty => ty,
                };
                b.declare_var(ty)
            });
            let remaining = b.declare_var(I64);
            let leader_of: HashMap<u32, usize> =
                job.leaders.iter().enumerate().map(|(index, &address)| (address, index)).collect();
            let leaders = job.leaders.iter().map(|_| b.create_block()).collect();
            let late = job.late_sealed(&leader_of);

            let mut emit = Emit {
                b: &mut b,
                state,
                memory,
                helper_context,
                interpret: job.helper as i64,
                interpret_sig,
                block_sig,
                pages: self.pages,
                counter: job.counter,
                registers,
                remaining,
                touched: 0,
                dirty: Dirty { leader: 0, local: 0, flushed: false },
                leaders,
                leader_of,
                edges: Vec::new(),
                stubs: Vec::new(),
            };
            let mut open = false;
            for (index, &(address, op, i)) in job.instructions.iter().enumerate() {
                if let Some(&leader) = emit.leader_of.get(&address) {
                    let block = emit.leaders[leader];
                    if open {
                        emit.edges.push((emit.dirty, leader));
                        emit.b.ins().jump(block, &[]);
                    }
                    emit.b.switch_to_block(block);
                    // Every way here is known once it is reached, but for loops and entries.
                    if !late[leader] {
                        emit.b.seal_block(block);
                    }
                    emit.dirty = Dirty { leader, local: 0, flushed: false };
                    emit.charge(job.block_length(index, &emit.leader_of));
                    open = true;
                } else if !open {
                    continue;
                }
                match emit.instruction(op, i, address) {
                    Flow::Next => {}
                    Flow::Ended => {
                        open = false;
                        continue;
                    }
                    Flow::Fallback if ends_block_after_fallback(op) => {
                        let stub = emit.stub(StubKind::Fallback { pc: address, then: Then::Leave });
                        emit.b.ins().jump(stub, &[]);
                        open = false;
                        continue;
                    }
                    Flow::Fallback => {
                        let resume = emit.b.create_block();
                        let then = Then::Resume(resume);
                        let stub = emit.stub(StubKind::Fallback { pc: address, then });
                        emit.b.ins().jump(stub, &[]);
                        emit.b.switch_to_block(resume);
                        emit.dirty.local = 0;
                        emit.dirty.flushed = true;
                    }
                }
                let next = address.wrapping_add(4);
                if job.instructions.get(index + 1).map(|following| following.0) != Some(next) {
                    let exit = emit.stub(StubKind::Exit(Target::Known(next)));
                    emit.b.ins().jump(exit, &[]);
                    open = false;
                }
            }
            emit.finish_stubs();

            emit.b.switch_to_block(prologue);
            emit.b.seal_block(prologue);
            // Three instructions per run tell the hot regions, the ones worth optimizing,
            // from the many that run a few times only.
            emit.count_run();
            emit.reload(emit.touched);
            let offset = offset_of!(HelperContext, remaining) as i32;
            let left = emit.b.ins().load(I64, flags(), helper_context, offset);
            emit.b.def_var(remaining, left);
            let first = emit.leaders[emit.leader_of[&job.pc]];
            if job.entries.len() == 1 {
                emit.b.ins().jump(first, &[]);
            } else {
                let pc = emit.load_state(offset_of!(CpuState, pc) as i32);
                let mut switch = Switch::new();
                for entry in &job.entries[1..] {
                    switch.set_entry(u128::from(*entry), emit.leaders[emit.leader_of[entry]]);
                }
                switch.emit(emit.b, pc, first);
            }
            b.seal_all_blocks();
            b.finalize(self.isa.frontend_config());
        }
        let isa = if job.optimized { &self.optimizing_isa } else { &self.isa };
        let compiled = match self.context.compile(&**isa, &mut ControlPlane::default()) {
            Ok(compiled) => compiled,
            Err(error) => {
                log::error!("JIT failed at {:#010x}: {:?}", job.pc, error.inner);
                return None;
            }
        };
        if !compiled.buffer.relocs().is_empty() {
            log::error!("JIT region at {:#010x} needs relocations", job.pc);
            return None;
        }
        Some((compiled.code_buffer().to_vec(), u64::from(compiled.buffer.min_alignment)))
    }
}

#[cfg(test)]
mod tests {
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
                    got.ps0.to_bits() == want.ps0.to_bits()
                        && got.ps1.to_bits() == want.ps1.to_bits(),
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
                &[
                    1.5_f32.to_bits(),
                    (-2.25_f32).to_bits(),
                    70_000.75_f32.to_bits(),
                    2.5_f32.to_bits(),
                ],
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
}
