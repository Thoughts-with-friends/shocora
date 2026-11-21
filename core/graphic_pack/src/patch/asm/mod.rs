//! Port of Cemu's PowerPC assembler (`ppcAssembler_assembleSingleInstruction`): one line
//! of a patch file into its bytes, with relocations for what refers to symbols.
//!
//! An instruction is looked up by name in [`table::TABLE`]; its operands are encoded as
//! the entry's form says. Data directives (`.int`, `.float`, `.byte "text"`, ...) and
//! `.align` give data instead. An operand that does not evaluate on its own (a label, a
//! preset variable, `import.coreinit.OSReport`) leaves a [`Reloc`] that the patch applier
//! fills in once every symbol is known.
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

impl<'t> Ctx<'t> {
    fn set_alignment(&mut self, alignment: u32) {
        if self.force_no_alignment {
            self.out.alignment = 1;
            self.out.padding = 0;
            self.out.address = self.virtual_address;
            return;
        }
        self.out.alignment = alignment;
        if alignment == 0 {
            self.out.padding = 0;
            self.out.address = self.virtual_address;
            return;
        }
        let aligned = (self.virtual_address.wrapping_add(alignment - 1)) & !(alignment - 1);
        self.out.padding = aligned.wrapping_sub(self.virtual_address);
        self.out.address = aligned;
    }

    fn reloc(&mut self, kind: RelocKind, expr: String, byte_offset: u32, bit_offset: u8, bits: u8) {
        self.out.relocs.push(Reloc { kind, expr, byte_offset, bit_offset, bit_count: bits });
    }

