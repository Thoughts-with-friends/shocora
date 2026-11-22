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
//! │ parsed shader      │   │ shader/emit/control.rs   │   │ WGSL source text   │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ WGSL generation: control │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "control" share of emit.rs (WGSL generation), moved here so that no file is longer than
//! 500 lines. It works exactly as if it were still written in that file. Writes the translated
//! shader as WGSL text. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;

impl<'a> Translator<'a> {
    pub(super) fn control_flow(&mut self, list: &[Cf], export: &mut dyn FnMut(&mut Self, &Export)) {
        for cf in list {
            match cf {
                Cf::Tex { addr, count } => self.tex_clause(*addr, *count),
                Cf::Alu(clause) => {
                    if clause.kind == AluKind::PushBefore {
                        self.uses_stack = true;
                        self.line("stack[sp] = exec_on; sp += 1;");
                    }
                    self.alu_clause(clause);
                    match clause.kind {
                        AluKind::PopAfter => {
                            self.uses_stack = true;
                            self.pop(1);
                        }
                        AluKind::Pop2After => {
                            self.uses_stack = true;
                            self.pop(2);
                        }
                        AluKind::ElseAfter => {
                            self.uses_stack = true;
                            self.line("exec_on = !exec_on && stack[sp - 1];");
                        }
                        AluKind::Break => {
                            if !self.open_loops.is_empty() {
                                self.line("if (!exec_on) { break; }");
                            }
                        }
                        AluKind::Plain | AluKind::PushBefore | AluKind::Continue => {}
                    }
                }
                Cf::LoopStart => {
                    self.uses_stack = true;
                    let id = self.loops_emitted;
                    self.loops_emitted += 1;
                    self.line(&format!("stack[sp] = exec_on; sp += 1; let loop_sp{id} = sp; var loop_guard{id} = 0u;"));
                    self.line("loop {");
                    self.line(&format!(
                        "loop_guard{id} += 1u; if (loop_guard{id} > 4096u) {{ break; }}"
                    ));
                    self.line("if (!exec_on) { break; }");
                    self.open_loops.push(id);
                }
                Cf::LoopEnd => {
                    if let Some(id) = self.open_loops.pop() {
                        self.line("if (!exec_on) { break; }");
                        self.line("}");
                        self.line(&format!("sp = loop_sp{id}; sp -= 1; exec_on = stack[sp];"));
                    }
                }
                Cf::LoopBreak => {
                    if !self.open_loops.is_empty() {
                        self.line("if (exec_on) { break; }");
                    }
                }
                // The pop count of ELSE and JUMP applies when the jump is taken. Skipped
                // clauses run here with an inactive mask and reach the regular pop anyway.
                Cf::Else { .. } => {
                    self.uses_stack = true;
                    self.line("exec_on = !exec_on && stack[sp - 1];");
                }
                Cf::Jump { .. } => {}
                Cf::Pop { pop } => {
                    if *pop > 0 {
                        self.uses_stack = true;
                        self.pop(*pop);
                    }
                }
                Cf::Export(e) => {
                    self.line("if (exec_on) {");
                    export(self, e);
                    self.line("}");
                }
                Cf::StreamWrite(write) => {
                    if self.stage == Stage::Vertex {
                        self.stream_write(write);
                    }
                }
                Cf::Unsupported(kind) => log::warn!("unsupported CF instruction {kind:#x}"),
            }
        }
    }

    /// Stores components of a register in the host's copy of the stream-out buffer.
    pub(super) fn stream_write(&mut self, write: &StreamWrite) {
        self.stream_out = true;
        self.note_gpr(write.gpr);
        let buffer = write.buffer;
        self.line(&format!("if (exec_on && P.so_stride[{buffer}] != 0u) {{"));
        self.line(&format!(
            "let so_at = P.so_base[{buffer}] + (vertex_index + P.so_instance_vertices * instance_index) * P.so_stride[{buffer}] + {}u;",
            write.array_base
        ));
        for component in 0..write.array_size.min(4) {
            if write.component_mask & (1 << component) != 0 {
                let line = format!(
                    "so[so_at + {component}u] = R[{}].{};",
                    write.gpr, CHANNEL[component as usize]
                );
                self.line(&line);
            }
        }
        self.line("}");
    }

