//! Part of [`super`]: see its documentation.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!               ▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ assembly text      │   │ patch/asm/operands.rs    │   │ instruction words  │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ PowerPC assembler:       │   │                    │
//! │                    │   │ operands                 │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "operands" share of mod.rs (PowerPC assembler), moved here so that no file is longer
//! than 500 lines. It works exactly as if it were still written in that file. Turns patch lines
//! written as PowerPC assembly into machine code. Terms such as HLE, JIT or shader are
//! explained in `docs/guide/wiiu_term.md`.

use super::*;

impl<'t> Ctx<'t> {
    pub(super) fn set_alignment(&mut self, alignment: u32) {
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

    pub(super) fn reloc(
        &mut self,
        kind: RelocKind,
        expr: String,
        byte_offset: u32,
        bit_offset: u8,
        bits: u8,
    ) {
        self.out.relocs.push(Reloc { kind, expr, byte_offset, bit_offset, bit_count: bits });
    }

    pub(super) fn operand(&self, index: usize) -> Result<&'t str, String> {
        self.operands.get(index).copied().ok_or_else(|| "Missing operand".to_owned())
    }

    // --- the table-driven operands (`OP_FORM_DYNAMIC`) ---

    pub(super) fn encode(&mut self, op: Operand, index: usize) -> Result<(), String> {
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
    pub(super) fn encode_memloc(&mut self, index: usize) -> Result<(), String> {
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

    pub(super) fn constrain(&mut self, constraint: Constraint) {
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
    pub(super) fn text_index(&self, index: usize) -> Result<Option<usize>, String> {
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

    pub(super) fn register(
        &mut self,
        index: usize,
        bit: u32,
        prefix: &str,
        count: u32,
    ) -> Result<(), String> {
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

    pub(super) fn gpr(&mut self, index: usize, bit: u32) -> Result<(), String> {
        self.register(index, bit, "r", 32)
    }

    pub(super) fn fpr(&mut self, index: usize, bit: u32) -> Result<(), String> {
        self.register(index, bit, "f", 32)
    }

    pub(super) fn cr(&mut self, index: usize, bit: u32, as_bit_index: bool) -> Result<(), String> {
        self.register(index, bit + if as_bit_index { 2 } else { 0 }, "cr", 8)
    }

    /// A condition register bit: `eq`, `4*cr1+gt`.
    pub(super) fn bi(&mut self, index: usize, bit: u32) -> Result<(), String> {
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

    pub(super) fn imm_s16(&mut self, index: usize, bit: u32) -> Result<(), String> {
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

    pub(super) fn imm_u5(&mut self, index: usize, bit: u32) -> Result<(), String> {
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

    pub(super) fn imm_u5_const(&self, index: usize) -> Result<u32, String> {
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

    pub(super) fn branch(&mut self, index: usize, kind: RelocKind) -> Result<(), String> {
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
pub(super) fn parse_register(text: &str, prefix: &str) -> Option<u32> {
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
pub(super) fn constant_int(ep: &Expr<'_>, expr: &str) -> Result<i32, String> {
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
pub(super) fn constant_branch_target(expr: &str) -> Option<i32> {
    let rest = expr.strip_prefix('.')?;
    #[allow(clippy::cast_possible_truncation)]
    Expr::new().eval(&format!("0{rest}")).ok().map(|v| v as i64 as i32)
}

pub(super) fn data_kind(name: &str) -> Option<DataKind> {
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
pub(super) fn unescape(body: &str) -> Result<Vec<u8>, String> {
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
    pub(super) fn data(&mut self, kind: DataKind) -> Result<(), String> {
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

    pub(super) fn align(&mut self, value: u32) {
        self.set_alignment(1);
        let mut address = self.virtual_address;
        while !address.is_multiple_of(value) {
            self.out.data.push(0);
            address = address.wrapping_add(1);
        }
    }
}
