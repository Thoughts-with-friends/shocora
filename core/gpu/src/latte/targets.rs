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
//! │ GPU register state │   │ latte/targets.rs         │   │ textures, buffers, │
//! │ draw commands      │──►│ ──────────────────────── │──►│ draws              │
//! │                    │   │ draws → host work:       │   │                    │
//! │                    │   │ targets                  │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "targets" share of latte.rs (draws → host work), moved here so that no file is longer
//! than 500 lines. It works exactly as if it were still written in that file. The core of the
//! GPU emulation: for each draw it gathers the game's shaders, textures and buffers and
//! describes the same draw for your graphics card. Terms such as HLE, JIT or shader are
//! explained in `docs/guide/wiiu_term.md`.

use super::*;

impl Latte {
    pub(super) fn note_target(&mut self, key: SurfaceKey) {
        self.host_surfaces.insert((key.address, key.width, key.height), key);
    }

    pub fn clear(
        &mut self,
        flags: u32,
        color: Option<&SurfaceWords>,
        depth: Option<&SurfaceWords>,
        rgba: [f32; 4],
        depth_value: f32,
        stencil: u32,
    ) {
        use crate::command::opcode::clear;
        let color_clear = color.filter(|w| flags & clear::COLOR != 0 && w.is_valid()).map(|w| {
            let key = Self::key_of_target(w, false);
            self.note_target(key);
            (key, w.slice, rgba)
        });
        let depth_clear = depth
            .filter(|w| flags & (clear::DEPTH | clear::STENCIL) != 0 && w.is_valid())
            .map(|w| {
                let key = Self::key_of_target(w, true);
                self.note_target(key);
                DepthClear {
                    surface: key,
                    slice: w.slice,
                    depth: (flags & clear::DEPTH != 0).then_some(depth_value),
                    stencil: (flags & clear::STENCIL != 0 && key.format.has_stencil())
                        .then_some(stencil),
                }
            });
        self.commands.push(RenderCommand::Clear { color: color_clear, depth: depth_clear });
    }

    /// Uses the `[TextureRedefine]` rules of the enabled graphic packs.
    pub fn set_texture_rules(&mut self, rules: Vec<wiiu_graphic_pack::TextureRule>) {
        self.sampler_rules =
            rules.iter().filter(|r| r.overwrite_anisotropy != -1).cloned().collect();
        self.texture_rules = rules;
        self.update_resolution();
    }

    /// The scale a graphic pack asks for: its rule for targets as wide as the TV picture,
    /// rounded up to a whole factor (shocora draws every scaled target at one factor;
    /// Cemu sizes each target as the rule says).
    pub(super) fn pack_factor(&self) -> u32 {
        let width = self.tv_width;
        let factor = self
            .texture_rules
            .iter()
            .filter(|r| r.width == width.cast_signed() && r.overwrite_width > 0)
            .map(|r| r.overwrite_width.cast_unsigned().div_ceil(width.max(1)))
            .max()
            .unwrap_or(1);
        factor.clamp(1, *ResolutionScale::FACTORS.last().unwrap_or(&1))
    }

    #[inline]
    pub fn set_resolution_factor(&mut self, factor: u32) {
        self.requested_factor = factor.max(1);
        self.update_resolution();
    }

    /// Tells the renderer if the scale changed. It takes the TV's width to know, so a title
    /// starts out at its own resolution until it shows its first picture.
    pub(super) fn update_resolution(&mut self) {
        if self.tv_width == 0 {
            return;
        }
        let factor = self.requested_factor.max(self.pack_factor());
        let new = ResolutionScale::for_tv(factor, self.tv_width);
        if new != self.resolution {
            self.resolution = new;
            self.commands.push(RenderCommand::SetResolution(new));
        }
    }

    /// `srgb`: the scan buffer has an sRGB format, so the copy encodes the colour.
    pub fn copy_to_scan(&mut self, words: &SurfaceWords, screen: crate::Screen, srgb: bool) {
        if screen == crate::Screen::Tv && words.width != self.tv_width {
            self.tv_width = words.width;
            self.update_resolution();
        }
        let key = Self::key_of_target(words, false);
        self.commands.push(RenderCommand::CopyToScanBuffer {
            surface: key,
            slice: words.slice,
            screen,
            srgb,
        });
    }

    pub fn copy_surface(&mut self, memory: &Memory, source: &SurfaceWords, target: &SurfaceWords) {
        let source_key = self.surface_for_read(memory, source);
        let mut target_key = Self::key_of_target(target, false);
        target_key.layers = target.depth.max(1);
        self.note_target(target_key);
        self.commands.push(RenderCommand::CopySurface {
            source: source_key,
            source_level: 0,
            source_slice: source.slice,
            target: target_key,
            target_level: target.level,
            target_slice: target.slice,
        });
    }

