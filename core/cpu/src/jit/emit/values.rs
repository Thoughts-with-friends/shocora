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
//! │ decoded            │   │ jit/emit/values.rs       │   │ code to compile    │
//! │ instructions       │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ PowerPC → Cranelift IR:  │   │                    │
//! │                    │   │ values                   │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "values" share of emit.rs (PowerPC → Cranelift IR), moved here so that no file is longer
//! than 500 lines. It works exactly as if it were still written in that file. Turns each
//! PowerPC instruction into the intermediate language of Cranelift, the compiler that produces
//! native code. Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.

use super::*;

impl Emit<'_, '_> {
    pub(super) fn c32(&mut self, value: u32) -> Value {
        self.b.ins().iconst(I32, i64::from(value as i32))
    }

    pub(super) fn load_state(&mut self, offset: i32) -> Value {
        self.b.ins().load(I32, flags(), self.state, offset)
    }

    pub(super) fn store_state(&mut self, offset: i32, value: Value) {
        self.b.ins().store(flags(), value, self.state, offset);
    }

    pub(super) fn read(&mut self, register: usize) -> Value {
        self.touched |= 1 << register;
        self.b.use_var(self.registers[register])
    }

    pub(super) fn write(&mut self, register: usize, value: Value) {
        self.touched |= 1 << register;
        self.dirty.local |= 1 << register;
        self.b.def_var(self.registers[register], value);
    }

    pub(super) fn gpr(&mut self, index: usize) -> Value {
        self.read(GPR + index)
    }

    pub(super) fn set_gpr(&mut self, index: usize, value: Value) {
        self.write(GPR + index, value);
    }

    /// `(rA|0)`.
    pub(super) fn ra_or_zero(&mut self, index: usize) -> Value {
        if index == 0 { self.c32(0) } else { self.gpr(index) }
    }

    pub(super) fn fpr(&mut self, index: usize, slot: usize) -> Value {
        self.read(FPR[slot] + index)
    }

    pub(super) fn set_fpr(&mut self, index: usize, slot: usize, value: Value) {
        self.write(FPR[slot] + index, value);
    }

    pub(super) fn lr(&mut self) -> Value {
        self.read(LR)
    }

    pub(super) fn set_lr(&mut self, value: Value) {
        self.write(LR, value);
    }

    pub(super) fn ctr(&mut self) -> Value {
        self.read(CTR)
    }

    pub(super) fn set_ctr(&mut self, value: Value) {
        self.write(CTR, value);
    }

    pub(super) fn cr(&mut self) -> Value {
        self.read(CR)
    }

    pub(super) fn set_cr(&mut self, value: Value) {
        self.write(CR, value);
    }

    /// XER[CA] as an `i32` 0/1.
    pub(super) fn ca(&mut self) -> Value {
        self.read(CA)
    }

    pub(super) fn set_ca(&mut self, value: Value) {
        self.write(CA, value);
    }

    pub(super) fn so(&mut self) -> Value {
        self.b.ins().uload8(I32, flags(), self.state, offset_of!(CpuState, xer_so) as i32)
    }

    /// Writes the registers of `mask` back to `state`.
    pub(super) fn flush(&mut self, mask: Mask) {
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
    pub(super) fn reload(&mut self, mask: Mask) {
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

    pub(super) fn set_pc(&mut self, pc: Value) {
        self.store_state(offset_of!(CpuState, pc) as i32, pc);
    }

    /// Accounts for the `count` instructions of a leader's block.
    pub(super) fn charge(&mut self, count: i64) {
        let remaining = self.b.use_var(self.remaining);
        let left = self.b.ins().iadd_imm_s(remaining, -count);
        self.b.def_var(self.remaining, left);
    }

    /// Counts a run of the quick translation, when it counts them.
    pub(super) fn count_run(&mut self) {
        if self.counter != 0 {
            let counter = self.b.ins().iconst(I64, self.counter as i64);
            let runs = self.b.ins().load(I32, flags(), counter, 0);
            let runs = self.b.ins().iadd_imm_u(runs, 1);
            self.b.ins().store(flags(), runs, counter, 0);
        }
    }

    /// A block with code to emit after the region.
    pub(super) fn stub(&mut self, kind: StubKind) -> IrBlock {
        let block = self.b.create_block();
        self.stubs.push(Stub { block, at: self.dirty, kind });
        block
    }

    /// Where a branch to `target` goes: straight to a leader of the region, or out.
    pub(super) fn destination(&mut self, target: Target, from: u32, link: bool) -> IrBlock {
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

    /// Returns to [`Jit::run`](super::super::Jit::run) when `condition` holds.
    pub(super) fn leave_if(&mut self, condition: Value, stop: Value) {
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
    pub(super) fn exit(&mut self, pc: Value, known: Option<u32>, mask: Mask) {
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
    pub(super) fn interpret(&mut self, pc: u32, mask: Mask, then: Then) {
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
    pub(super) fn finish_stubs(&mut self) {
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

    pub(super) fn host_address(&mut self, ea: Value) -> Value {
        let offset = self.b.ins().uextend(I64, ea);
        self.b.ins().iadd(self.memory, offset)
    }

    pub(super) fn load32(&mut self, ea: Value) -> Value {
        let address = self.host_address(ea);
        let raw = self.b.ins().load(I32, flags(), address, 0);
        self.b.ins().bswap(raw)
    }

    pub(super) fn store32(&mut self, ea: Value, value: Value) {
        let address = self.host_address(ea);
        let raw = self.b.ins().bswap(value);
        self.b.ins().store(flags(), raw, address, 0);
    }

    /// `crf = (lt, gt, eq, so)` from `i8` booleans.
    pub(super) fn set_cr_field(&mut self, field: usize, value: Value) {
        let shift = 28 - 4 * field as i64;
        let cr = self.cr();
        let cleared = self.b.ins().band_imm_s(cr, !(0xF_i64 << shift));
        let placed = self.b.ins().ishl_imm_s(value, shift);
        let merged = self.b.ins().bor(cleared, placed);
        self.set_cr(merged);
    }

    pub(super) fn compare_field(&mut self, lt: Value, gt: Value, eq: Value, low: Value) -> Value {
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

    pub(super) fn compare(&mut self, field: usize, a: Value, b: Value, signed: bool) {
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

    pub(super) fn update_cr0(&mut self, result: Value) {
        let zero = self.c32(0);
        self.compare(0, result, zero, true);
    }

    pub(super) fn record(&mut self, i: Instr, result: Value) {
        if i.rc() {
            self.update_cr0(result);
        }
    }
}
