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
//! │ GPU register state │   │ latte/programs.rs        │   │ textures, buffers, │
//! │ draw commands      │──►│ ──────────────────────── │──►│ draws              │
//! │                    │   │ draws → host work:       │   │                    │
//! │                    │   │ programs                 │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "programs" share of latte.rs (draws → host work), moved here so that no file is longer
//! than 500 lines. It works exactly as if it were still written in that file. The core of the
//! GPU emulation: for each draw it gathers the game's shaders, textures and buffers and
//! describes the same draw for your graphics card. Terms such as HLE, JIT or shader are
//! explained in `docs/guide/wiiu_term.md`.

use super::*;

impl Latte {
    /// Everything shader translation depends on, translated once. Only registers
    /// the shaders really read count, the same way: titles leave the rest as it was.
    pub(super) fn program_pair(
        &mut self,
        regs: &RegisterFile,
        vs: (u64, Arc<[u8]>),
        ps: (u64, Arc<[u8]>),
        fetch: (u64, Arc<[u8]>),
    ) -> Arc<ProgramPair> {
        const VS_STAGE: u32 = resource::VS_TEXTURE;
        const PS_STAGE: u32 = resource::PS_TEXTURE;

        let uniform_blocks = regs.get(config::SQ_CONFIG) & (1 << 8) == 0;
        let vs_use = self.vertex_use(vs.0, &vs.1, uniform_blocks);
        let ps_use = self.pixel_use(ps.0, &ps.1, uniform_blocks);
        let attributes = self.fetch_attributes(fetch.0, &fetch.1);
        let vs_signatures = self.used_signatures(regs, VS_STAGE, 18, vs_use.textures);
        let ps_signatures = self.used_signatures(regs, PS_STAGE, 0, ps_use.textures);

        let semantic_table: [u32; 32] =
            std::array::from_fn(|i| regs.get(reg::SQ_VTX_SEMANTIC_0 + i as u32));
        let vs_out_semantics: [u32; 40] = std::array::from_fn(|p| {
            (regs.get(reg::SPI_VS_OUT_ID_0 + (p / 4) as u32) >> ((p % 4) * 8)) & 0xFF
        });
        let control0 = regs.get(reg::SPI_PS_IN_CONTROL_0);
        let control1 = regs.get(reg::SPI_PS_IN_CONTROL_0 + 1);
        let inputs_used = (control0 & 0x3F).min(32);
        let input_controls: [u32; 32] = std::array::from_fn(|i| {
            if (i as u32) < inputs_used { regs.get(reg::SPI_PS_INPUT_CNTL_0 + i as u32) } else { 0 }
        });
        let alpha_control = regs.get(reg::SX_ALPHA_TEST_CONTROL);
        let alpha_function = (alpha_control & 8 != 0).then_some(alpha_control & 7);
        let slots = export_slots(regs.get(reg::CB_SHADER_MASK), ps_use.color_exports);
        let target_kinds: [TextureKind; 8] = std::array::from_fn(|i| {
            let Some(slot) = slots[i] else { return TextureKind::Float };
            let w = &self.color_targets[usize::from(slot)];
            if ps_use.color_exports & (1 << i) != 0 && w.is_valid() {
                host_format(w.format).kind()
            } else {
                TextureKind::Float
            }
        });

        let mut key = std::mem::take(&mut self.key_buffer);
        key.clear();
        key.extend_from_slice(&vs.0.to_le_bytes());
        key.extend_from_slice(&ps.0.to_le_bytes());
        key.extend_from_slice(&fetch.0.to_le_bytes());
        // Which register each attribute lands in.
        for attribute in attributes.iter() {
            let slot = semantic_table.iter().position(|id| id & 0xFF == attribute.semantic);
            key.push(slot.map_or(0xFF, |slot| slot as u8));
        }
        // Which exported parameter each input reads.
        for &control in &input_controls[..inputs_used as usize] {
            key.extend_from_slice(&control.to_le_bytes());
            let parameter = vs_out_semantics.iter().enumerate().find(|(index, id)| {
                **id == control & 0xFF && vs_use.exports & (1 << (*index).min(31)) != 0
            });
            key.push(parameter.map_or(0xFF, |(index, _)| index as u8));
        }
        key.extend_from_slice(&control0.to_le_bytes());
        key.extend_from_slice(&control1.to_le_bytes());
        key.push(alpha_function.map_or(0xFF, |f| f as u8));
        key.push(u8::from(uniform_blocks));
        for kind in target_kinds {
            key.push(kind as u8);
        }
        for signature in vs_signatures.iter().chain(&ps_signatures) {
            key.push(signature.dim as u8);
            key.push(signature.kind as u8);
            key.extend_from_slice(&signature.comp_sel);
            key.push(u8::from(signature.comparison));
            key.push(signature.compare_function);
        }
        let hash = xxh3_64(&key);
        self.key_buffer = key;
        if let Some(pair) = self.programs.get(&hash) {
            return Arc::clone(pair);
        }

        let inputs = PixelInputs {
            control0,
            control1,
            input_controls: &input_controls,
            vs_out_semantics: &vs_out_semantics,
            vs_exports: vs_use.exports,
            alpha_function,
            target_kinds,
        };
        let (pixel, flat_params) = shader::translate_pixel(&ps.1, &inputs, &ps_signatures);
        let vertex = shader::translate_vertex(
            &vs.1,
            &attributes,
            &semantic_table,
            &vs_signatures,
            flat_params,
        );

        let make_bindings = |translated: &shader::TranslatedShader,
                             signatures: &[TextureSignature; TEXTURE_UNITS],
                             stage_base: u32| {
            let mut bindings = Vec::new();
            for &unit in &translated.textures {
                let signature = signatures[usize::from(unit)];
                bindings.push((
                    (stage_base + u32::from(unit)) * 2,
                    BindingKind::Texture { dim: signature.dim, kind: signature.kind },
                ));
            }
            let mut seen = Vec::new();
            for &(unit, _) in &translated.samplers {
                if seen.contains(&unit) {
                    continue;
                }
                seen.push(unit);
                let signature = signatures[usize::from(unit)];
                bindings.push((
                    (stage_base + u32::from(unit)) * 2 + 1,
                    BindingKind::Sampler {
                        comparison: signature.comparison,
                        filtering: signature.kind == TextureKind::Float && !signature.comparison,
                    },
                ));
            }
            bindings
        };
        let (vs_blocks, ps_blocks) =
            (used_uniform_blocks(&vertex.wgsl, "vs"), used_uniform_blocks(&pixel.wgsl, "ps"));
        let (vs_ubo_vec4s, ps_ubo_vec4s) = (vertex.ubo_vec4s, pixel.ubo_vec4s);
        let vs_module = Arc::new(ShaderModule {
            id: self.next_module_id,
            bindings: make_bindings(&vertex, &vs_signatures, 0),
            stream_out: vertex.stream_out,
            wgsl: vertex.wgsl,
        });
        let ps_module = Arc::new(ShaderModule {
            id: self.next_module_id + 1,
            bindings: make_bindings(&pixel, &ps_signatures, TEXTURE_UNITS as u32),
            stream_out: false,
            wgsl: pixel.wgsl,
        });
        self.next_module_id += 2;
        let pair = Arc::new(ProgramPair {
            vertex: vs_module,
            pixel: ps_module,
            vs_samplers: vertex.samplers,
            ps_samplers: pixel.samplers,
            vs_textures: vertex.textures,
            ps_textures: pixel.textures,
            color_exports: pixel.color_exports,
            vs_blocks,
            ps_blocks,
            vs_ubo_vec4s,
            ps_ubo_vec4s,
        });
        self.programs.insert(hash, Arc::clone(&pair));
        pair
    }