    /// The host surface for `words`: a known render target, or a fresh upload.
    pub(super) fn surface_for_read(&mut self, memory: &Memory, words: &SurfaceWords) -> SurfaceKey {
        let address = masked_address(words.address, words.tile_mode);
        if let Some(key) = self.host_surfaces.get(&(address, words.width, words.height)) {
            return *key;
        }
        let key = SurfaceKey {
            address,
            width: words.width,
            height: words.height,
            layers: words.depth.max(1),
            format: host_format(words.format),
        };
        self.upload_texture(memory, key, words, TextureDim::D2, 1, words.address);
        key
    }

    /// Uploads the texture when its guest memory changed.
    pub(super) fn upload_texture(
        &mut self,
        memory: &Memory,
        key: SurfaceKey,
        words: &SurfaceWords,
        dim: TextureDim,
        mip_count: u32,
        mip_base: u32,
    ) {
        let frame = self.frame;
        let known = self.textures.get(&key).map(|k| (k.last_check, k.stamp, k.hashed));
        if let Some((last_check, _, _)) = known
            && frame.wrapping_sub(last_check) < TEXTURE_RECHECK_FRAMES
        {
            return;
        }
        let info = addrlib::gx2_surface_info(
            words.format,
            words.width,
            words.height,
            words.depth.max(1),
            gx2_dim(dim),
            words.tile_mode,
            words.aa,
            0,
            false,
            false,
        );
        let size = (info.surf_size as u32).max(1);
        let Some(effective) = wiiu_mem_addr::physical_to_effective(words.address) else { return };
        let Ok(base) = memory.slice(
            masked_address(effective, words.tile_mode) & !0xFF,
            size.saturating_add(0xFF).min(0x0400_0000),
        ) else {
            log::debug!("texture at {:#010x} outside mapped memory", words.address);
            return;
        };
        let chain = (mip_count > 1 && mip_base != 0).then(|| {
            let effective = wiiu_mem_addr::physical_to_effective(mip_base).unwrap_or(0);
            (
                masked_address(effective, words.tile_mode) & !0xFF,
                mip_chain_size(words, dim, mip_count),
            )
        });
        let base_start = masked_address(effective, words.tile_mode) & !0xFF;
        let untouched = |stamp: u32| {
            self.writes.unchanged_since(base_start, base.len() as u32, stamp)
                && chain.is_none_or(|(start, size)| {
                    start == 0 || self.writes.unchanged_since(start, size.min(0x0400_0000), stamp)
                })
        };
        if self.buffer_cache
            && let Some((_, stamp, hashed)) = known
            && frame.wrapping_sub(hashed) < TEXTURE_TRUST_FRAMES
            && untouched(stamp)
        {
            if let Some(known) = self.textures.get_mut(&key) {
                known.last_check = frame;
            }
            return;
        }
        let stamp = self.writes.now;
        // Every byte is hashed. A sampled hash missed partial updates: BotW streams the
        // near terrain's height and material data into textures in place, and stale ones drew
        // the ground dark and too high (Link's feet sank into it); Mario Tennis draws glyphs
        // into a font cache one by one (blank or wrong letters). Measured cost: none visible
        // in a BotW field run (226 s against 230 s with sampling).
        let hash_of = xxh3_64;
        let mut hash = hash_of(base);
        // The smaller levels live elsewhere and are often streamed in after the largest:
        // looking only at that one kept garbage mips, seen as black or striped ground
        // whenever the camera moved (BotW).
        if mip_count > 1 && mip_base != 0 {
            let chain = mip_chain_size(words, dim, mip_count);
            let mips = wiiu_mem_addr::physical_to_effective(mip_base).and_then(|effective| {
                memory
                    .slice(
                        masked_address(effective, words.tile_mode) & !0xFF,
                        chain.min(0x0400_0000),
                    )
                    .ok()
            });
            if let Some(mips) = mips {
                hash ^= hash_of(mips).rotate_left(17);
            }
        }
        if let Some(known) = self.textures.get_mut(&key) {
            known.last_check = frame;
            known.stamp = stamp;
            known.hashed = frame;
            if known.content_hash == hash {
                return;
            }
        }
        let mips = decode_mips(memory, words, dim, mip_count, mip_base);
        self.textures.insert(
            key,
            KnownTexture { content_hash: hash, last_check: frame, stamp, hashed: frame },
        );
        self.commands.push(RenderCommand::Upload(Box::new(TextureUpload { key, dim, mips })));
    }

