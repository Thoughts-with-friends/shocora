//! Part of [`super`]: see its documentation.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                                        ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ parsed shader      │   │ shader/emit/operands.rs  │   │ WGSL source text   │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ WGSL generation:         │   │                    │
//! │                    │   │ operands                 │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "operands" share of emit.rs (WGSL generation), moved here so that no file is longer than
//! 500 lines. It works exactly as if it were still written in that file. Writes the translated
//! shader as WGSL text. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;

impl<'a> Translator<'a> {
    pub(super) fn new(
        stage: Stage,
        code: &'a [u8],
        textures: &'a [TextureSignature; TEXTURE_UNITS],
    ) -> Self {
        Self {
            stage,
            code,
            textures,
            body: String::new(),
            max_gpr: 0,
            relative_gpr: false,
            next_temp: 0,
            used_samplers: BTreeSet::new(),
            used_textures: BTreeSet::new(),
            color_exports: 0,
            writes_depth: false,
            param_exports: BTreeSet::new(),
            open_loops: Vec::new(),
            loops_emitted: 0,
            uses_stack: false,
            stream_out: false,
            ubo_vec4s: [0; 16],
        }
    }

    /// Notes that uniform block `block` is read at vec4 `index`, or anywhere (`None`).
    pub(super) fn note_ubo(&mut self, block: usize, index: Option<u32>) {
        let reach = &mut self.ubo_vec4s[block.min(15)];
        *reach = index.map_or(u32::MAX, |index| (*reach).max(index.saturating_add(1)));
    }

    pub(super) fn temp(&mut self) -> String {
        self.next_temp += 1;
        format!("t{}", self.next_temp)
    }

    pub(super) fn line(&mut self, text: &str) {
        self.body.push_str(text);
        self.body.push('\n');
    }

    pub(super) fn note_gpr(&mut self, index: u32) {
        self.max_gpr = self.max_gpr.max(index);
    }

    pub(super) fn gpr_index(&mut self, index: u32, rel: bool, mode: u32) -> String {
        if rel {
            self.relative_gpr = true;
            format!("R[{}u + {}]", index, address_register(mode))
        } else {
            self.note_gpr(index);
            format!("R[{index}]")
        }
    }

    /// Bits of a source operand, with its modifiers applied.
    pub(super) fn source(
        &mut self,
        op: &AluOp,
        operand: usize,
        clause: &AluClause,
        group: &AluGroup,
    ) -> String {
        let src = op.src[operand];
        let chan = CHANNEL[src.chan as usize];
        let raw = match src.sel {
            0..=127 => format!("{}.{chan}", self.gpr_index(src.sel, src.rel, op.index_mode)),
            s @ (128..=191) => {
                let bank = usize::from(s >= sel::KCACHE1);
                let base = if s >= sel::KCACHE1 { sel::KCACHE1 } else { sel::KCACHE0 };
                let index = (s - base) + clause.kcache_addr[bank] * 16;
                let block = clause.kcache_bank[bank];
                let relative = if src.rel {
                    format!(" + {}", address_register(op.index_mode))
                } else {
                    String::new()
                };
                self.note_ubo(block as usize, (!src.rel).then_some(index));
                format!(
                    "arena[(P.{}_ubo[{block}] >> 2u) + ({index}u{relative}) * 4u + {}u]",
                    self.stage.prefix(),
                    src.chan
                )
            }
            sel::ZERO => "0u".to_owned(),
            sel::ONE => "0x3F800000u".to_owned(),
            sel::ONE_INT => "1u".to_owned(),
            sel::MINUS_ONE_INT => "0xFFFFFFFFu".to_owned(),
            sel::HALF => "0x3F000000u".to_owned(),
            sel::LITERAL => format!("{:#010x}u", group.literals[src.chan as usize]),
            sel::PV => format!("PV.{chan}"),
            sel::PS => "PS".to_owned(),
            s if s >= sel::CFILE => {
                let index = s - sel::CFILE;
                let relative = if src.rel {
                    format!(" + {}", address_register(op.index_mode))
                } else {
                    String::new()
                };
                format!(
                    "arena[(P.{}_const >> 2u) + ({index}u{relative}) * 4u + {}u]",
                    self.stage.prefix(),
                    src.chan
                )
            }
            _ => "0u".to_owned(),
        };
        let mut value = raw;
        if src.abs {
            value = format!("({value} & 0x7FFFFFFFu)");
        }
        if src.neg {
            value = format!("({value} ^ 0x80000000u)");
        }
        value
    }