    pub(super) fn pop(&mut self, count: u32) {
        if count > 0 {
            self.line(&format!("sp -= {count}; exec_on = stack[sp];"));
        }
    }

    pub(super) fn register_declarations(&self) -> String {
        let count = if self.relative_gpr { 128 } else { self.max_gpr + 1 };
        let mut text = format!(
            "var<private> R: array<vec4<u32>, {count}>;\n\
             var<private> PV: vec4<u32>;\n\
             var<private> PS: u32;\n\
             var<private> AR: vec4<i32>;\n\
             var<private> GRAD_H: vec3<f32>;\n\
             var<private> GRAD_V: vec3<f32>;\n\
             var<private> exec_on: bool = true;\n"
        );
        if self.uses_stack {
            text.push_str("var<private> stack: array<bool, 32>;\nvar<private> sp: u32 = 0u;\n");
        }
        text
    }

    /// Entry point of `RECTS` draws, standing in for the geometry stage the host
    /// lacks. A rect is three corners; the fourth completes the parallelogram over
    /// whichever edge is the diagonal, as Cemu's generated geometry shader does.
    pub(super) fn rect_entry_point(&self) -> String {
        let count = if self.relative_gpr { 128 } else { self.max_gpr + 1 };
        let mut reset = format!(
            "R = array<vec4<u32>, {count}>();\nPV = vec4<u32>();\nPS = 0u;\nAR = vec4<i32>();\nexec_on = true;\n"
        );
        if self.uses_stack {
            reset.push_str("sp = 0u;\n");
        }
        let mut fourth = String::from("v.position = x.position + y.position - z.position;\n");
        for param in &self.param_exports {
            let _ = writeln!(fourth, "v.p{param} = x.p{param} + y.p{param} - z.p{param};");
        }
        format!(
            "fn rect_corner(vertex_index: u32, instance_index: u32) -> VsOut {{\n{reset}return vs_body(vertex_index, instance_index);\n}}\n\
             @vertex\nfn {RECT_ENTRY_POINT}(@builtin(vertex_index) vertex_index: u32, @builtin(instance_index) instance_index: u32) -> VsOut {{\n\
             if ((vertex_index & {RECT_CORNER_FLAG}u) == 0u) {{ return rect_corner(vertex_index, instance_index); }}\n\
             let word = vertex_index & {RECT_WORD_MASK}u;\n\
             let a = rect_corner(arena[word], instance_index);\n\
             let b = rect_corner(arena[word + 1u], instance_index);\n\
             let c = rect_corner(arena[word + 2u], instance_index);\n\
             let ab = distance(a.position.xy, b.position.xy);\n\
             let ac = distance(a.position.xy, c.position.xy);\n\
             let bc = distance(b.position.xy, c.position.xy);\n\
             var x = b; var y = c; var z = a;\n\
             if (ab > ac && ab > bc) {{ x = a; y = b; z = c; }} else if (ac > ab && ac > bc) {{ x = c; y = a; z = b; }}\n\
             let corner = (vertex_index >> {RECT_CORNER_SHIFT}u) & 3u;\n\
             if (corner == 0u) {{ return y; }}\n\
             if (corner == 1u) {{ return x; }}\n\
             var v = y;\n{fourth}return v;\n}}\n"
        )
    }

