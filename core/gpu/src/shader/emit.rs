//! WGSL generation.
#![allow(clippy::too_many_lines, clippy::format_push_string)]

use std::{collections::BTreeSet, fmt::Write as _};

use super::{
    TEXTURE_UNITS, TextureDim, TextureKind, TextureSignature, TranslatedShader, WGSL_PRELUDE,
    cf::{
        AluClause, AluGroup, AluKind, AluOp, Cf, Export, StreamWrite, TexOp, VFETCH, parse_alu,
        parse_cf, parse_tex,
    },
    fetch::{Attribute, format},
};

const CHANNEL: [char; 4] = ['x', 'y', 'z', 'w'];

/// ALU source selects.
mod sel {
    pub const KCACHE0: u32 = 128;
    pub const KCACHE1: u32 = 160;
    pub const ZERO: u32 = 248;
    pub const ONE: u32 = 249;
    pub const ONE_INT: u32 = 250;
    pub const MINUS_ONE_INT: u32 = 251;
    pub const HALF: u32 = 252;
    pub const LITERAL: u32 = 253;
    pub const PV: u32 = 254;
    pub const PS: u32 = 255;
    pub const CFILE: u32 = 256;
}

/// TEX opcodes.
mod tex {
    pub const LD: u32 = 0x03;
    pub const GET_TEXTURE_RESINFO: u32 = 0x04;
    pub const GET_COMP_TEX_LOD: u32 = 0x06;
    pub const GET_GRADIENTS_H: u32 = 0x07;
    pub const GET_GRADIENTS_V: u32 = 0x08;
    pub const SET_GRADIENTS_H: u32 = 0x0B;
    pub const SET_GRADIENTS_V: u32 = 0x0C;
    pub const SET_CUBEMAP_INDEX: u32 = 0x0E;
    pub const FETCH4: u32 = 0x0F;
    pub const SAMPLE: u32 = 0x10;
    pub const SAMPLE_L: u32 = 0x11;
    pub const SAMPLE_LB: u32 = 0x12;
    pub const SAMPLE_LZ: u32 = 0x13;
    pub const SAMPLE_G: u32 = 0x14;
    pub const SAMPLE_C: u32 = 0x18;
    pub const SAMPLE_C_L: u32 = 0x19;
    pub const SAMPLE_C_LZ: u32 = 0x1B;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Vertex,
    Pixel,
}

impl Stage {
    const fn prefix(self) -> &'static str {
        match self {
            Self::Vertex => "vs",
            Self::Pixel => "ps",
        }
    }
}

/// Shared state of a translation.
/// Vertex shader entry point of `RECTS` draws.
pub const RECT_ENTRY_POINT: &str = "main_rect";
/// Marks an index as a generated corner of the rect at the arena word in the low bits.
pub const RECT_CORNER_FLAG: u32 = 0x8000_0000;
/// Which vertex of the rect's second triangle the index stands for.
pub const RECT_CORNER_SHIFT: u32 = 29;
const RECT_WORD_MASK: u32 = (1 << RECT_CORNER_SHIFT) - 1;

struct Translator<'a> {
    stage: Stage,
    code: &'a [u8],
    textures: &'a [TextureSignature; TEXTURE_UNITS],
    body: String,
    max_gpr: u32,
    relative_gpr: bool,
    next_temp: u32,
    used_samplers: BTreeSet<(u8, u8)>,
    used_textures: BTreeSet<u8>,
    color_exports: u8,
    writes_depth: bool,
    /// Params exported by a vertex shader.
    param_exports: BTreeSet<u32>,
    loop_depth: u32,
    uses_stack: bool,
    /// Writes stream-out buffers (`MEM_STREAM*_WRITE`).
    stream_out: bool,
}

