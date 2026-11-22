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
//! │ GPU register state │   │ latte/draws.rs           │   │ textures, buffers, │
//! │ draw commands      │──►│ ──────────────────────── │──►│ draws              │
//! │                    │   │ draws → host work: draws │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "draws" share of latte.rs (draws → host work), moved here so that no file is longer than
//! 500 lines. It works exactly as if it were still written in that file. The core of the GPU
//! emulation: for each draw it gathers the game's shaders, textures and buffers and describes
//! the same draw for your graphics card. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;

impl Latte {
    /// Executes a draw from the current registers.
    /// Indices of a draw as the host takes them: little-endian `u32`, restarts as
    /// `u32::MAX`, primitives the host lacks made of triangles.
    pub(super) fn host_indices(
        &mut self,
        source: Option<&[u8]>,
        draw: &DrawRequest<'_>,
        mode: u32,
        reset_index: u32,
        stamp: u32,
    ) -> HostIndices {
        let mut max_index = draw.base_vertex.saturating_add(draw.count);
        let mut count = draw.count;
        let expanded = if let Some(source) = source {
            let mut converted = read_indices(source, draw.count, draw.index_type, draw.immediate);
            if reset_index != u32::MAX {
                for index in &mut converted {
                    if *index == reset_index {
                        *index = u32::MAX;
                    }
                }
            }
            max_index = converted
                .iter()
                .copied()
                .filter(|i| *i != u32::MAX)
                .max()
                .unwrap_or(0)
                .saturating_add(1);
            expand_indices(mode, Some(converted))
        } else {
            expand_indices(mode, needs_expansion(mode).then(|| (0..draw.count).collect()))
        };
        let offset = expanded.map(|list| {
            let list = if mode == primitive::RECTS {
                self.rect_corners(&list, draw.base_vertex)
            } else {
                list
            };
            count = list.len() as u32;
            let bytes: Vec<u8> = list.iter().flat_map(|i| i.to_le_bytes()).collect();
            self.arena_store(&bytes, 4)
        });
        HostIndices { offset, count, max_index, stamp, checked: self.frame }
    }