    /// Texture and sampler declarations in `group(1)`; bindings follow the stage and unit.
    pub(super) fn resource_declarations(&self) -> String {
        let mut text = String::new();
        if self.stream_out {
            text.push_str("@group(2) @binding(0) var<storage, read_write> so: array<u32>;\n");
        }
        let stage_base = if self.stage == Stage::Vertex { 0 } else { TEXTURE_UNITS };
        for &unit in &self.used_textures {
            let signature = self.textures[usize::from(unit)];
            let ty = texture_type(signature);
            let binding = (stage_base + usize::from(unit)) * 2;
            let _ = writeln!(
                text,
                "@group(1) @binding({binding}) var {}_tex{unit}: {ty};",
                self.stage.prefix()
            );
        }
        let mut seen = BTreeSet::new();
        for &(unit, _) in &self.used_samplers {
            if !seen.insert(unit) {
                continue;
            }
            let signature = self.textures[usize::from(unit)];
            let binding = (stage_base + usize::from(unit)) * 2 + 1;
            let ty = if signature.comparison { "sampler_comparison" } else { "sampler" };
            let _ = writeln!(
                text,
                "@group(1) @binding({binding}) var {}_smp{unit}: {ty};",
                self.stage.prefix()
            );
        }
        text
    }

    pub(super) fn finish(self, wgsl: String) -> TranslatedShader {
        TranslatedShader {
            wgsl,
            samplers: self.used_samplers.into_iter().collect(),
            textures: self.used_textures.into_iter().collect(),
            color_exports: self.color_exports,
            writes_depth: self.writes_depth,
            stream_out: self.stream_out,
            ubo_vec4s: self.ubo_vec4s,
        }
    }
}

/// `reference OP value` for `DEPTH_COMPARE_FUNCTION` `function`.
pub(super) fn compare_expression(function: u8, reference: &str, value: &str) -> String {
    let op = match function {
        1 => "<",
        2 => "==",
        3 => "<=",
        4 => ">",
        5 => "!=",
        6 => ">=",
        7 => return "true".to_owned(),
        _ => return "false".to_owned(),
    };
    format!("({reference} {op} {value})")
}

pub(super) fn address_register(mode: u32) -> String {
    match mode {
        0..=3 => format!("u32(AR.{})", CHANNEL[mode as usize]),
        _ => "0u".to_owned(),
    }
}

pub(super) fn texture_type(signature: TextureSignature) -> String {
    let element = match signature.kind {
        TextureKind::Float | TextureKind::Depth => "f32",
        TextureKind::Uint => "u32",
        TextureKind::Sint => "i32",
    };
    if signature.kind == TextureKind::Depth {
        return match signature.dim {
            TextureDim::D2 | TextureDim::D3 => "texture_depth_2d".to_owned(),
            TextureDim::D2Array => "texture_depth_2d_array".to_owned(),
            TextureDim::Cube => "texture_depth_cube".to_owned(),
        };
    }
    match signature.dim {
        TextureDim::D2 => format!("texture_2d<{element}>"),
        TextureDim::D2Array => format!("texture_2d_array<{element}>"),
        TextureDim::D3 => format!("texture_3d<{element}>"),
        TextureDim::Cube => format!("texture_cube<{element}>"),
    }
}

// ---- Vertex fetch -----------------------------------------------------------