    fn operand(&self, index: usize) -> Result<&'t str, String> {
        self.operands.get(index).copied().ok_or_else(|| "Missing operand".to_owned())
    }

    // --- the table-driven operands (`OP_FORM_DYNAMIC`) ---

    fn encode(&mut self, op: Operand, index: usize) -> Result<(), String> {
        let put = |opcode: &mut u32, pos: u8, value: u32| {
            *opcode &= !(0x1F << pos);
            *opcode |= value << pos;
        };
        match op {
            Operand::Gpr { pos, ea_zero } => {
                let text = self.operand(index)?;
                if ea_zero && text == "0" {
                    put(&mut self.opcode, pos, 0);
                    return Ok(());
                }
                let reg = reg_index(text, "r").filter(|&r| r < 32).ok_or_else(|| {
                    format!("Operand \"{text}\" is not a valid GPR (expected r0 - r31)")
                })?;
                put(&mut self.opcode, pos, reg);
            }
            Operand::Fpr(pos) => {
                let text = self.operand(index)?;
                let reg = reg_index(text, "f").filter(|&r| r < 32).ok_or_else(|| {
                    format!("Operand \"{text}\" is not a valid FPR (expected f0 - f31)")
                })?;
                put(&mut self.opcode, pos, reg);
            }
            Operand::Cr(pos) => {
                let text = self.operand(index)?;
                let cr = reg_index(text, "cr").filter(|&r| r < 8).ok_or_else(|| {
                    format!("Operand \"{text}\" is not a valid CR field (expected cr0 - cr7)")
                })?;
                put(&mut self.opcode, pos, cr << 2);
            }
            Operand::Spr => {
                let text = self.operand(index)?;
                let spr = reg_index(text, "spr").filter(|&r| r < 1024).ok_or_else(|| {
                    format!("Operand \"{text}\" is not a valid GPR (expected spr0 - spr1023)")
                })?;
                put(&mut self.opcode, 16, spr & 0x1F);
                put(&mut self.opcode, 11, (spr >> 5) & 0x1F);
            }
            Operand::MemLoc => self.encode_memloc(index)?,
            Operand::Imm { pos, bits, signed, negate, extended } => {
                let text = self.operand(index)?;
                let expr = if negate { format!("0-({text})") } else { text.to_owned() };
                let Some(value) = constant(&expr)? else {
                    self.reloc(RelocKind::MaskedImm, expr, 0, pos, bits);
                    return Ok(());
                };
                let imm = to_u32(value);
                let bits32 = u32::from(bits);
                let fits =
                    can_store(imm, bits32, signed) || (extended && can_store(imm, bits32, false));
                if !fits {
                    return Err(format!("Value of operand \"{text}\" is out of range"));
                }
                let mask = (1_u32 << bits) - 1;
                self.opcode &= !(mask << pos);
                self.opcode |= (imm & mask) << pos;
            }
            Operand::U5Reverse { pos, base } => {
                let text = self.operand(index)?;
                let expr = format!("{base}-({text})");
                let Some(value) = constant(&expr)? else {
                    self.reloc(RelocKind::MaskedImm, expr, 0, pos, 5);
                    return Ok(());
                };
                let imm = to_u32(value);
                if !can_store(imm, 5, false) {
                    return Err(format!("Value of operand \"{text}\" is out of range"));
                }
                put(&mut self.opcode, pos, imm & 0x1F);
            }
        }
        Ok(())
    }

    /// `offset(rA)`.
    fn encode_memloc(&mut self, index: usize) -> Result<(), String> {
        let text = self.operand(index)?;
        let bad = || {
            format!(
                "'{text}' does not end with valid memory register syntax. Memory operand must \
                 have the form offset(gpr). Example: 0x20(r3)"
            )
        };
        let trimmed = text.trim_end_matches([' ', '\t']);
        let inner = trimmed.strip_suffix(')').ok_or_else(bad)?;
        let open = inner.rfind('(').ok_or_else(bad)?;
        let (expr, reg) = (&inner[..open], &inner[open + 1..]);
        let gpr = reg_index(reg, "r").filter(|&r| r < 32).ok_or_else(|| {
            format!("Memory operand register \"{reg}\" is not a valid GPR (expected r0 - r31)")
        })?;
        self.opcode &= !(0x1F << 16);
        self.opcode |= gpr << 16;
        match constant(expr)? {
            Some(value) => {
                #[allow(clippy::cast_possible_truncation)]
                let imm = value as i64 as i32;
                if !(-32768..=32767).contains(&imm) {
                    return Err(format!(
                        "\"{expr}\" evaluates to offset out of range (Valid range is -32768 to \
                         32767)"
                    ));
                }
                self.opcode &= !0xFFFF;
                self.opcode |= imm.cast_unsigned() & 0xFFFF;
            }
            None => self.reloc(RelocKind::MaskedImm, expr.to_owned(), 0, 0, 16),
        }
        Ok(())
    }

    fn constrain(&mut self, constraint: Constraint) {
        let put = |opcode: &mut u32, pos: u8, value: u32| {
            *opcode &= !(0x1F << pos);
            *opcode |= value << pos;
        };
        match constraint {
            Constraint::FixedU5 { pos, value } => put(&mut self.opcode, pos, u32::from(value)),
            Constraint::MirrorU5 { src, dst } => {
                let value = (self.opcode >> src) & 0x1F;
                put(&mut self.opcode, dst, value);
            }
            Constraint::MirrorReverseU5 { src, dst, base } => {
                let value = u32::from(base).wrapping_sub((self.opcode >> src) & 0x1F);
                put(&mut self.opcode, dst, value & 0x1F);
            }
            Constraint::FixedSpr(spr) => {
                let spr = u32::from(spr);
                put(&mut self.opcode, 16, spr & 0x1F);
                put(&mut self.opcode, 11, (spr >> 5) & 0x1F);
            }
            Constraint::CheckSignBit => {}
        }
    }

    // --- the older hand-written forms ---

    /// The text index of internal operand `index`; `None` if the entry skips it.
    fn text_index(&self, index: usize) -> Result<Option<usize>, String> {
        let index = op_index(self.def, index);
        if self.def.flags & (1 << index) != 0 {
            return Ok(None);
        }
        let text = (0..index).filter(|i| self.def.flags & (1 << i) == 0).count();
        if text >= self.operands.len() {
            return Err("Missing operand".to_owned());
        }
        Ok(Some(text))
    }

    fn register(&mut self, index: usize, bit: u32, prefix: &str, count: u32) -> Result<(), String> {
        let Some(text) = self.text_index(index)? else { return Ok(()) };
        let text = self.operands[text];
        let reg = parse_register(text, prefix).filter(|&r| r < count).ok_or_else(|| {
            format!(
                "'{text}' is not a valid register operand (must be {prefix}0 to {prefix}{})",
                count - 1
            )
        })?;
        self.opcode |= reg << bit;
        Ok(())
    }

    fn gpr(&mut self, index: usize, bit: u32) -> Result<(), String> {
        self.register(index, bit, "r", 32)
    }

    fn fpr(&mut self, index: usize, bit: u32) -> Result<(), String> {
        self.register(index, bit, "f", 32)
    }

    fn cr(&mut self, index: usize, bit: u32, as_bit_index: bool) -> Result<(), String> {
        self.register(index, bit + if as_bit_index { 2 } else { 0 }, "cr", 8)
    }

    /// A condition register bit: `eq`, `4*cr1+gt`.
    fn bi(&mut self, index: usize, bit: u32) -> Result<(), String> {
        let Some(text) = self.text_index(index)? else { return Ok(()) };
        let expr = self.operands[text];
        let mut ep = Expr::integer();
        for (name, value) in [("lt", 0.0), ("gt", 1.0), ("eq", 2.0), ("so", 3.0)] {
            ep.add_constant(name, value);
        }
        for cr in 0..8_u8 {
            ep.add_constant(&format!("cr{cr}"), f64::from(cr));
        }
        let bi = constant_int(&ep, expr)?;
        if !(0..32).contains(&bi) {
            return Err(format!("CR bit operand '{expr}' evaluated to {bi} which is out of range"));
        }
        self.opcode &= !(0x1F << bit);
        self.opcode |= (bi.cast_unsigned()) << bit;
        Ok(())
    }

    fn imm_s16(&mut self, index: usize, bit: u32) -> Result<(), String> {
        let Some(text) = self.text_index(index)? else { return Ok(()) };
        let expr = self.operands[text].to_owned();
        match constant(&expr)? {
            Some(value) => self.opcode |= (to_u32(value) & 0xFFFF) << bit,
            None => {
                #[allow(clippy::cast_possible_truncation)]
                self.reloc(RelocKind::MaskedImm, expr, 0, bit as u8, 16);
            }
        }
        Ok(())
    }

    fn imm_u5(&mut self, index: usize, bit: u32) -> Result<(), String> {
        let Some(text) = self.text_index(index)? else { return Ok(()) };
        let expr = self.operands[text];
        let value =
            Expr::new().eval(expr).map_err(|_| format!("'{expr}' is not a valid expression"))?;
        #[allow(clippy::cast_possible_truncation)]
        let imm = value as i64 as i32;
        if !(0..32).contains(&imm) {
            return Err(format!("'{expr}' is not in range 0-31"));
        }
        self.opcode |= (imm.cast_unsigned() & 0x1F) << bit;
        Ok(())
    }

    fn imm_u5_const(&self, index: usize) -> Result<u32, String> {
        let Some(text) = self.text_index(index)? else { return Ok(0) };
        let expr = self.operands[text];
        let value = constant_int(&Expr::integer(), expr)?;
        if !(0..32).contains(&value) {
            return Err(format!(
                "Expression ''{expr}' which evaluates to {value} is not in range 0-31"
            ));
        }
        Ok(value.cast_unsigned())
    }

    fn branch(&mut self, index: usize, kind: RelocKind) -> Result<(), String> {
        let Some(text) = self.text_index(index)? else { return Ok(()) };
        let expr = self.operands[text].to_owned();
        if let Some(relative) = constant_branch_target(&expr) {
            let (range, mask) = match kind {
                RelocKind::BranchS16 => (-32768..=32767, 0xFFFC),
                _ => (-33_554_432..=33_554_431, 0x03FF_FFFC),
            };
            if !range.contains(&relative) {
                return Err("Branch target out of range".to_owned());
            }
            if relative & 3 != 0 {
                return Err("Branch target must be aligned to 4".to_owned());
            }
            self.opcode |= relative.cast_unsigned() & mask;
            return Ok(());
        }
        self.reloc(kind, expr, 0, 0, 0);
        Ok(())
    }
}