    pub(super) fn bind_textures(
        &mut self,
        memory: &Memory,
        regs: &RegisterFile,
        out: &mut Vec<BoundTexture>,
        stage: TextureStage,
        units: &[u8],
        samplers: &[(u8, u8)],
    ) {
        let TextureStage { resource_base: stage_base, binding_base, sampler_base } = stage;
        for &unit in units {
            let sampler_unit = samplers.iter().find(|(t, _)| *t == unit).map(|(_, s)| *s);
            let Some((signature, words)) = self.texture_signature(
                regs,
                stage_base,
                usize::from(unit),
                sampler_base + u32::from(sampler_unit.unwrap_or(unit)),
            ) else {
                continue;
            };
            let format = format_from_resource(words[1], words[4]);
            let address = words[2] << 8;
            let width = ((words[0] >> 19) & 0x1FFF) + 1;
            let height = (words[1] & 0x1FFF) + 1;
            let tile = (words[0] >> 3) & 0xF;
            let hw_dim = words[0] & 7;
            let depth_field = ((words[1] >> 13) & 0x1FFF) + 1;
            let layers = if hw_dim == addrlib::dim::CUBE { depth_field * 6 } else { depth_field };
            let base_level = (words[4] >> 28) & 0xF;
            let last_level = words[5] & 0xF;
            let base_layer = (words[5] >> 4) & 0x1FFF;
            let last_layer = (words[5] >> 17) & 0x1FFF;
            let masked = masked_address(address, tile);
            let host = self.host_surfaces.get(&(masked, width, height)).copied();
            let key = if let Some(key) = host {
                // Drawn in another format of the same texel size: the title reads the same bits
                // in its own format (Xenoblade X's G-buffer under its R11G11B10 light buffer),
                // and the renderer brings them over.
                let wanted = match host_format(format) {
                    HostFormat::Rgba8Srgb => HostFormat::Rgba8Unorm,
                    wanted => wanted,
                };
                if wanted != key.format && same_bits(key.format) && same_bits(wanted) {
                    SurfaceKey { format: wanted, ..key }
                } else {
                    key
                }
            } else {
                let key = SurfaceKey {
                    address: masked,
                    width,
                    height,
                    layers,
                    format: host_format(format),
                };
                let pixel_pitch = ((words[0] >> 8) & 0x7FF).wrapping_add(1) << 3;
                let surface = SurfaceWords {
                    address,
                    width,
                    height,
                    depth: layers,
                    pitch: if addrlib::is_compressed(format & 0x3F) {
                        pixel_pitch / 4
                    } else {
                        pixel_pitch
                    },
                    tile_mode: tile,
                    swizzle: address & 0x700,
                    format,
                    aa: 0,
                    level: 0,
                    slice: 0,
                };
                let mip_count = (last_level + 1).clamp(1, 16);
                self.upload_texture(memory, key, &surface, signature.dim, mip_count, words[3] << 8);
                key
            };
            let is_multi_level = host.is_none();
            let mut sampler =
                sampler_unit.map(|s| read_sampler(regs, sampler_base + u32::from(s), signature));
            if let Some(sampler) = sampler.as_mut().filter(|_| !self.sampler_rules.is_empty()) {
                let desc = wiiu_graphic_pack::TextureDesc {
                    width,
                    height,
                    depth: depth_field,
                    format,
                    tile_mode: tile,
                    in_mem1: address < wiiu_mem_addr::MEM1.size,
                };
                let overwrite = wiiu_graphic_pack::overwrite_for(&self.sampler_rules, &desc);
                // As Cemu's samplers: the pack's level replaces the title's anisotropy.
                if let Some(level) = overwrite.anisotropy {
                    let filtered = sampler.mag_linear && sampler.min_linear && sampler.max_lod > 0;
                    sampler.anisotropy = if level > 0 && filtered && sampler.compare.is_none() {
                        1 << level.min(4)
                    } else {
                        0
                    };
                }
            }
            out.push(BoundTexture {
                binding: (binding_base + u32::from(unit)) * 2,
                key,
                dim: signature.dim,
                base_level: if is_multi_level { base_level } else { 0 },
                level_count: if is_multi_level {
                    (last_level + 1).saturating_sub(base_level).max(1)
                } else {
                    1
                },
                base_layer: if is_multi_level { base_layer } else { 0 },
                layer_count: (last_layer + 1).saturating_sub(base_layer).max(1),
                sampler,
            });
        }
    }