pub(super) fn attribute_fetch(body: &mut String, attribute: &Attribute, gpr: u32) {
    let buffer = attribute.buffer;
    let index = match attribute.rate {
        0 => "vertex_index".to_owned(),
        1 => "(instance_index + P.base_instance) / max(P.divisor0, 1u)".to_owned(),
        2 => "(instance_index + P.base_instance) / max(P.divisor1, 1u)".to_owned(),
        _ => "(instance_index + P.base_instance)".to_owned(),
    };
    let _ = writeln!(
        body,
        "{{ let a = P.attr_base[{buffer}] + P.attr_stride[{buffer}] * ({index}) + {}u;",
        attribute.offset
    );

    let read16 = if attribute.endian == 1 || attribute.endian == 2 { "rd_be16" } else { "rd_le16" };
    let read32 = if attribute.endian == 0 { "rd_le32" } else { "rd_be32" };
    let normalized = attribute.num_format == 0;
    let scaled = attribute.num_format == 2;
    let int_channel = |value: String, bits: u32| -> String {
        if normalized {
            if attribute.signed {
                let max = f64::from((1_u32 << (bits - 1)) - 1);
                format!("bitcast<u32>(max(f32(sext({value},{bits}u)) / {max:.1}, -1.0))")
            } else {
                let max = f64::from(((1_u64 << bits) - 1) as u32);
                format!("bitcast<u32>(f32({value}) / {max:.1})")
            }
        } else if scaled {
            if attribute.signed {
                format!("bitcast<u32>(f32(sext({value},{bits}u)))")
            } else {
                format!("bitcast<u32>(f32({value}))")
            }
        } else if attribute.signed {
            format!("bitcast<u32>(sext({value},{bits}u))")
        } else {
            value
        }
    };
    let words32 = |count: u32, as_int: bool| -> Vec<String> {
        (0..count)
            .map(|i| {
                let value = format!("{read32}(a + {}u)", i * 4);
                if as_int && !normalized && !scaled {
                    value
                } else if as_int {
                    int_channel(value, 32)
                } else {
                    value
                }
            })
            .collect()
    };
    let bytes = |count: u32| -> Vec<String> {
        (0..count)
            .map(|i| {
                // With a 32-bit swap the bytes arrive reversed.
                let position = if attribute.endian == 2 { count - 1 - i } else { i };
                int_channel(format!("rd8(a + {position}u)"), 8)
            })
            .collect()
    };
    let halves = |count: u32| -> Vec<String> {
        (0..count).map(|i| int_channel(format!("{read16}(a + {}u)", i * 2), 16)).collect()
    };
    let half_floats = |count: u32| -> Vec<String> {
        (0..count).map(|i| format!("bitcast<u32>(f16_bits({read16}(a + {}u)))", i * 2)).collect()
    };

    let mut channels: Vec<String> = match attribute.format {
        format::F8 => bytes(1),
        format::F8_8 => bytes(2),
        format::F8_8_8_8 => bytes(4),
        format::F16 => halves(1),
        format::F16_16 => halves(2),
        format::F16_16_16_16 => halves(4),
        format::F16_FLOAT => half_floats(1),
        format::F16_16_FLOAT => half_floats(2),
        format::F16_16_16_16_FLOAT => half_floats(4),
        format::F32_FLOAT => words32(1, false),
        format::F32_32_FLOAT => words32(2, false),
        format::F32_32_32_FLOAT => words32(3, false),
        format::F32_32_32_32_FLOAT => words32(4, false),
        format::F32 => words32(1, true),
        format::F32_32 => words32(2, true),
        format::F32_32_32 => words32(3, true),
        format::F32_32_32_32 => words32(4, true),
        format::F2_10_10_10 => {
            let _ = writeln!(body, "let raw = {read32}(a);");
            vec![
                int_channel("(raw & 0x3FFu)".to_owned(), 10),
                int_channel("((raw >> 10u) & 0x3FFu)".to_owned(), 10),
                int_channel("((raw >> 20u) & 0x3FFu)".to_owned(), 10),
                if normalized {
                    "bitcast<u32>(f32(raw >> 30u) / 3.0)".to_owned()
                } else {
                    "(raw >> 30u)".to_owned()
                },
            ]
        }
        format::F10_11_11_FLOAT => {
            log::warn!("attribute format 10_11_11 float is decoded as zero");
            vec!["0u".to_owned(); 3]
        }
        other => {
            log::warn!("unhandled attribute format {other:#x}");
            vec!["0u".to_owned(); 4]
        }
    };
    channels.resize(4, "0u".to_owned());
    let is_int = attribute.num_format == 1;
    let one = if is_int { "1u" } else { "0x3F800000u" };
    let _ = writeln!(body, "let v = vec4<u32>({});", channels.join(","));
    for (component, selection) in attribute.dst_sel.iter().enumerate() {
        let value = match selection {
            s @ 0..=3 => format!("v.{}", CHANNEL[*s as usize]),
            4 => "0u".to_owned(),
            5 => one.to_owned(),
            _ => continue,
        };
        let _ = writeln!(body, "R[{gpr}].{} = {value};", CHANNEL[component]);
    }
    body.push_str("}\n");
}