/// Cemu's `_ppcAssembler_parseRegister`: spaces around, `prefix` then digits, below 32.
fn parse_register(text: &str, prefix: &str) -> Option<u32> {
    let text = text.trim_matches(' ');
    if text.len() < 2 {
        return None;
    }
    let digits = text
        .get(..prefix.len())
        .filter(|h| h.eq_ignore_ascii_case(prefix))
        .map(|_| &text[prefix.len()..])?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u32>().ok().filter(|&r| r < 32)
}

/// Cemu's `_ppcAssembler_evaluateConstantExpression` for an integer parser.
fn constant_int(ep: &Expr<'_>, expr: &str) -> Result<i32, String> {
    match ep.eval(expr) {
        #[allow(clippy::cast_possible_truncation)]
        Ok(value) => Ok(value as i64 as i32),
        Err(ExprError::UnknownConstant { .. } | ExprError::Function(_)) => {
            Err(format!("'{expr}' does not evaluate to a constant expression"))
        }
        Err(_) => Err(format!("'{expr}' is not a valid expression")),
    }
}

/// `.+8`, `.-0x10`: a distance from the instruction itself.
fn constant_branch_target(expr: &str) -> Option<i32> {
    let rest = expr.strip_prefix('.')?;
    #[allow(clippy::cast_possible_truncation)]
    Expr::new().eval(&format!("0{rest}")).ok().map(|v| v as i64 as i32)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DataKind {
    Float,
    Double,
    U32,
    U16,
    U8,
}

fn data_kind(name: &str) -> Option<DataKind> {
    Some(match name {
        ".FLOAT" => DataKind::Float,
        ".DOUBLE" => DataKind::Double,
        ".INT" | ".UINT" | ".PTR" | ".U32" | ".LONG" => DataKind::U32,
        ".WORD" | ".U16" | ".SHORT" => DataKind::U16,
        ".BYTE" | ".STRING" | ".U8" | ".CHAR" => DataKind::U8,
        _ => return None,
    })
}

/// A `"..."` operand of `.byte` with C escapes, as Cemu reads them.
fn unescape(body: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(body.len());
    let mut bytes = body.bytes().peekable();
    while let Some(c) = bytes.next() {
        if c != b'\\' {
            out.push(c);
            continue;
        }
        let Some(c) = bytes.next() else { break };
        match c.to_ascii_lowercase() {
            b'\\' => out.push(b'\\'),
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'"' => out.push(b'"'),
            b'\'' => out.push(b'\''),
            b'x' => {
                let mut value = 0_u32;
                let mut digits = false;
                while let Some(d) = bytes.peek().and_then(|h| char::from(*h).to_digit(16)) {
                    value = (value << 4) | d;
                    digits = true;
                    bytes.next();
                }
                if !digits {
                    return Err("String contains invalid hex escape sequence".to_owned());
                }
                #[allow(clippy::cast_possible_truncation)]
                out.push(value as u8);
                // As Cemu: the rest of the string after a hex escape is dropped.
                break;
            }
            _ => out.push(b'\\'),
        }
    }
    Ok(out)
}

impl Ctx<'_> {
    fn data(&mut self, kind: DataKind) -> Result<(), String> {
        let (size, reloc) = match kind {
            DataKind::Float => (4, RelocKind::Float),
            DataKind::Double => (8, RelocKind::Double),
            DataKind::U32 => (4, RelocKind::U32),
            DataKind::U16 => (2, RelocKind::U16),
            DataKind::U8 => (1, RelocKind::U8),
        };
        self.set_alignment(size);
        let operands = std::mem::take(&mut self.operands);
        for expr in operands {
            if let Some(quoted) = expr.strip_prefix('"') {
                if kind != DataKind::U8 {
                    return Err("Strings constants are only allowed in .byte or .string data \
                                directives"
                        .to_owned());
                }
                let body = quoted.strip_suffix('"').ok_or_else(|| {
                    "String constants must end with a quotation mark. Example: \"text\"".to_owned()
                })?;
                self.out.data.extend(unescape(body)?);
                self.out.data.push(0);
                continue;
            }
            let at = self.out.data.len();
            self.out.data.resize(at + size as usize, 0);
            match constant(expr)? {
                Some(value) => {
                    let slot = &mut self.out.data[at..];
                    #[allow(clippy::cast_possible_truncation)]
                    match kind {
                        DataKind::U32 => slot.copy_from_slice(&to_u32(value).to_be_bytes()),
                        DataKind::U16 => {
                            slot.copy_from_slice(&(to_u32(value) as u16).to_be_bytes());
                        }
                        DataKind::U8 => slot[0] = to_u32(value) as u8,
                        DataKind::Double => slot.copy_from_slice(&value.to_be_bytes()),
                        DataKind::Float => slot.copy_from_slice(&(value as f32).to_be_bytes()),
                    }
                }
                #[allow(clippy::cast_possible_truncation)]
                None => self.reloc(reloc, expr.to_owned(), at as u32, 0, 0),
            }
        }
        Ok(())
    }

    fn align(&mut self, value: u32) {
        self.set_alignment(1);
        let mut address = self.virtual_address;
        while !address.is_multiple_of(value) {
            self.out.data.push(0);
            address = address.wrapping_add(1);
        }
    }
}