    pub(super) fn cached_code(
        &mut self,
        memory: &Memory,
        physical: u32,
        size: u32,
    ) -> Option<(u64, Arc<[u8]>)> {
        if size == 0 {
            return None;
        }
        let frame = self.frame;
        if let Some((hash, code, seen)) = self.code_cache.get_mut(&(physical, size)) {
            if *seen == frame {
                return Some((*hash, Arc::clone(code)));
            }
        }
        let effective = wiiu_mem_addr::physical_to_effective(physical)?;
        let bytes = memory.slice(effective, size).ok()?;
        let hash = xxh3_64(bytes);
        let code: Arc<[u8]> = match self.code_cache.get(&(physical, size)) {
            Some((old_hash, code, _)) if *old_hash == hash => Arc::clone(code),
            _ => Arc::from(bytes),
        };
        self.code_cache.insert((physical, size), (hash, Arc::clone(&code), frame));
        Some((hash, code))
    }

    /// Reads the seven dwords of texture unit `unit`.
    pub(super) fn texture_signature(
        &self,
        regs: &RegisterFile,
        stage_base: u32,
        unit: usize,
        samplers: u32,
    ) -> Option<(TextureSignature, [u32; 7])> {
        let start = base::RESOURCE + stage_base + unit as u32 * 7;
        let words: [u32; 7] = std::array::from_fn(|i| regs.get(start + i as u32));
        if words[2] == 0 {
            return None;
        }
        let format = format_from_resource(words[1], words[4]);
        let address = words[2] << 8;
        let width = ((words[0] >> 19) & 0x1FFF) + 1;
        let height = (words[1] & 0x1FFF) + 1;
        let tile = (words[0] >> 3) & 0xF;
        let host_surface = self.host_surfaces.get(&(masked_address(address, tile), width, height));
        let mut kind =
            host_surface.map_or_else(|| host_format(format).kind(), |key| key.format.kind());
        let dim = dim_of(words[0] & 7);
        let sampler_word0 = regs.get(base::SAMPLER + samplers * 3);
        let comparison = (sampler_word0 >> 26) & 7 != 0 && kind == TextureKind::Depth;
        if kind == TextureKind::Depth && dim == TextureDim::D3 {
            kind = TextureKind::Float;
        }
        let comp = |shift: u32| ((words[4] >> shift) & 7) as u8;
        Some((
            TextureSignature {
                dim,
                kind,
                comp_sel: [comp(16), comp(19), comp(22), comp(25)],
                comparison,
                compare_function: ((sampler_word0 >> 26) & 7) as u8,
            },
            words,
        ))
    }

    /// What the vertex shader `code` uses, found out once.
    pub(super) fn vertex_use(&mut self, hash: u64, code: &[u8], uniform_blocks: bool) -> ShaderUse {
        *self.shader_uses.entry((hash, uniform_blocks, false)).or_insert_with(|| {
            let probe = shader::translate_vertex(
                code,
                &[],
                &[],
                &[TextureSignature::default(); TEXTURE_UNITS],
                0,
            );
            ShaderUse {
                textures: unit_mask(&probe.textures),
                exports: exported_params(&probe.wgsl),
                color_exports: 0,
            }
        })
    }

    /// What the pixel shader `code` uses, found out once.
    pub(super) fn pixel_use(&mut self, hash: u64, code: &[u8], uniform_blocks: bool) -> ShaderUse {
        *self.shader_uses.entry((hash, uniform_blocks, true)).or_insert_with(|| {
            let inputs = PixelInputs {
                control0: 0,
                control1: 0,
                input_controls: &[],
                vs_out_semantics: &[],
                vs_exports: 0,
                alpha_function: None,
                target_kinds: [TextureKind::Float; 8],
            };
            let (probe, _) = shader::translate_pixel(
                code,
                &inputs,
                &[TextureSignature::default(); TEXTURE_UNITS],
            );
            ShaderUse {
                textures: unit_mask(&probe.textures),
                exports: 0,
                color_exports: probe.color_exports,
            }
        })
    }

    /// The attributes of a fetch shader, parsed once.
    pub(super) fn fetch_attributes(
        &mut self,
        hash: u64,
        code: &[u8],
    ) -> Arc<Vec<shader::Attribute>> {
        Arc::clone(
            self.fetch_cache
                .entry(hash)
                .or_insert_with(|| Arc::new(shader::parse_fetch_shader(code))),
        )
    }

    /// Signatures of the units in `used`. The others stay default: they hold whatever
    /// an earlier draw left there, and telling those apart would translate the same
    /// program over and over.
    pub(super) fn used_signatures(
        &self,
        regs: &RegisterFile,
        stage_base: u32,
        sampler_base: u32,
        used: u32,
    ) -> [TextureSignature; TEXTURE_UNITS] {
        let mut signatures = [TextureSignature::default(); TEXTURE_UNITS];
        for (unit, signature) in signatures.iter_mut().enumerate() {
            if used & (1 << unit) != 0 {
                if let Some((found, _)) =
                    self.texture_signature(regs, stage_base, unit, sampler_base + unit as u32)
                {
                    *signature = found;
                }
            }
        }
        signatures
    }
}