impl<'a> Translator<'a> {
    fn new(stage: Stage, code: &'a [u8], textures: &'a [TextureSignature; TEXTURE_UNITS]) -> Self {
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
            loop_depth: 0,
            uses_stack: false,
            stream_out: false,
        }
    }

    fn temp(&mut self) -> String {
        self.next_temp += 1;
        format!("t{}", self.next_temp)
    }

    fn line(&mut self, text: &str) {
        self.body.push_str(text);
        self.body.push('\n');
    }

    fn note_gpr(&mut self, index: u32) {
        self.max_gpr = self.max_gpr.max(index);
    }

    fn gpr_index(&mut self, index: u32, rel: bool, mode: u32) -> String {
        if rel {
            self.relative_gpr = true;
            format!("R[{}u + {}]", index, address_register(mode))
        } else {
            self.note_gpr(index);
            format!("R[{index}]")
        }
    }

    /// Bits of a source operand, with its modifiers applied.
    fn source(
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

    fn alu_clause(&mut self, clause: &AluClause) {
        let groups = parse_alu(self.code, clause);
        self.line("if (exec_on) {");
        self.line("var next_active = true;");
        for group in &groups {
            self.alu_group(group, clause);
        }
        self.line("exec_on = next_active;");
        self.line("}");
    }

    fn alu_group(&mut self, group: &AluGroup, clause: &AluClause) {
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
    fn modifiers(op: &AluOp, bits: String, is_float: bool) -> Option<String> {
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

    fn alu_op(&mut self, op: &AluOp, clause: &AluClause, group: &AluGroup) -> Option<String> {
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

    fn tex_clause(&mut self, addr: u32, count: u32) {
        self.line("if (exec_on) {");
        for op in parse_tex(self.code, addr, count) {
            self.tex_op(&op);
        }
        self.line("}");
    }

    fn coord(&mut self, op: &TexOp, component: usize) -> String {
        self.note_gpr(op.src);
        match op.src_sel[component] {
            s @ 0..=3 => format!("bitcast<f32>(R[{}].{})", op.src, CHANNEL[s as usize]),
            4 => "0.0".to_owned(),
            _ => "1.0".to_owned(),
        }
    }

    fn tex_op(&mut self, op: &TexOp) {
        match op.opcode {
            VFETCH => {
                // First uniform block is buffer 0x80.
                self.note_gpr(op.src);
                let block = op.texture.saturating_sub(0x80).min(15);
                let index = format!("R[{}].{}", op.src, CHANNEL[op.src_sel[0] as usize]);
                let name = self.temp();
                let base = format!("(P.{}_ubo[{block}] >> 2u) + {index} * 4u", self.stage.prefix());
                let line = format!(
                    "let {name} = vec4<u32>(arena[{base}], arena[{base} + 1u], arena[{base} + 2u], arena[{base} + 3u]);"
                );
                self.line(&line);
                self.write_tex_result(op, &name);
                return;
            }
            tex::SET_GRADIENTS_H | tex::SET_GRADIENTS_V | tex::SET_CUBEMAP_INDEX => {
                if op.opcode != tex::SET_CUBEMAP_INDEX {
                    let target =
                        if op.opcode == tex::SET_GRADIENTS_H { "GRAD_H" } else { "GRAD_V" };
                    let (x, y, z) = (self.coord(op, 0), self.coord(op, 1), self.coord(op, 2));
                    let line = format!("{target} = vec3<f32>({x},{y},{z});");
                    self.line(&line);
                }
                return;
            }
            _ => {}
        }
        let unit = (op.texture as usize).min(TEXTURE_UNITS - 1);
        let sampler_unit = (op.sampler as usize).min(TEXTURE_UNITS - 1);
        let signature = self.textures[unit];
        let prefix = self.stage.prefix();
        let tex_name = format!("{prefix}_tex{unit}");
        let sampler_name = format!("{prefix}_smp{unit}");

        if op.opcode == tex::GET_GRADIENTS_H || op.opcode == tex::GET_GRADIENTS_V {
            let f = if op.opcode == tex::GET_GRADIENTS_H { "dpdx" } else { "dpdy" };
            let (x, y, z) = (self.coord(op, 0), self.coord(op, 1), self.coord(op, 2));
            let name = self.temp();
            let line = if self.stage == Stage::Pixel {
                format!(
                    "let {name} = bitcast<vec4<u32>>(vec4<f32>({f}({x}),{f}({y}),{f}({z}),0.0));"
                )
            } else {
                format!("let {name} = vec4<u32>(0u);")
            };
            self.line(&line);
            self.write_tex_result(op, &name);
            return;
        }
        self.used_textures.insert(unit as u8);
        let sampled = matches!(
            op.opcode,
            tex::SAMPLE
                | tex::SAMPLE_L
                | tex::SAMPLE_LB
                | tex::SAMPLE_LZ
                | tex::SAMPLE_G
                | tex::SAMPLE_C
                | tex::SAMPLE_C_L
                | tex::SAMPLE_C_LZ
                | tex::FETCH4
        );
        if sampled {
            self.used_samplers.insert((unit as u8, sampler_unit as u8));
        }

        match op.opcode {
            tex::GET_TEXTURE_RESINFO => {
                let name = self.temp();
                let size = match signature.dim {
                    TextureDim::D2 => {
                        let scale = self.texture_scale(unit as u32);
                        format!("vec3<u32>(textureDimensions({tex_name}) / u32({scale}), 1u)")
                    }
                    TextureDim::Cube => {
                        format!("vec3<u32>(textureDimensions({tex_name}), 1u)")
                    }
                    TextureDim::D2Array => format!(
                        "vec3<u32>(textureDimensions({tex_name}), textureNumLayers({tex_name}))"
                    ),
                    TextureDim::D3 => format!("textureDimensions({tex_name})"),
                };
                let line = format!("let {name} = vec4<u32>({size}, textureNumLevels({tex_name}));");
                self.line(&line);
                self.write_tex_result(op, &name);
            }
            tex::GET_COMP_TEX_LOD => {
                let name = self.temp();
                let line = format!("let {name} = vec4<u32>(0u);");
                self.line(&line);
                self.write_tex_result(op, &name);
            }
            tex::LD => {
                let (x, y, z) =
                    (self.int_coord(op, 0), self.int_coord(op, 1), self.int_coord(op, 2));
                let lod = self.int_coord(op, 3);
                let load = match signature.dim {
                    TextureDim::D2 => {
                        let scale = self.texture_scale(unit as u32);
                        format!("textureLoad({tex_name}, vec2<i32>({x},{y}) * i32({scale}), {lod})")
                    }
                    TextureDim::Cube => {
                        format!("textureLoad({tex_name}, vec2<i32>({x},{y}), {lod})")
                    }
                    TextureDim::D2Array => {
                        format!("textureLoad({tex_name}, vec2<i32>({x},{y}), {z}, {lod})")
                    }
                    TextureDim::D3 => {
                        format!("textureLoad({tex_name}, vec3<i32>({x},{y},{z}), {lod})")
                    }
                };
                let bits = self.texel_bits(signature, &load);
                let swizzled = self.swizzle_texel(signature, &bits);
                self.write_tex_result(op, &swizzled);
            }
            tex::FETCH4 if signature.dim != TextureDim::D3 => {
                let call = self.sample_call(op, signature, unit as u32, &tex_name, &sampler_name);
                let bits = if matches!(signature.kind, TextureKind::Uint) {
                    call
                } else {
                    format!("bitcast<vec4<u32>>({call})")
                };
                // Four texels of one channel, so the view swizzle does not apply to the result.
                // FETCH4 orders them yzxw, like `textureGather`'s xyzw.
                let name = self.temp();
                self.line(&format!("let {name} = ({bits}).zxyw;"));
                self.write_tex_result(op, &name);
            }
            _ => {
                let call = self.sample_call(op, signature, unit as u32, &tex_name, &sampler_name);
                let bits = self.texel_bits(signature, &call);
                let swizzled = self.swizzle_texel(signature, &bits);
                self.write_tex_result(op, &swizzled);
            }
        }
    }

    fn int_coord(&mut self, op: &TexOp, component: usize) -> String {
        self.note_gpr(op.src);
        match op.src_sel[component] {
            s @ 0..=3 => format!("bitcast<i32>(R[{}].{})", op.src, CHANNEL[s as usize]),
            4 => "0".to_owned(),
            _ => "1".to_owned(),
        }
    }

    /// The expression sampling the texture.
    /// Expression for how much larger than the title thinks texture `unit` is.
    fn texture_scale(&self, unit: u32) -> String {
        let mask = if self.stage == Stage::Pixel { "P.ps_scaled" } else { "P.vs_scaled" };
        format!("tex_scale({mask}, {unit}u)")
    }

    fn sample_call(
        &mut self,
        op: &TexOp,
        signature: TextureSignature,
        unit: u32,
        tex_name: &str,
        sampler_name: &str,
    ) -> String {
        let (x, y, z) = (self.coord(op, 0), self.coord(op, 1), self.coord(op, 2));
        let lod_arg = self.coord(op, 3);
        let scale_x = !op.normalized[0];
        let scale_y = !op.normalized[1];
        let (x, y) = if scale_x || scale_y {
            let scale = self.texture_scale(unit);
            let size = format!("(vec2<f32>(textureDimensions({tex_name})) / {scale})");
            (
                if scale_x { format!("({x} / {size}.x)") } else { x },
                if scale_y { format!("({y} / {size}.y)") } else { y },
            )
        } else {
            (x, y)
        };
        let coords = match signature.dim {
            TextureDim::D2 => format!("vec2<f32>({x},{y})"),
            // Latte (like GLSL) rounds the slice to the nearest one: a slice of 2.9999 is 3.
            TextureDim::D2Array => format!("vec2<f32>({x},{y}), i32(floor({z} + 0.5))"),
            TextureDim::D3 => format!("vec3<f32>({x},{y},{z})"),
            TextureDim::Cube => format!("cube_direction(vec2<f32>({x},{y}), i32({z}))"),
        };
        // Offsets come in half texels; the host takes whole ones. Cube maps have none.
        let (off_x, off_y, off_z) = (op.offset[0] / 2, op.offset[1] / 2, op.offset[2] / 2);
        let offset = match signature.dim {
            TextureDim::D2 | TextureDim::D2Array if off_x != 0 || off_y != 0 => {
                format!(", vec2<i32>({off_x},{off_y})")
            }
            TextureDim::D3 if off_x != 0 || off_y != 0 || off_z != 0 => {
                format!(", vec3<i32>({off_x},{off_y},{off_z})")
            }
            _ => String::new(),
        };
        let in_pixel = self.stage == Stage::Pixel;
        match op.opcode {
            tex::SAMPLE if in_pixel => {
                format!("textureSample({tex_name}, {sampler_name}, {coords}{offset})")
            }
            tex::SAMPLE | tex::SAMPLE_LZ => {
                if signature.kind == TextureKind::Depth {
                    format!("textureSampleLevel({tex_name}, {sampler_name}, {coords}, 0{offset})")
                } else {
                    format!("textureSampleLevel({tex_name}, {sampler_name}, {coords}, 0.0{offset})")
                }
            }
            tex::SAMPLE_L => {
                if signature.kind == TextureKind::Depth {
                    format!(
                        "textureSampleLevel({tex_name}, {sampler_name}, {coords}, i32({lod_arg}){offset})"
                    )
                } else {
                    format!(
                        "textureSampleLevel({tex_name}, {sampler_name}, {coords}, {lod_arg}{offset})"
                    )
                }
            }
            tex::SAMPLE_LB if in_pixel => {
                format!(
                    "textureSampleBias({tex_name}, {sampler_name}, {coords}, {lod_arg}{offset})"
                )
            }
            tex::SAMPLE_LB => {
                format!("textureSampleLevel({tex_name}, {sampler_name}, {coords}, 0.0{offset})")
            }
            tex::SAMPLE_G => {
                let (h, v) = match signature.dim {
                    TextureDim::D2 | TextureDim::D2Array => ("GRAD_H.xy", "GRAD_V.xy"),
                    _ => ("GRAD_H", "GRAD_V"),
                };
                format!("textureSampleGrad({tex_name}, {sampler_name}, {coords}, {h}, {v}{offset})")
            }
            tex::FETCH4 if signature.dim != TextureDim::D3 => {
                // Like Cemu, a small bias against rounding down when point sampling.
                let coords = if signature.dim == TextureDim::D2 {
                    format!("{coords} + vec2<f32>(0.0001)")
                } else {
                    coords
                };
                if signature.kind == TextureKind::Depth {
                    format!("textureGather({tex_name}, {sampler_name}, {coords}{offset})")
                } else {
                    let channel = match signature.comp_sel[0] {
                        c @ 0..=3 => c,
                        _ => 0,
                    };
                    format!(
                        "textureGather({channel}, {tex_name}, {sampler_name}, {coords}{offset})"
                    )
                }
            }
            tex::SAMPLE_C | tex::SAMPLE_C_L | tex::SAMPLE_C_LZ => {
                let reference_slot = if op.opcode == tex::SAMPLE_C_L { 2 } else { 3 };
                let reference = self.coord(op, reference_slot);
                if !signature.comparison {
                    // The host compares only depth surfaces through comparison samplers:
                    // read the value and compare here.
                    let value = if signature.kind == TextureKind::Depth {
                        format!(
                            "textureSampleLevel({tex_name}, {sampler_name}, {coords}, 0{offset})"
                        )
                    } else {
                        format!(
                            "textureSampleLevel({tex_name}, {sampler_name}, {coords}, 0.0{offset}).x"
                        )
                    };
                    let passed = compare_expression(signature.compare_function, &reference, &value);
                    let result = format!("select(0.0, 1.0, {passed})");
                    return if signature.kind == TextureKind::Depth {
                        result
                    } else {
                        format!("vec4<f32>({result}, 0.0, 0.0, 1.0)")
                    };
                }
                if in_pixel && op.opcode == tex::SAMPLE_C {
                    format!(
                        "textureSampleCompare({tex_name}, {sampler_name}, {coords}, {reference}{offset})"
                    )
                } else {
                    format!(
                        "textureSampleCompareLevel({tex_name}, {sampler_name}, {coords}, {reference}{offset})"
                    )
                }
            }
            other => {
                log::warn!("unhandled TEX opcode {other:#x}");
                match signature.kind {
                    TextureKind::Depth => "0.0",
                    TextureKind::Float => "vec4<f32>(0.0)",
                    TextureKind::Uint => "vec4<u32>(0u)",
                    TextureKind::Sint => "vec4<i32>(0)",
                }
                .to_owned()
            }
        }
    }

    /// Stores the texel in a temporary as raw bits.
    fn texel_bits(&mut self, signature: TextureSignature, expr: &str) -> String {
        let name = self.temp();
        let line = match signature.kind {
            TextureKind::Float => format!("let {name} = bitcast<vec4<u32>>({expr});"),
            TextureKind::Depth => {
                format!("let {name} = bitcast<vec4<u32>>(vec4<f32>({expr}, 0.0, 0.0, 1.0));")
            }
            TextureKind::Uint => format!("let {name} = {expr};"),
            TextureKind::Sint => format!("let {name} = bitcast<vec4<u32>>({expr});"),
        };
        self.line(&line);
        name
    }

    /// Applies the view swizzle of `SQ_TEX_RESOURCE_WORD4`.
    fn swizzle_texel(&mut self, signature: TextureSignature, bits: &str) -> String {
        if signature.comp_sel == [0, 1, 2, 3] {
            return bits.to_owned();
        }
        let one = if matches!(signature.kind, TextureKind::Uint | TextureKind::Sint) {
            "1u"
        } else {
            "0x3F800000u"
        };
        let parts = signature
            .comp_sel
            .map(|s| match s {
                0..=3 => format!("{bits}.{}", CHANNEL[s as usize]),
                4 => "0u".to_owned(),
                _ => one.to_owned(),
            })
            .join(",");
        let name = self.temp();
        self.line(&format!("let {name} = vec4<u32>({parts});"));
        name
    }

    fn write_tex_result(&mut self, op: &TexOp, bits: &str) {
        self.note_gpr(op.dest);
        for (component, selection) in op.dst_sel.iter().enumerate() {
            let value = match selection {
                s @ 0..=3 => format!("{bits}.{}", CHANNEL[*s as usize]),
                4 => "0u".to_owned(),
                5 => "0x3F800000u".to_owned(),
                _ => continue,
            };
            let line = format!("R[{}].{} = {value};", op.dest, CHANNEL[component]);
            self.line(&line);
        }
    }

    fn export_value(&mut self, export: &Export, burst: u32) -> String {
        let register = export.gpr + burst;
        let index = self.gpr_index(register, export.rel, 0);
        let parts = export
            .sel
            .map(|s| match s {
                0..=3 => format!("bitcast<f32>({index}.{})", CHANNEL[s as usize]),
                4 | 7 => "0.0".to_owned(),
                _ => "1.0".to_owned(),
            })
            .join(",");
        format!("vec4<f32>({parts})")
    }

    fn control_flow(&mut self, list: &[Cf], export: &mut dyn FnMut(&mut Self, &Export)) {
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
                            if self.loop_depth > 0 {
                                self.line("if (!exec_on) { break; }");
                            }
                        }
                        AluKind::Plain | AluKind::PushBefore | AluKind::Continue => {}
                    }
                }
                Cf::LoopStart => {
                    self.uses_stack = true;
                    let depth = self.loop_depth;
                    self.line(&format!("stack[sp] = exec_on; sp += 1; let loop_sp{depth} = sp; var loop_guard{depth} = 0u;"));
                    self.line("loop {");
                    self.line(&format!(
                        "loop_guard{depth} += 1u; if (loop_guard{depth} > 4096u) {{ break; }}"
                    ));
                    self.line("if (!exec_on) { break; }");
                    self.loop_depth += 1;
                }
                Cf::LoopEnd => {
                    if self.loop_depth > 0 {
                        self.loop_depth -= 1;
                        let depth = self.loop_depth;
                        self.line("if (!exec_on) { break; }");
                        self.line("}");
                        self.line(&format!("sp = loop_sp{depth}; sp -= 1; exec_on = stack[sp];"));
                    }
                }
                Cf::LoopBreak => {
                    if self.loop_depth > 0 {
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
    fn stream_write(&mut self, write: &StreamWrite) {
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

    fn pop(&mut self, count: u32) {
        if count > 0 {
            self.line(&format!("sp -= {count}; exec_on = stack[sp];"));
        }
    }

    fn register_declarations(&self) -> String {
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
    fn rect_entry_point(&self) -> String {
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
    fn resource_declarations(&self) -> String {
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

    fn finish(self, wgsl: String) -> TranslatedShader {
        TranslatedShader {
            wgsl,
            samplers: self.used_samplers.into_iter().collect(),
            textures: self.used_textures.into_iter().collect(),
            color_exports: self.color_exports,
            writes_depth: self.writes_depth,
            stream_out: self.stream_out,
        }
    }
}

/// `reference OP value` for `DEPTH_COMPARE_FUNCTION` `function`.
fn compare_expression(function: u8, reference: &str, value: &str) -> String {
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

fn address_register(mode: u32) -> String {
    match mode {
        0..=3 => format!("u32(AR.{})", CHANNEL[mode as usize]),
        _ => "0u".to_owned(),
    }
}

fn texture_type(signature: TextureSignature) -> String {
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

fn attribute_fetch(body: &mut String, attribute: &Attribute, gpr: u32) {
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

/// Translates a vertex shader. `flat_params[i]` selects flat interpolation of
/// parameter export `i`.
pub fn translate_vertex(
    code: &[u8],
    attributes: &[Attribute],
    semantic_table: &[u32],
    textures: &[TextureSignature; TEXTURE_UNITS],
    flat_params: u32,
) -> TranslatedShader {
    let mut t = Translator::new(Stage::Vertex, code, textures);
    let list = parse_cf(code);

    let mut fetch = String::new();
    for attribute in attributes {
        // SQ_VTX_SEMANTIC_N maps semantic IDs to R(N + 1).
        let Some(slot) = semantic_table.iter().position(|id| id & 0xFF == attribute.semantic)
        else {
            continue;
        };
        let gpr = slot as u32 + 1;
        t.note_gpr(gpr);
        attribute_fetch(&mut fetch, attribute, gpr);
    }

    let mut has_position = false;
    let mut export = |t: &mut Translator<'_>, e: &Export| match e.kind {
        1 if e.array_base == 60 => {
            has_position = true;
            let value = t.export_value(e, 0);
            t.line(&format!("vout.position = {value};"));
        }
        2 => {
            for burst in 0..e.burst {
                let index = e.array_base + burst;
                t.param_exports.insert(index);
                let value = t.export_value(e, burst);
                t.line(&format!("vout.p{index} = {value};"));
            }
        }
        _ => {}
    };
    t.control_flow(&list, &mut export);

    let mut wgsl = String::from(WGSL_PRELUDE);
    wgsl.push_str(&t.register_declarations());
    wgsl.push_str(&t.resource_declarations());
    wgsl.push_str("struct VsOut {\n@builtin(position) position: vec4<f32>,\n");
    for &param in &t.param_exports {
        let interpolation =
            if flat_params & (1 << param.min(31)) != 0 { "@interpolate(flat) " } else { "" };
        let _ = writeln!(wgsl, "@location({param}) {interpolation}p{param}: vec4<f32>,");
    }
    wgsl.push_str("}\nfn vs_body(vertex_index: u32, instance_index: u32) -> VsOut {\n");
    wgsl.push_str("var vout: VsOut;\nR[0] = vec4<u32>(vertex_index, 0u, 0u, instance_index);\n");
    wgsl.push_str(&fetch);
    wgsl.push_str(&t.body);
    // Latte clip space to the host's: y is flipped by the viewport sign, z may span [-w, w].
    wgsl.push_str("vout.position.y = vout.position.y * P.y_sign;\n");
    wgsl.push_str(
        "if (P.half_z == 0u) { vout.position.z = (vout.position.z + vout.position.w) * 0.5; }\n",
    );
    wgsl.push_str("return vout;\n}\n");
    wgsl.push_str("@vertex\nfn main(@builtin(vertex_index) vertex_index: u32, @builtin(instance_index) instance_index: u32) -> VsOut {\nreturn vs_body(vertex_index, instance_index);\n}\n");
    wgsl.push_str(&t.rect_entry_point());
    if !has_position {
        log::debug!("vertex shader without position export");
    }
    t.finish(wgsl)
}

/// Register state a pixel shader depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelInputs<'a> {
    /// `SPI_PS_IN_CONTROL_0`.
    pub control0: u32,
    /// `SPI_PS_IN_CONTROL_1`.
    pub control1: u32,
    /// `SPI_PS_INPUT_CNTL_N`.
    pub input_controls: &'a [u32],
    /// Semantic of each parameter the vertex shader exports (`SPI_VS_OUT_ID_N`), by index.
    pub vs_out_semantics: &'a [u32],
    /// Parameter indices the vertex shader actually exports.
    pub vs_exports: u32,
    /// `SX_ALPHA_TEST_CONTROL` compare function when alpha test is on.
    pub alpha_function: Option<u32>,
    /// Sample type of each bound color target (`Uint`/`Sint` need integer outputs).
    pub target_kinds: [TextureKind; 8],
}

/// Translates a pixel shader; returns it and the mask of flat-interpolated
/// parameters the vertex shader must match.
pub fn translate_pixel(
    code: &[u8],
    inputs: &PixelInputs<'_>,
    textures: &[TextureSignature; TEXTURE_UNITS],
) -> (TranslatedShader, u32) {
    let mut t = Translator::new(Stage::Pixel, code, textures);
    let list = parse_cf(code);
    let mut flat_params = 0_u32;

    let mut prologue = String::new();
    let mut input_fields = String::new();
    let position_enabled = (inputs.control0 >> 8) & 1 != 0;
    let position_gpr = (inputs.control0 >> 10) & 0x1F;
    let count = (inputs.control0 & 0x3F).min(32).min(inputs.input_controls.len() as u32);
    for i in 0..count {
        t.note_gpr(i);
        if position_enabled && i == position_gpr {
            continue;
        }
        let control = inputs.input_controls[i as usize];
        let semantic = control & 0xFF;
        let parameter = inputs
            .vs_out_semantics
            .iter()
            .enumerate()
            .find(|(index, id)| {
                **id == semantic && inputs.vs_exports & (1 << (*index).min(31)) != 0
            })
            .map(|(index, _)| index as u32);
        if let Some(parameter) = parameter {
            let flat = (control >> 10) & 1 != 0;
            if flat {
                flat_params |= 1 << parameter.min(31);
            }
            let interpolation = if flat { "@interpolate(flat) " } else { "" };
            let _ = writeln!(
                input_fields,
                "@location({parameter}) {interpolation}p{parameter}: vec4<f32>,"
            );
            let _ = writeln!(prologue, "R[{i}] = bitcast<vec4<u32>>(input.p{parameter});");
        } else {
            let default = match (control >> 8) & 3 {
                0 => "vec4<u32>(0u,0u,0u,0u)",
                1 => "vec4<u32>(0u,0u,0u,0x3F800000u)",
                2 => "vec4<u32>(0x3F800000u,0x3F800000u,0x3F800000u,0u)",
                _ => "vec4<u32>(0x3F800000u)",
            };
            let _ = writeln!(prologue, "R[{i}] = {default};");
        }
    }
    if position_enabled {
        t.note_gpr(position_gpr);
        // In the title's pixels, however large the host draws.
        let _ = writeln!(
            prologue,
            "R[{position_gpr}] = bitcast<vec4<u32>>(vec4<f32>(input.frag_position.xy / \
             select(1.0, P.res_scale, P.target_scaled != 0u), input.frag_position.zw));"
        );
    }
    let front_face = (inputs.control1 >> 8) & 1 != 0;
    if front_face {
        let chan = CHANNEL[((inputs.control1 >> 9) & 3) as usize];
        let register = (inputs.control1 >> 12) & 0x1F;
        t.note_gpr(register);
        let _ = writeln!(
            prologue,
            "R[{register}].{chan} = select(0u, 0x3F800000u, input.front_facing);"
        );
    }

    let target_kinds = inputs.target_kinds;
    let alpha_function = inputs.alpha_function;
    let mut export = |t: &mut Translator<'_>, e: &Export| {
        if e.kind != 0 {
            return;
        }
        if e.array_base == 61 {
            t.writes_depth = true;
            let value = t.export_value(e, 0);
            t.line(&format!("out.depth = ({value}).x;"));
            return;
        }
        for burst in 0..e.burst {
            let target = e.array_base + burst;
            if target >= 8 {
                continue;
            }
            t.color_exports |= 1 << target;
            let value = t.export_value(e, burst);
            if target == 0 {
                if let Some(function) = alpha_function {
                    let value_name = t.temp();
                    t.line(&format!("let {value_name} = ({value}).a;"));
                    let pass = match function & 7 {
                        0 => "false".to_owned(),
                        1 => format!("{value_name} < P.alpha_ref"),
                        2 => format!("{value_name} == P.alpha_ref"),
                        3 => format!("{value_name} <= P.alpha_ref"),
                        4 => format!("{value_name} > P.alpha_ref"),
                        5 => format!("{value_name} != P.alpha_ref"),
                        6 => format!("{value_name} >= P.alpha_ref"),
                        _ => "true".to_owned(),
                    };
                    t.line(&format!("if (!({pass})) {{ discard; }}"));
                }
            }
            let line = match target_kinds[target as usize] {
                TextureKind::Uint => format!("out.c{target} = bitcast<vec4<u32>>({value});"),
                TextureKind::Sint => format!("out.c{target} = bitcast<vec4<i32>>({value});"),
                _ => format!("out.c{target} = finite_color({value});"),
            };
            t.line(&line);
        }
    };
    t.control_flow(&list, &mut export);

    let mut wgsl = String::from(WGSL_PRELUDE);
    wgsl.push_str(&t.register_declarations());
    wgsl.push_str(&t.resource_declarations());
    wgsl.push_str("struct PsIn {\n@builtin(position) frag_position: vec4<f32>,\n@builtin(front_facing) front_facing: bool,\n");
    wgsl.push_str(&input_fields);
    wgsl.push_str("}\nstruct PsOut {\n");
    for target in 0..8 {
        if t.color_exports & (1 << target) != 0 {
            let ty = match target_kinds[target as usize] {
                TextureKind::Uint => "vec4<u32>",
                TextureKind::Sint => "vec4<i32>",
                _ => "vec4<f32>",
            };
            let _ = writeln!(wgsl, "@location({target}) c{target}: {ty},");
        }
    }
    if t.writes_depth {
        wgsl.push_str("@builtin(frag_depth) depth: f32,\n");
    }
    if t.color_exports == 0 && !t.writes_depth {
        wgsl.push_str("@location(0) unused: vec4<f32>,\n");
    }
    wgsl.push_str("}\n@fragment\nfn main(input: PsIn) -> PsOut {\nvar out: PsOut;\n");
    wgsl.push_str(&prologue);
    wgsl.push_str(&t.body);
    wgsl.push_str("return out;\n}\n");
    let translated = t.finish(wgsl);
    (translated, flat_params)
}
