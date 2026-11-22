//! Port of Cemu's PowerPC assembler (`ppcAssembler_assembleSingleInstruction`): one line
//! of a patch file into its bytes, with relocations for what refers to symbols.
//!
//! An instruction is looked up by name in [`table::TABLE`]; its operands are encoded as
//! the entry's form says. Data directives (`.int`, `.float`, `.byte "text"`, ...) and
//! `.align` give data instead. An operand that does not evaluate on its own (a label, a
//! preset variable, `import.coreinit.OSReport`) leaves a [`Reloc`] that the patch applier
//! fills in once every symbol is known.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!               ▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ assembly text      │   │ patch/asm/mod.rs         │   │ instruction words  │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ PowerPC assembler        │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Turns patch lines written as PowerPC assembly into machine code. Terms such as HLE, JIT or
//! shader are explained in `docs/guide/wiiu_term.md`.
mod table;

use crate::expr::{Expr, ExprError};

/// How a relocation is written into the assembled bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocKind {
    /// `bit_count` bits at `bit_offset` of the instruction word.
    MaskedImm,
    /// 14-bit word offset of a conditional branch.
    BranchS16,
    /// 24-bit word offset (or absolute target with `AA`) of `b`.
    BranchS26,
    Float,
    Double,
    U32,
    U16,
    U8,
}

/// A value of the assembled bytes that waits for its expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reloc {
    pub kind: RelocKind,
    pub expr: String,
    pub byte_offset: u32,
    pub bit_offset: u8,
    pub bit_count: u8,
}

/// One assembled line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Assembled {
    pub data: Vec<u8>,
    pub relocs: Vec<Reloc>,
    /// Alignment the output needs (4 for instructions, the element size for data).
    pub alignment: u32,
    /// Bytes skipped before the output to reach the alignment.
    pub padding: u32,
    /// Address of the output: the requested one rounded up to the alignment.
    pub address: u32,
}

#[derive(Debug, Clone, Copy)]
enum Form {
    XlCr,
    Op3ACmp,
    Op3AImm,
    BranchS16,
    BranchS24,
    BranchBcclr,
    Rlwinm,
    RlwinmExtended,
    Rlwnm,
    RlwnmExtended,
    CmpSimm,
    NoOperand,
    XFpCmp,
    Dynamic,
}

#[derive(Debug, Clone, Copy)]
enum Operand {
    Gpr { pos: u8, ea_zero: bool },
    Fpr(u8),
    Cr(u8),
    Spr,
    MemLoc,
    Imm { pos: u8, bits: u8, signed: bool, negate: bool, extended: bool },
    U5Reverse { pos: u8, base: u8 },
}

#[derive(Debug, Clone, Copy)]
enum Constraint {
    FixedU5 {
        pos: u8,
        value: u8,
    },
    MirrorU5 {
        src: u8,
        dst: u8,
    },
    #[allow(dead_code)]
    MirrorReverseU5 {
        src: u8,
        dst: u8,
        base: u8,
    },
    FixedSpr(u16),
    /// Only checked when disassembling.
    CheckSignBit,
}

struct InstrDef {
    name: &'static str,
    /// Primary opcode and the sub-opcodes of its group (`0xFFFF`: none).
    opc: [u16; 3],
    form: Form,
    /// `1 << n`: operand `n` is not written; `1 << 6/7/8`: operands swapped.
    flags: u16,
    mask: u32,
    bits: u32,
    operands: &'static [Operand],
    constraints: &'static [Constraint],
}

const OPC_EXTENDED_BIT: u16 = 0x8000;
const FLG_SWAP_OP0_OP1: u16 = 1 << 6;
const FLG_SWAP_OP1_OP2: u16 = 1 << 7;
const FLG_SWAP_OP2_OP3: u16 = 1 << 8;