    /// Stores the uniform block of unit `index` and returns its arena offset. Only the
    /// `vec4s` the shaders read from its start are stored: titles bind blocks of kilobytes
    /// and read a few rows of them (Breath of the Wild hashed and copied 21 MB a frame).
    pub(super) fn store_uniform_block(
        &mut self,
        memory: &Memory,
        regs: &RegisterFile,
        stage_base: u32,
        index: u32,
        vec4s: u32,
    ) -> u32 {
        let start = base::RESOURCE + stage_base + index * 7;
        let (physical, size) = (regs.get(start), regs.get(start + 1).wrapping_add(1));
        if physical == 0 || size == 0 || size > 0x10_0000 {
            return 0;
        }
        let size = size.min(vec4s.saturating_mul(16));
        if size == 0 {
            return 0;
        }
        // Stream-out results only exist on the host: Xenoblade X streams out its matrices and
        // binds them as a uniform block.
        if let Some(offset) = self.streamed_attribute(memory, physical) {
            return offset;
        }
        let Some(effective) = wiiu_mem_addr::physical_to_effective(physical) else { return 0 };
        let Ok(bytes) = memory.slice(effective, size) else { return 0 };
        // Titles endian-swap uniform blocks themselves (GX2EndianSwap): the GPU reads them as they are.
        // Copied whatever the cache says, like Cemu does: titles rewrite them without flushing.
        self.arena_store(bytes, 16)
    }

