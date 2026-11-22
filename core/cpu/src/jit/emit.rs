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
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                         ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ decoded            │   │ jit/emit.rs              │   │ code to compile    │
//! │ instructions       │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ PowerPC → Cranelift IR   │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Turns each PowerPC instruction into the intermediate language of Cranelift, the compiler
//! that produces native code. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.
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

#[cfg(test)]
#[path = "emit_tests.rs"]
mod tests;

mod arith_part;
mod blocks;
mod integer;
mod memory_float;
mod values;
#[allow(unused_imports)]
pub(crate) use arith_part::*;
#[allow(unused_imports)]
pub(crate) use blocks::*;
#[allow(unused_imports)]
pub(crate) use integer::*;
#[allow(unused_imports)]
pub(crate) use memory_float::*;
#[allow(unused_imports)]
pub(crate) use values::*;