    pub(super) fn alu_clause(&mut self, clause: &AluClause) {
        let groups = parse_alu(self.code, clause);
        self.line("if (exec_on) {");
        self.line("var next_active = true;");
        for group in &groups {
            self.alu_group(group, clause);
        }
        self.line("exec_on = next_active;");
        self.line("}");
    }

    pub(super) fn alu_group(&mut self, group: &AluGroup, clause: &AluClause) {
        let mut pv_writes: Vec<(u32, String)> = Vec::new();
        let mut gpr_writes: Vec<String> = Vec::new();
        let is_reduction = |op: &AluOp| !op.is_op3 && matches!(op.opcode, 0x50..=0x52);

        // Reduction ops compute one result from all four vector units.
        let reduction = if group.ops.iter().any(is_reduction) {
            let parts: Vec<(String, String)> = (0..4)
                .map(|unit| {
                    group.ops.iter().find(|op| op.unit == unit && is_reduction(op)).map_or_else(
                        || ("0u".to_owned(), "0u".to_owned()),
                        |op| (self.source(op, 0, clause, group), self.source(op, 1, clause, group)),
                    )
                })
                .collect();
            let result = self.temp();
            let opcode = group.ops.iter().find(|op| is_reduction(op)).map_or(0, |op| op.opcode);
            let f = |s: &str| format!("bitcast<f32>({s})");
            if opcode == 0x52 {
                let a = parts.iter().map(|p| f(&p.0)).collect::<Vec<_>>().join(",");
                let b = parts.iter().map(|p| f(&p.1)).collect::<Vec<_>>().join(",");
                self.line(&format!(
                    "let {result} = bitcast<vec4<u32>>(redc_cube(vec4<f32>({a}), vec4<f32>({b})));"
                ));
            } else {
                let sum = parts
                    .iter()
                    .map(|p| {
                        if opcode == 0x50 {
                            format!("mul_ni({},{})", f(&p.0), f(&p.1))
                        } else {
                            format!("({} * {})", f(&p.0), f(&p.1))
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" + ");
                let d = self.temp();
                self.line(&format!("let {d} = bitcast<u32>({sum});"));
                self.line(&format!("let {result} = vec4<u32>({d});"));
            }
            Some(result)
        } else {
            None
        };

        for op in &group.ops {
            let value = if is_reduction(op) {
                let chan = CHANNEL[op.unit.min(3) as usize];
                let result = reduction.clone().unwrap_or_default();
                let inner = format!("{result}.{chan}");
                Self::modifiers(op, inner, true)
            } else {
                self.alu_op(op, clause, group)
            };
            let Some(value) = value else { continue };
            let name = self.temp();
            self.line(&format!("let {name} = {value};"));
            pv_writes.push((op.unit, name.clone()));
            if op.write {
                let register = self.gpr_index(op.dest, op.dest_rel, op.index_mode);
                gpr_writes.push(format!("{register}.{} = {name};", CHANNEL[op.dest_elem as usize]));
            }
        }
        for write in gpr_writes {
            self.line(&write);
        }
        for (unit, name) in pv_writes {
            if unit < 4 {
                let line = format!("PV.{} = {name};", CHANNEL[unit as usize]);
                self.line(&line);
            } else {
                let line = format!("PS = {name};");
                self.line(&line);
            }
        }
    }

    /// Output modifiers of float-producing ops.
    pub(super) fn modifiers(op: &AluOp, bits: String, is_float: bool) -> Option<String> {
        if !is_float {
            return Some(bits);
        }
        let mut value = format!("bitcast<f32>({bits})");
        match op.omod {
            1 => value = format!("({value} * 2.0)"),
            2 => value = format!("({value} * 4.0)"),
            3 => value = format!("({value} * 0.5)"),
            _ => {}
        }
        if op.clamp {
            value = format!("clamp({value}, 0.0, 1.0)");
        }
        if op.omod == 0 && !op.clamp {
            return Some(bits);
        }
        Some(format!("bitcast<u32>({value})"))
    }

    pub(super) fn alu_op(
        &mut self,
        op: &AluOp,
        clause: &AluClause,
        group: &AluGroup,
    ) -> Option<String> {
        let a = self.source(op, 0, clause, group);
        let fa = format!("bitcast<f32>({a})");
        let ia = format!("bitcast<i32>({a})");
        if op.is_op3 {
            let b = self.source(op, 1, clause, group);
            let c = self.source(op, 2, clause, group);
            let (fb, fc) = (format!("bitcast<f32>({b})"), format!("bitcast<f32>({c})"));
            let float = |value: String| format!("bitcast<u32>({value})");
            let (bits, is_float) = match op.opcode {
                0x10 => (float(format!("mul_ni({fa},{fb}) + {fc}")), true),
                0x11 => (float(format!("(mul_ni({fa},{fb}) + {fc}) * 2.0")), true),
                0x12 => (float(format!("(mul_ni({fa},{fb}) + {fc}) * 4.0")), true),
                0x13 => (float(format!("(mul_ni({fa},{fb}) + {fc}) * 0.5")), true),
                0x14 => (float(format!("{fa} * {fb} + {fc}")), true),
                0x18 => (format!("select({c}, {b}, {fa} == 0.0)"), true),
                0x19 => (format!("select({c}, {b}, {fa} > 0.0)"), true),
                0x1A => (format!("select({c}, {b}, {fa} >= 0.0)"), true),
                0x1C => (format!("select({c}, {b}, {ia} == 0)"), false),
                0x1D => (format!("select({c}, {b}, {ia} > 0)"), false),
                0x1E => (format!("select({c}, {b}, {ia} >= 0)"), false),
                other => {
                    log::warn!("unhandled ALU OP3 {other:#x}");
                    ("0u".to_owned(), false)
                }
            };
            return Self::modifiers(op, bits, is_float);
        }

        let b = self.source(op, 1, clause, group);
        let fb = format!("bitcast<f32>({b})");
        let ib = format!("bitcast<i32>({b})");
        let float = |value: String| format!("bitcast<u32>({value})");
        let set_float = |cond: String| format!("select(0u, 0x3F800000u, {cond})");
        let set_int = |cond: String| format!("select(0u, 0xFFFFFFFFu, {cond})");
        let (bits, is_float) = match op.opcode {
            0x00 => (float(format!("{fa} + {fb}")), true),
            0x01 => (float(format!("mul_ni({fa},{fb})")), true),
            0x02 => (float(format!("{fa} * {fb}")), true),
            0x03 | 0x05 => (float(format!("max({fa},{fb})")), true),
            0x04 | 0x06 => (float(format!("min({fa},{fb})")), true),
            0x08 => (set_float(format!("{fa} == {fb}")), true),
            0x09 => (set_float(format!("{fa} > {fb}")), true),
            0x0A => (set_float(format!("{fa} >= {fb}")), true),
            0x0B => (set_float(format!("{fa} != {fb}")), true),
            0x0C => (set_int(format!("{fa} == {fb}")), false),
            0x0D => (set_int(format!("{fa} > {fb}")), false),
            0x0E => (set_int(format!("{fa} >= {fb}")), false),
            0x0F => (set_int(format!("{fa} != {fb}")), false),
            0x10 => (float(format!("fract({fa})")), true),
            0x11 => (float(format!("trunc({fa})")), true),
            0x12 => (float(format!("ceil({fa})")), true),
            0x13 => (float(format!("round({fa})")), true),
            0x14 => (float(format!("floor({fa})")), true),
            // The address registers hold -256..=255.
            0x15 | 0x16 => {
                let line = format!(
                    "AR.{} = i32(clamp(floor({fa}), -256.0, 255.0));",
                    CHANNEL[op.dest_elem as usize]
                );
                self.line(&line);
                (float(format!("clamp(floor({fa}), -256.0, 255.0)")), true)
            }
            0x18 => {
                let line =
                    format!("AR.{} = clamp({ia}, -256, 255);", CHANNEL[op.dest_elem as usize]);
                self.line(&line);
                (format!("bitcast<u32>(clamp({ia}, -256, 255))"), false)
            }
            0x19 => return Self::modifiers(op, a, op.omod != 0 || op.clamp),
            0x1A => return None,
            0x20..=0x23 | 0x42..=0x45 => {
                let cond = match op.opcode {
                    0x20 => format!("{fa} == {fb}"),
                    0x21 => format!("{fa} > {fb}"),
                    0x22 => format!("{fa} >= {fb}"),
                    0x23 => format!("{fa} != {fb}"),
                    0x42 => format!("{ia} == {ib}"),
                    0x43 => format!("{ia} > {ib}"),
                    0x44 => format!("{ia} >= {ib}"),
                    _ => format!("{ia} != {ib}"),
                };
                if op.update_exec {
                    let line = format!("next_active = {cond};");
                    self.line(&line);
                }
                (set_float(cond), true)
            }
            0x2C..=0x2F | 0x46..=0x49 => {
                let cond = match op.opcode {
                    0x2C => format!("{fa} == {fb}"),
                    0x2D => format!("{fa} > {fb}"),
                    0x2E => format!("{fa} >= {fb}"),
                    0x2F => format!("{fa} != {fb}"),
                    0x46 => format!("{ia} == {ib}"),
                    0x47 => format!("{ia} > {ib}"),
                    0x48 => format!("{ia} >= {ib}"),
                    _ => format!("{ia} != {ib}"),
                };
                if self.stage == Stage::Pixel {
                    let line = format!("if ({cond}) {{ discard; }}");
                    self.line(&line);
                }
                ("0u".to_owned(), false)
            }
            0x30 => (format!("({a} & {b})"), false),
            0x31 => (format!("({a} | {b})"), false),
            0x32 => (format!("({a} ^ {b})"), false),
            0x33 => (format!("(~{a})"), false),
            0x34 => (format!("({a} + {b})"), false),
            0x35 => (format!("({a} - {b})"), false),
            0x36 => (format!("bitcast<u32>(max({ia},{ib}))"), false),
            0x37 => (format!("bitcast<u32>(min({ia},{ib}))"), false),
            0x38 => (format!("max({a},{b})"), false),
            0x39 => (format!("min({a},{b})"), false),
            0x3A => (set_int(format!("{a} == {b}")), false),
            0x3B => (set_int(format!("{ia} > {ib}")), false),
            0x3C => (set_int(format!("{ia} >= {ib}")), false),
            0x3D => (set_int(format!("{a} != {b}")), false),
            0x3E => (set_int(format!("{a} > {b}")), false),
            0x3F => (set_int(format!("{a} >= {b}")), false),
            0x61 => (float(format!("exp2({fa})")), true),
            0x62 => (float(format!("log_clamped({fa})")), true),
            0x63 => (float(format!("log2(max({fa}, 0.0))")), true),
            0x64 => (float(format!("recip_clamped({fa})")), true),
            0x65 => (float(format!("recip_ff({fa})")), true),
            0x66 => (float(format!("1.0 / {fa}")), true),
            0x67 => (float(format!("recipsqrt_clamped({fa})")), true),
            0x68 => (float(format!("recipsqrt_ff({fa})")), true),
            0x69 => (float(format!("1.0 / sqrt({fa})")), true),
            0x6A => (float(format!("sqrt({fa})")), true),
            0x6B => (format!("bitcast<u32>(i32({fa}))"), false),
            0x6C => (float(format!("f32({ia})")), true),
            0x6D => (float(format!("f32({a})")), true),
            // The angle comes in full turns.
            0x6E => (float(format!("sin({fa} / 0.1591549367)")), true),
            0x6F => (float(format!("cos({fa} / 0.1591549367)")), true),
            0x70 => (format!("bitcast<u32>({ia} >> ({b} & 31u))"), false),
            0x71 => (format!("({a} >> ({b} & 31u))"), false),
            0x72 => (format!("({a} << ({b} & 31u))"), false),
            0x73 => (format!("bitcast<u32>({ia} * {ib})"), false),
            0x75 => (format!("({a} * {b})"), false),
            0x79 => (format!("u32({fa})"), false),
            other => {
                log::warn!("unhandled ALU OP2 {other:#x}");
                ("0u".to_owned(), false)
            }
        };
        Self::modifiers(op, bits, is_float)
    }

    pub(super) fn tex_clause(&mut self, addr: u32, count: u32) {
        self.line("if (exec_on) {");
        for op in parse_tex(self.code, addr, count) {
            self.tex_op(&op);
        }
        self.line("}");
    }

    pub(super) fn coord(&mut self, op: &TexOp, component: usize) -> String {
        self.note_gpr(op.src);
        match op.src_sel[component] {
            s @ 0..=3 => format!("bitcast<f32>(R[{}].{})", op.src, CHANNEL[s as usize]),
            4 => "0.0".to_owned(),
            _ => "1.0".to_owned(),
        }
    }
}