    /// Where a stream-out buffer lives in the stream region, reserving room the first time.
    pub(super) fn stream_buffer(&mut self, physical: u32, size: u32) -> Option<usize> {
        if let Some(index) =
            self.stream_buffers.iter().position(|b| b.physical == physical && b.size == size)
        {
            return Some(index);
        }
        let aligned = size.next_multiple_of(256);
        if aligned > STREAM_REGION_SIZE {
            log::warn!("stream-out buffer of {size:#x} bytes does not fit");
            return None;
        }
        if self.stream_cursor + aligned > STREAM_REGION_SIZE {
            // Full: the oldest buffers give way.
            self.stream_buffers.clear();
            self.stream_cursor = 0;
        }
        let offset = self.stream_cursor;
        self.stream_cursor += aligned;
        self.stream_buffers.push(StreamBuffer { physical, size, offset, guest_hash: 0 });
        Some(self.stream_buffers.len() - 1)
    }

    /// Arena offset of attribute or uniform data the GPU produced itself, unless the CPU replaced it.
    pub(super) fn streamed_attribute(&mut self, memory: &Memory, physical: u32) -> Option<u32> {
        let index = self
            .stream_buffers
            .iter()
            .position(|b| physical >= b.physical && physical - b.physical < b.size)?;
        let buffer = self.stream_buffers[index];
        let effective = wiiu_mem_addr::physical_to_effective(buffer.physical)?;
        let guest = memory.slice(effective, buffer.size).ok()?;
        if sampled_hash(guest) != buffer.guest_hash {
            self.stream_buffers.remove(index);
            return None;
        }
        Some(STREAM_REGION_BASE + buffer.offset + (physical - buffer.physical))
    }

    /// Sets up the stream-out writes of a draw of `vertices` vertices per instance.
    pub(super) fn stream_out_setup(
        &mut self,
        memory: &Memory,
        regs: &mut RegisterFile,
        params: &mut [u32],
        vertices: u32,
        instances: u32,
        base_vertex: u32,
    ) -> Vec<StreamCopy> {
        let mut copies = Vec::new();
        params[80] = vertices;
        for index in 0..4 {
            let at = |register: u32| register + index * 4;
            let stride = regs.get(at(reg::VGT_STRMOUT_VTX_STRIDE_0));
            let physical = regs.get(at(reg::VGT_STRMOUT_BUFFER_BASE_0)) << 8;
            let size = regs.get(at(reg::VGT_STRMOUT_BUFFER_SIZE_0)) << 2;
            let written = regs.get(at(reg::VGT_STRMOUT_BUFFER_OFFSET_0));
            if stride == 0 || physical == 0 || size == 0 {
                continue;
            }
            let Some(buffer) = self.stream_buffer(physical, size) else { continue };
            let guest_hash = wiiu_mem_addr::physical_to_effective(physical)
                .and_then(|effective| memory.slice(effective, size).ok())
                .map_or(0, sampled_hash);
            self.stream_buffers[buffer].guest_hash = guest_hash;
            let start = self.stream_buffers[buffer].offset + written.min(size);
            let total = u64::from(vertices) * u64::from(instances) * u64::from(stride) * 4;
            let range = total.min(u64::from(size - written.min(size))) as u32;
            // The shader indexes by vertex number, which includes the base vertex.
            params[72 + index as usize] =
                (start / 4).wrapping_sub(base_vertex.wrapping_mul(stride));
            params[76 + index as usize] = stride;
            if range > 0 {
                copies.push(StreamCopy { offset: start, size: range });
            }
            regs.set(at(reg::VGT_STRMOUT_BUFFER_OFFSET_0), written.wrapping_add(range));
        }
        copies
    }
}
