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
//! │ parsed shader      │   │ shader/emit/fetches.rs   │   │ WGSL source text   │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ WGSL generation: fetches │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "fetches" share of emit.rs (WGSL generation), moved here so that no file is longer than
//! 500 lines. It works exactly as if it were still written in that file. Writes the translated
//! shader as WGSL text. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;

impl<'a> Translator<'a> {
    pub(super) fn tex_op(&mut self, op: &TexOp) {
        match op.opcode {
            VFETCH => {
                // First uniform block is buffer 0x80.
                self.note_gpr(op.src);
                let block = op.texture.saturating_sub(0x80).min(15);
                self.note_ubo(block as usize, None);
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

    pub(super) fn int_coord(&mut self, op: &TexOp, component: usize) -> String {
        self.note_gpr(op.src);
        match op.src_sel[component] {
            s @ 0..=3 => format!("bitcast<i32>(R[{}].{})", op.src, CHANNEL[s as usize]),
            4 => "0".to_owned(),
            _ => "1".to_owned(),
        }
    }

    /// The expression sampling the texture.
    /// Expression for how much larger than the title thinks texture `unit` is.
    pub(super) fn texture_scale(&self, unit: u32) -> String {
        let mask = if self.stage == Stage::Pixel { "P.ps_scaled" } else { "P.vs_scaled" };
        format!("tex_scale({mask}, {unit}u)")
    }

    pub(super) fn sample_call(
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
            tex::SAMPLE if in_pixel && signature.kind == TextureKind::Float => {
                let bias = format!("P.ps_lod_bias[{}]", op.sampler.min(17));
                format!("textureSampleBias({tex_name}, {sampler_name}, {coords}, {bias}{offset})")
            }
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
            tex::SAMPLE_LB if in_pixel && signature.kind == TextureKind::Float => {
                let bias = format!("({lod_arg} + P.ps_lod_bias[{}])", op.sampler.min(17));
                format!("textureSampleBias({tex_name}, {sampler_name}, {coords}, {bias}{offset})")
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
    pub(super) fn texel_bits(&mut self, signature: TextureSignature, expr: &str) -> String {
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
    pub(super) fn swizzle_texel(&mut self, signature: TextureSignature, bits: &str) -> String {
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

    pub(super) fn write_tex_result(&mut self, op: &TexOp, bits: &str) {
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

    pub(super) fn export_value(&mut self, export: &Export, burst: u32) -> String {
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
}
