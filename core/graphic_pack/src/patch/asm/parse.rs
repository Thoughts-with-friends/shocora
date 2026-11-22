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
//! │ assembly text      │   │ patch/asm/parse.rs       │   │ instruction words  │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ PowerPC assembler: parse │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "parse" share of mod.rs (PowerPC assembler), moved here so that no file is longer than
//! 500 lines. It works exactly as if it were still written in that file. Turns patch lines
//! written as PowerPC assembly into machine code. Terms such as HLE, JIT or shader are
//! explained in `docs/guide/wiiu_term.md`.

use super::*;

/// Splits the operands at commas outside strings, trimmed (Cemu's tokenizer).
pub(super) fn split_operands(text: &str) -> Vec<&str> {
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
