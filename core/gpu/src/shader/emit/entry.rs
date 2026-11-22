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
//! │ parsed shader      │   │ shader/emit/entry.rs     │   │ WGSL source text   │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ WGSL generation: entry   │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "entry" share of emit.rs (WGSL generation), moved here so that no file is longer than
//! 500 lines. It works exactly as if it were still written in that file. Writes the translated
//! shader as WGSL text. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;

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