const EXTLWI: [&str; 2] = ["EXTLWI", "EXTLWI."];
const EXTRWI: [&str; 2] = ["EXTRWI", "EXTRWI."];
const SLWI: [&str; 2] = ["SLWI", "SLWI."];
const SRWI: [&str; 2] = ["SRWI", "SRWI."];
const CLRLWI: [&str; 2] = ["CLRLWI", "CLRLWI."];
const CLRRWI: [&str; 2] = ["CLRRWI", "CLRRWI."];
const ROTLW: [&str; 2] = ["ROTLW", "ROTLW."];

/// The fixed bits of `def`: primary opcode and group sub-opcodes.
fn opcode_mask(def: &InstrDef) -> (u32, u32) {
    let [opc0, opc1, opc2] = def.opc.map(u32::from);
    let mut mask = 0x3F << 26;
    let mut bits = opc0 << 26;
    let mut set = |index: u32, count: u32, value: u32| {
        mask |= ((1 << count) - 1) << (31 - index);
        bits |= value << (31 - index);
    };
    match opc0 {
        31 | 19 => set(30, 10, opc1),
        4 => {
            set(30, 5, opc1);
            if opc1 == 16 {
                set(25, 5, opc2);
            }
        }
        59 => set(30, 5, opc1),
        63 if def.opc[1] & OPC_EXTENDED_BIT != 0 => {
            set(30, 10, u32::from(def.opc[1] & !OPC_EXTENDED_BIT));
        }
        63 => set(30, 5, opc1),
        _ => {}
    }
    (mask, bits)
}

/// The text position of internal operand `index` after the entry's swaps.
const fn op_index(def: &InstrDef, mut index: usize) -> usize {
    if (index == 0 || index == 1) && def.flags & FLG_SWAP_OP0_OP1 != 0 {
        index ^= 1;
    }
    if def.flags & FLG_SWAP_OP1_OP2 != 0 {
        if index == 1 {
            index = 2;
        } else if index == 2 {
            index = 1;
        }
    }
    if def.flags & FLG_SWAP_OP2_OP3 != 0 {
        if index == 2 {
            index = 3;
        } else if index == 3 {
            index = 2;
        }
    }
    index
}

/// Whether `value` survives being cut to `bits` bits and extended back.
const fn can_store(value: u32, bits: u32, signed: bool) -> bool {
    let shift = 32 - bits;
    if signed {
        ((value.cast_signed() << shift) >> shift).cast_unsigned() == value
    } else {
        (value << shift) >> shift == value
    }
}

/// Cemu's `(uint32)(sint32)double`, wrapping instead of saturating.
#[allow(clippy::cast_possible_truncation)]
pub(crate) const fn to_u32(value: f64) -> u32 {
    value as i64 as u32
}

/// `Some(value)` if `expr` evaluates without symbols, `None` if it needs some
/// (Cemu's `IsConstantExpression`).
fn constant(expr: &str) -> Result<Option<f64>, String> {
    match Expr::new().eval(expr) {
        Ok(value) => Ok(Some(value)),
        Err(ExprError::UnknownConstant { .. } | ExprError::Function(_)) => Ok(None),
        Err(error) => Err(format!("\"{expr}\" could not be evaluated. Error: {error}")),
    }
}

/// `prefix` and a decimal number, case-insensitive (Cemu's `_parseRegIndex`).
fn reg_index(text: &str, prefix: &str) -> Option<u32> {
    let head = text.get(..prefix.len())?;
    if !head.eq_ignore_ascii_case(prefix) {
        return None;
    }
    let digits = &text[prefix.len()..];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

struct Ctx<'t> {
    def: &'static InstrDef,
    operands: Vec<&'t str>,
    opcode: u32,
    out: Assembled,
    force_no_alignment: bool,
    virtual_address: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DataKind {
    Float,
    Double,
    U32,
    U16,
    U8,
}

#[cfg(test)]
mod tests;

mod operands;
mod parse;
#[allow(unused_imports)]
pub use operands::*;
#[allow(unused_imports)]
pub use parse::*;