/// Splits the operands at commas outside strings, trimmed (Cemu's tokenizer).
fn split_operands(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut operands = Vec::new();
    let mut in_string = false;
    let mut pos = 0;
    while pos < bytes.len() {
        pos += 1;
        let start = pos;
        while pos < bytes.len() {
            let c = bytes[pos];
            if c == b'"' {
                let backslashes =
                    bytes[start..pos].iter().rev().take_while(|&&b| b == b'\\').count();
                if backslashes % 2 == 0 {
                    in_string = !in_string;
                }
            }
            if c == b',' && !in_string {
                break;
            }
            pos += 1;
        }
        let operand = text.get(start..pos).unwrap_or_default().trim_matches([' ', '\t']);
        operands.push(operand);
    }
    operands
}

/// Assembles one line at `address`. With `force_no_alignment` the output goes exactly
/// there (a line with an explicit `address = ` prefix).
///
/// # Errors
/// Says what is wrong with the line, in Cemu's words.
#[allow(clippy::too_many_lines)]
pub fn assemble(text: &str, address: u32, force_no_alignment: bool) -> Result<Assembled, String> {
    let text = text.split('#').next().unwrap_or_default().trim_matches([' ', '\t']);
    let name_end = text.find([' ', '\t']).unwrap_or(text.len());
    let mut name = text[..name_end].to_ascii_uppercase();
    if name.len() > 32 {
        return Err("Instruction name exceeds maximum allowed length".to_owned());
    }
    if name.is_empty() {
        return Err("Instruction name is invalid".to_owned());
    }
    let mut branch_hint = false;
    if name.ends_with('+') {
        branch_hint = true;
        name.pop();
    }
    if name.ends_with('-') {
        branch_hint = true;
        name.pop();
    }
    match name.as_str() {
        "BNL" => name = "BGT".to_owned(),
        "BNG" => name = "BLE".to_owned(),
        _ => {}
    }
    let operands = split_operands(&text[name_end..]);
    let mut ctx = Ctx {
        def: &table::TABLE[0],
        operands,
        opcode: 0,
        out: Assembled::default(),
        force_no_alignment,
        virtual_address: address,
    };

    if let Some(kind) = data_kind(&name) {
        if ctx.operands.is_empty() {
            return Err(format!("Value expected after data directive {name}"));
        }
        ctx.data(kind)?;
        return Ok(ctx.out);
    }
    if name == ".ALIGN" {
        if ctx.operands.len() != 1 {
            return Err(".align directive must have exactly one operand".to_owned());
        }
        let expr = ctx.operands[0];
        let value = constant(expr)?
            .ok_or_else(|| format!("Expression '{expr}' for .align directive is not a constant"))?;
        #[allow(clippy::cast_possible_truncation)]
        let value = value as i64 as i32;
        if !(1..256).contains(&value) {
            return Err(format!(
                "Alignment value '{value}' is not within the allowed range (1-256)"
            ));
        }
        ctx.align(value.cast_unsigned());
        return Ok(ctx.out);
    }

    let def = table::TABLE
        .iter()
        .find(|d| d.name == name)
        .ok_or_else(|| format!("Instruction '{name}' is unknown or not supported"))?;
    ctx.def = def;
    ctx.set_alignment(4);
    let (mask, bits) = opcode_mask(def);
    ctx.opcode = (bits & mask) | (def.bits & def.mask);
    let count = ctx.operands.len();
    match def.form {
        Form::Dynamic => {
            for index in 0..4 {
                match def.operands.get(index) {
                    Some(&op) => ctx.encode(op, index)?,
                    None => {
                        if ctx.operands.get(index).is_some_and(|o| !o.is_empty()) {
                            return Err("Too many operands".to_owned());
                        }
                    }
                }
            }
            for &constraint in def.constraints {
                ctx.constrain(constraint);
            }
        }
        Form::NoOperand => {}
        Form::Op3ACmp => {
            if count == 2 {
                ctx.gpr(0, 16)?;
                ctx.gpr(1, 11)?;
            } else {
                ctx.cr(0, 21, true)?;
                ctx.gpr(1, 16)?;
                ctx.gpr(2, 11)?;
            }
        }
        Form::CmpSimm => {
            if count == 2 {
                ctx.gpr(0, 16)?;
                ctx.imm_s16(1, 0)?;
            } else {
                ctx.cr(0, 21, true)?;
                ctx.gpr(1, 16)?;
                ctx.imm_s16(2, 0)?;
            }
        }
        Form::Op3AImm => {
            ctx.gpr(0, 16)?;
            ctx.gpr(1, 21)?;
            ctx.imm_u5(op_index(def, 2), 11)?;
        }
        Form::XFpCmp => {
            if count == 2 {
                ctx.fpr(0, 16)?;
                ctx.fpr(1, 11)?;
            } else {
                ctx.cr(0, 21, true)?;
                ctx.fpr(1, 16)?;
                ctx.fpr(2, 11)?;
            }
        }
        Form::Rlwinm => {
            ctx.gpr(0, 16)?;
            ctx.gpr(1, 21)?;
            ctx.imm_u5(2, 11)?;
            ctx.imm_u5(3, 6)?;
            ctx.imm_u5(4, 1)?;
        }
        Form::RlwinmExtended => {
            ctx.gpr(0, 16)?;
            ctx.gpr(1, 21)?;
            let (sh, mb, me) = if EXTLWI.contains(&def.name) {
                let (n, b) = (ctx.imm_u5_const(2)?, ctx.imm_u5_const(3)?);
                (b, 0, n.wrapping_sub(1))
            } else if EXTRWI.contains(&def.name) {
                let (n, b) = (ctx.imm_u5_const(2)?, ctx.imm_u5_const(3)?);
                (b + n, 32 - n, 31)
            } else if SLWI.contains(&def.name) {
                let n = ctx.imm_u5_const(2)?;
                (n, 0, 31 - n)
            } else if SRWI.contains(&def.name) {
                let n = ctx.imm_u5_const(2)?;
                (32 - n, n, 31)
            } else if CLRLWI.contains(&def.name) {
                (0, ctx.imm_u5_const(2)?, 31)
            } else if CLRRWI.contains(&def.name) {
                (0, 0, 31 - ctx.imm_u5_const(2)?)
            } else {
                (0, 0, 0)
            };
            // Cemu keeps these in `uint8`.
            ctx.opcode |= (sh & 0xFF) << 11;
            ctx.opcode |= (mb & 0xFF) << 6;
            ctx.opcode |= (me & 0xFF) << 1;
        }
        Form::Rlwnm => {
            ctx.gpr(0, 16)?;
            ctx.gpr(1, 21)?;
            ctx.gpr(2, 11)?;
            ctx.imm_u5(3, 6)?;
            ctx.imm_u5(4, 1)?;
        }
        Form::RlwnmExtended => {
            ctx.gpr(0, 16)?;
            ctx.gpr(1, 21)?;
            ctx.gpr(2, 11)?;
            if ROTLW.contains(&def.name) {
                ctx.opcode |= 31 << 1;
            }
        }
        Form::BranchS16 => {
            if def.name == "BC" {
                let bo = ctx.imm_u5_const(0)?;
                ctx.bi(1, 16)?;
                ctx.branch(2, RelocKind::BranchS16)?;
                ctx.opcode |= bo << 21;
            } else {
                if count == 2 {
                    ctx.cr(0, 16, true)?;
                    ctx.branch(1, RelocKind::BranchS16)?;
                } else {
                    ctx.branch(0, RelocKind::BranchS16)?;
                }
                if branch_hint {
                    ctx.opcode |= 1 << 21;
                }
            }
        }
        Form::BranchS24 => ctx.branch(0, RelocKind::BranchS26)?,
        Form::BranchBcclr => {
            if count >= 1 {
                ctx.cr(0, 18, false)?;
            }
        }
        Form::XlCr => {
            ctx.bi(0, 21)?;
            ctx.bi(1, 16)?;
            ctx.bi(2, 11)?;
        }
    }
    ctx.out.data = ctx.opcode.to_be_bytes().to_vec();
    Ok(ctx.out)
}

#[cfg(test)]
mod tests;