    pub fn draw(&mut self, memory: &Memory, regs: &mut RegisterFile, draw: &DrawRequest<'_>) {
        if regs.get(reg::VGT_GS_MODE) & 3 != 0 {
            // Geometry shaders have no host equivalent yet.
            self.skipped_draws += 1;
            return;
        }
        // DX_RASTERIZATION_KILL: only stream-out can observe such a draw.
        let killed = regs.get(reg::PA_CL_CLIP_CNTL) & (1 << 22) != 0;
        let streaming = regs.get(reg::VGT_STRMOUT_EN) & 1 != 0;
        if killed && !streaming {
            return;
        }
        let program_size = |start: u32| regs.get(start + 1) << 3;
        let code = |this: &mut Self, start: u32| {
            this.cached_code(memory, regs.get(start) << 8, program_size(start))
        };
        let (Some(vs), Some(ps), Some(fetch)) = (
            code(self, reg::SQ_PGM_START_VS),
            code(self, reg::SQ_PGM_START_PS),
            code(self, reg::SQ_PGM_START_FS),
        ) else {
            self.skipped_draws += 1;
            return;
        };
        let attributes = self.fetch_attributes(fetch.0, &fetch.1);
        let pair = self.program_pair(regs, vs, ps, fetch);
        let stream_only = killed && pair.vertex.stream_out;
        if killed && !stream_only {
            return;
        }

        let mut textures = Vec::new();
        self.bind_textures(
            memory,
            regs,
            &mut textures,
            TextureStage { resource_base: resource::VS_TEXTURE, binding_base: 0, sampler_base: 18 },
            &pair.vs_textures,
            &pair.vs_samplers,
        );
        if !stream_only {
            self.bind_textures(
                memory,
                regs,
                &mut textures,
                TextureStage {
                    resource_base: resource::PS_TEXTURE,
                    binding_base: TEXTURE_UNITS as u32,
                    sampler_base: 0,
                },
                &pair.ps_textures,
                &pair.ps_samplers,
            );
        }

        // Render targets.
        let cb_target_mask = regs.get(reg::CB_TARGET_MASK);
        let cb_shader_mask = regs.get(reg::CB_SHADER_MASK);
        let color_control = regs.get(reg::CB_COLOR_CONTROL);
        let color_enabled = (color_control >> 4) & 7 == 0;
        let mut state = pipeline_state(regs);
        let mut color_targets = [None; 8];
        let slot_blend = state.blend;
        let slots = export_slots(cb_shader_mask, pair.color_exports);
        for (target, slot) in slots.into_iter().enumerate() {
            let Some(slot) = slot.map(usize::from) else { continue };
            let words = self.color_targets[slot];
            let mask =
                ((cb_target_mask >> (4 * slot)) & (cb_shader_mask >> (4 * slot)) & 0xF) as u8;
            if stream_only
                || !color_enabled
                || !words.is_valid()
                || pair.color_exports & (1 << target) == 0
            {
                continue;
            }
            let key = Self::key_of_target(&words, false);
            self.note_readback(&words, key);
            self.note_target(key);
            state.color_formats[target] = Some(key.format);
            state.color_masks[target] = mask;
            state.blend[target] = slot_blend[slot];
            color_targets[target] = Some((key, words.slice));
        }
        let depth_words = self.depth_target;
        let depth_control = regs.get(reg::DB_DEPTH_CONTROL);
        let wants_depth = depth_control & 0b111 != 0;
        let depth_target = (depth_words.is_valid() && wants_depth && !stream_only).then(|| {
            let key = Self::key_of_target(&depth_words, true);
            self.note_target(key);
            state.depth_format = Some(key.format);
            (key, depth_words.slice)
        });
        if depth_target.is_none() {
            state.depth_test = false;
            state.depth_write = false;
            state.stencil_test = false;
        }
        if stream_only {
            // The host wants something to render to: a small depth surface it keeps itself.
            state.depth_format = Some(HostFormat::Depth32Float);
        } else if color_targets.iter().all(Option::is_none) && depth_target.is_none() {
            self.skipped_draws += 1;
            return;
        }

        // Uniforms: registers or blocks.
        let mut params = [0_u32; PARAMS_SIZE / 4];
        let uniform_blocks = regs.get(config::SQ_CONFIG) & (1 << 8) == 0;
        if uniform_blocks {
            // Hashing and copying every bound block on every draw was most of the cost of a
            // draw; blocks the shaders never read stay at offset 0.
            for block in 0..16 {
                if pair.vs_blocks & (1 << block) != 0 {
                    params[8 + block as usize] = self.store_uniform_block(
                        memory,
                        regs,
                        resource::VS_UNIFORM_BLOCK,
                        block,
                        pair.vs_ubo_vec4s[block as usize],
                    );
                }
                if pair.ps_blocks & (1 << block) != 0 {
                    params[24 + block as usize] = self.store_uniform_block(
                        memory,
                        regs,
                        resource::PS_UNIFORM_BLOCK,
                        block,
                        pair.ps_ubo_vec4s[block as usize],
                    );
                }
            }
        } else {
            let read_alu = |offset: u32| -> Vec<u8> {
                (0..0x400)
                    .flat_map(|i| regs.get(base::ALU_CONST + offset + i).to_le_bytes())
                    .collect()
            };
            let ps_regs = read_alu(0);
            let vs_regs = read_alu(0x400);
            params[0] = self.arena_store(&vs_regs, 16);
            params[1] = self.arena_store(&ps_regs, 16);
        }

        // Vertex data.
        let mode = regs.get(config::VGT_PRIMITIVE_TYPE);
        let resets = regs.get(reg::VGT_MULTI_PRIM_IB_RESET_EN) & 1 != 0
            && matches!(
                mode,
                primitive::LINE_STRIP | primitive::TRIANGLE_STRIP | primitive::TRIANGLE_FAN
            );
        let reset_index =
            if resets { regs.get(reg::VGT_MULTI_PRIM_IB_RESET_INDX) } else { u32::MAX };
        let width =
            if matches!(draw.index_type, index_type::U32_LE | index_type::U32_BE) { 4 } else { 2 };
        let guest_indices = draw.index_address.and_then(|address| {
            Some((address, memory.slice(address, draw.count.saturating_mul(width)).ok()?))
        });
        let key: IndexKey = (
            draw.index_address.unwrap_or(0),
            draw.count,
            draw.index_type,
            mode,
            reset_index,
            draw.base_vertex,
        );
        let cached = self.index_buffers.get(&key).copied().filter(|known| {
            self.buffer_cache
                && guest_indices.is_some()
                && self.frame.wrapping_sub(known.checked) < BUFFER_RECHECK_FRAMES
                && self.writes.unchanged_since(key.0, draw.count.saturating_mul(width), known.stamp)
        });
        let host = match cached {
            Some(host) => host,
            None => {
                let stamp = self.writes.now;
                let source = guest_indices.map(|(_, bytes)| bytes).or(draw.indices);
                let host = self.host_indices(source, draw, mode, reset_index, stamp);
                if guest_indices.is_some() {
                    self.index_buffers.insert(key, host);
                }
                host
            }
        };
        let (indices, index_count, max_index) = (host.offset, host.count, host.max_index);

        // Attribute buffers are stored raw: the shader decodes them.
        let vertex_end = max_index.saturating_add(draw.base_vertex);
        let instance_end = draw.instances.max(1);
        let mut used_buffers = [false; 16];
        for attribute in attributes.iter() {
            used_buffers[attribute.buffer as usize] = true;
        }
        for (buffer, used) in used_buffers.iter().enumerate() {
            if !*used {
                continue;
            }
            let start = base::RESOURCE + resource::ATTRIBUTE_BLOCK + buffer as u32 * 7;
            let (physical, size, word2) =
                (regs.get(start), regs.get(start + 1).wrapping_add(1), regs.get(start + 2));
            let stride = (word2 >> 11) & 0xFFFF;
            if physical == 0 {
                continue;
            }
            if let Some(offset) = self.streamed_attribute(memory, physical) {
                params[40 + buffer] = offset;
                params[56 + buffer] = stride;
                continue;
            }
            let Some(effective) = wiiu_mem_addr::physical_to_effective(physical) else { continue };
            let per_instance =
                attributes.iter().any(|a| a.buffer as usize == buffer && a.rate != 0);
            let count = if per_instance { instance_end } else { vertex_end };
            let needed = u64::from(count) * u64::from(stride.max(1)) + 64;
            let available = needed.min(u64::from(size)).min(0x0400_0000) as u32;
            let Ok(bytes) = memory.slice(effective, available) else { continue };
            params[40 + buffer] = self.arena_store_guest(effective, bytes, 4);
            params[56 + buffer] = stride;
        }
        let y_scale = f32::from_bits(regs.get(reg::PA_CL_VPORT_XSCALE + 2));
        params[2] = regs.get(reg::SX_ALPHA_REF);
        params[3] = (if y_scale > 0.0 { -1.0_f32 } else { 1.0_f32 }).to_bits();
        let clip_control = regs.get(reg::PA_CL_CLIP_CNTL);
        params[4] = u32::from(clip_control & (1 << 19) != 0);
        params[5] = regs.get(reg::VGT_INSTANCE_STEP_RATE_0);
        params[6] = regs.get(reg::VGT_INSTANCE_STEP_RATE_0 + 1);
        params[7] = draw.base_instance;
        let stream_copies = if streaming && pair.vertex.stream_out {
            self.stream_out_setup(
                memory,
                regs,
                &mut params,
                draw.count,
                draw.instances.max(1),
                draw.base_vertex,
            )
        } else {
            Vec::new()
        };
        // Shaders work in the title's pixels: these tell them what is larger than it thinks.
        let resolution = self.resolution;
        let target =
            color_targets.iter().flatten().next().or(depth_target.as_ref()).map(|(key, _)| key);
        params[81] = (resolution.factor as f32).to_bits();
        params[82] = u32::from(target.is_some_and(|key| resolution.applies(key)));
        for texture in &textures {
            let is_target = self.host_surfaces.contains_key(&(
                texture.key.address,
                texture.key.width,
                texture.key.height,
            ));
            if is_target && resolution.applies(&texture.key) {
                let unit = texture.binding / 2;
                let (word, unit) = if unit < TEXTURE_UNITS as u32 {
                    (83, unit)
                } else {
                    (84, unit - TEXTURE_UNITS as u32)
                };
                params[word] |= 1 << unit;
            }
        }
        // The sampler's LOD bias (signed 6.6 in the top of word 1): titles sharpen textures
        // with a negative one, which the host sampler has no field for.
        for unit in 0..PS_SAMPLERS {
            let word1 = regs.get(base::SAMPLER + unit as u32 * 3 + 1);
            let bias = (word1.cast_signed() >> 20) as f32 / 64.0;
            params[PARAM_PS_LOD_BIAS + unit] = bias.to_bits();
        }
        let params_bytes: Vec<u8> = params.iter().flat_map(|w| w.to_le_bytes()).collect();
        let params_offset = self.arena_write_unique(&params_bytes, PARAMS_ALIGN);

        let viewport = viewport_of(regs);
        let scissor_tl = regs.get(reg::PA_SC_GENERIC_SCISSOR_TL);
        let scissor_br = regs.get(reg::PA_SC_GENERIC_SCISSOR_BR);
        let (sx, sy) = (scissor_tl & 0x7FFF, (scissor_tl >> 16) & 0x7FFF);
        let (ex, ey) = (scissor_br & 0x7FFF, (scissor_br >> 16) & 0x7FFF);
        let stencil_reference = (regs.get(reg::DB_STENCILREFMASK) & 0xFF) as u8;
        let blend_constant =
            std::array::from_fn(|i| f32::from_bits(regs.get(reg::CB_BLEND_RED + i as u32)));
        state.topology = if stream_only { Topology::Points } else { topology_of(mode) };
        let command = DrawCommand {
            stream_only,
            stream_copies,
            vertex: Arc::clone(&pair.vertex),
            pixel: Arc::clone(&pair.pixel),
            state,
            color_targets,
            depth_target,
            textures,
            params_offset,
            viewport,
            scissor: [sx, sy, ex.saturating_sub(sx), ey.saturating_sub(sy)],
            stencil_reference,
            blend_constant,
            count: index_count,
            instances: draw.instances.max(1),
            first_vertex: 0,
            index_offset: indices,
            // Rect corners carry the base vertex already.
            base_vertex: if mode == primitive::RECTS { 0 } else { draw.base_vertex.cast_signed() },
        };
        self.commands.push(RenderCommand::Draw(Box::new(command)));
    }
}

/// Uniform blocks a translated stage reads (`P.<stage>_ubo[n]`), as a bit mask.
pub(super) fn used_uniform_blocks(wgsl: &str, stage: &str) -> u16 {
    let pattern = format!("P.{stage}_ubo[");
    let mut mask = 0;
    let mut rest = wgsl;
    while let Some(at) = rest.find(&pattern) {
        rest = &rest[at + pattern.len()..];
        let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        match rest[..digits].parse::<u32>() {
            Ok(block) if block < 16 => mask |= 1 << block,
            // Not a plain number: assume the worst.
            _ => return u16::MAX,
        }
    }
    mask
}

/// Which parameters a translated vertex shader exports (from its `VsOut` struct).
pub(super) fn exported_params(wgsl: &str) -> u32 {
    let mut mask = 0;
    for line in wgsl.lines() {
        if let Some(rest) = line.strip_prefix("@location(") {
            if let Some((number, _)) = rest.split_once(')') {
                if let Ok(index) = number.parse::<u32>() {
                    mask |= 1_u32 << index.min(31);
                }
            }
        }
    }
    mask
}

pub(super) const fn needs_expansion(mode: u32) -> bool {
    matches!(
        mode,
        primitive::TRIANGLE_FAN
            | primitive::RECTS
            | primitive::LINE_LOOP
            | primitive::QUADS
            | primitive::QUAD_STRIP
    )
}

pub(super) fn read_indices(source: &[u8], count: u32, kind: u32, immediate: bool) -> Vec<u32> {
    let count = count as usize;
    let is_32 = matches!(kind, index_type::U32_LE | index_type::U32_BE);
    let big_endian = matches!(kind, index_type::U32_BE | index_type::U16_BE) || immediate;
    if is_32 {
        source
            .as_chunks::<4>()
            .0
            .iter()
            .take(count)
            .map(|b| {
                if big_endian {
                    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
                } else {
                    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
                }
            })
            .collect()
    } else {
        source
            .as_chunks::<2>()
            .0
            .iter()
            .take(count)
            .map(|b| {
                u32::from(if big_endian {
                    u16::from_be_bytes([b[0], b[1]])
                } else {
                    u16::from_le_bytes([b[0], b[1]])
                })
            })
            .collect()
    }
}
