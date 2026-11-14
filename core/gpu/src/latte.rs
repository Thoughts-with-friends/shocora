//! GPU: AMD Radeon, package codenamed "Latte".
//! Core clock is 549.999 MHz.
//! Turns register state into host work: render targets, shaders, textures,
//! vertex data and pipeline state of a draw.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    clippy::cast_precision_loss
)]

use std::{collections::HashMap, sync::Arc};

use wiiu_memory::Memory;
use xxhash_rust::xxh3::xxh3_64;

use crate::{
    addrlib::{self, tile_mode},
    command::opcode::SURFACE_WORDS,
    regs::{RegisterFile, base, config, context as reg, resource},
    render::{
        AddressMode, BindingKind, BlendState, BorderColor, BoundTexture, DepthClear, DrawCommand,
        HostFormat, MipData, PipelineState, RenderCommand, ResolutionScale, SamplerDesc,
        ShaderModule, StencilFace, StreamCopy, SurfaceKey, TextureUpload, Topology,
    },
    shader::{self, PixelInputs, TEXTURE_UNITS, TextureDim, TextureKind, TextureSignature},
    tiling::{Layout, detile, swizzle_from_address},
};

/// Size of the arena every vertex, index and uniform byte goes through.
pub const ARENA_SIZE: u32 = 0x1000_0000;
/// The end of the arena holds stream-out results, which outlive the rest.
pub const STREAM_REGION_SIZE: u32 = 0x400_0000;
/// Where the stream region starts.
pub const STREAM_REGION_BASE: u32 = ARENA_SIZE - STREAM_REGION_SIZE;
/// Alignment of draw parameter blocks (`min_storage_buffer_offset_alignment`).
const PARAMS_ALIGN: u32 = 256;
/// Size of `DrawParams` in the shader prelude.
/// Bytes of the draw parameters the shader prelude reads (`DrawParams`).
pub const PARAMS_SIZE: usize = 32 + 4 * 16 * 4 + 36 + 16 + 4 * PS_SAMPLERS;
/// Pixel shader sampler units, whose LOD bias the shaders read from the draw parameters.
const PS_SAMPLERS: usize = 18;
/// First draw parameter word of the pixel samplers' LOD biases.
const PARAM_PS_LOD_BIAS: usize = 85;
/// Texture contents are re-checked once per this many frames at most.
const TEXTURE_RECHECK_FRAMES: u64 = 2;
/// Frames a vertex or uniform buffer is trusted without looking at it again, unless the
/// title flushes its memory out of the CPU's cache. A safety net: titles do flush.
const BUFFER_RECHECK_FRAMES: u64 = 30;
/// Granularity of [`WriteTracker`].
const TRACK_PAGE_BITS: u32 = 12;
/// Frames without use after which a texture is forgotten, as in Cemu's texture cache. Only
/// what guest memory can restore is ever forgotten, never what the GPU drew.
const TEXTURE_EXPIRE_FRAMES: u64 = 2000;
/// How often to look for such textures.
const TEXTURE_SWEEP_FRAMES: u64 = 120;
/// Textures forgotten at a time: making room should not cost a stutter (Cemu does the same).
const MAX_TEXTURES_FORGOTTEN: usize = 32;
/// Bytes hashed per step when checking whether a texture changed.
const HASH_STRIDE: usize = 64;

/// Primitive modes of `VGT_PRIMITIVE_TYPE`.
mod primitive {
    pub const POINTS: u32 = 1;
    pub const LINES: u32 = 2;
    pub const LINE_STRIP: u32 = 3;
    pub const TRIANGLE_FAN: u32 = 5;
    pub const TRIANGLE_STRIP: u32 = 6;
    pub const RECTS: u32 = 0x11;
    pub const LINE_LOOP: u32 = 0x12;
    pub const QUADS: u32 = 0x13;
    pub const QUAD_STRIP: u32 = 0x14;
}

/// `VGT_DMA_INDEX_TYPE` values GX2 uses.
pub mod index_type {
    pub const U16_LE: u32 = 0;
    pub const U32_LE: u32 = 1;
    pub const U16_BE: u32 = 4;
    pub const U32_BE: u32 = 9;
}

/// The surface descriptor of the HLE packets.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SurfaceWords {
    pub address: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub pitch: u32,
    pub tile_mode: u32,
    pub swizzle: u32,
    pub format: u32,
    pub aa: u32,
    pub level: u32,
    pub slice: u32,
}

impl SurfaceWords {
    pub fn parse(words: &[u32]) -> Option<Self> {
        let w: &[u32; SURFACE_WORDS] = words.get(..SURFACE_WORDS)?.try_into().ok()?;
        Some(Self {
            address: w[0],
            width: w[1],
            height: w[2],
            depth: w[3],
            pitch: w[4],
            tile_mode: w[5],
            swizzle: w[6],
            format: w[7],
            aa: w[8],
            level: w[9] >> 16,
            slice: w[9] & 0xFFFF,
        })
    }

    #[inline]
    pub const fn is_valid(&self) -> bool {
        self.address != 0 && self.width != 0 && self.height != 0
    }
}

/// `E_GX2SURFFMT` → host format.
pub fn host_format(gx2_format: u32) -> HostFormat {
    use HostFormat as H;
    match gx2_format {
        0x001 => H::R8Unorm,
        0x201 => H::R8Snorm,
        0x101 => H::R8Uint,
        0x301 => H::R8Sint,
        0x002 | 0x007 => H::Rg8Unorm,
        0x207 => H::Rg8Snorm,
        0x107 => H::Rg8Uint,
        0x307 => H::Rg8Sint,
        // These are widened on upload.
        0x008..=0x00C => H::Rgba8Unorm,
        0x01A => H::Rgba8Unorm,
        0x21A => H::Rgba8Snorm,
        0x11A => H::Rgba8Uint,
        0x31A => H::Rgba8Sint,
        0x41A => H::Rgba8Srgb,
        0x005 => H::R16Unorm,
        0x105 => H::R16Uint,
        0x305 => H::R16Sint,
        0x806 => H::R16Float,
        0x00F => H::Rg16Unorm,
        0x10F => H::Rg16Uint,
        0x30F => H::Rg16Sint,
        0x810 => H::Rg16Float,
        0x01F => H::Rgba16Unorm,
        0x21F => H::Rgba16Snorm,
        0x11F => H::Rgba16Uint,
        0x31F => H::Rgba16Sint,
        0x820 => H::Rgba16Float,
        0x10D => H::R32Uint,
        0x30D => H::R32Sint,
        0x80E => H::R32Float,
        // `T_R24_UNORM_X8` and `T_R24_FLOAT_X8`: depth read as a texture, its value in red.
        0x011 | 0x811 => H::R32Float,
        0x11D => H::Rg32Uint,
        0x31D => H::Rg32Sint,
        0x81E => H::Rg32Float,
        0x122 => H::Rgba32Uint,
        0x322 => H::Rgba32Sint,
        0x823 => H::Rgba32Float,
        0x019 | 0x01B | 0x419 => H::Rgb10a2Unorm,
        0x816 => H::Rg11b10Float,
        0x031 => H::Bc1Unorm,
        0x431 => H::Bc1Srgb,
        0x032 => H::Bc2Unorm,
        0x432 => H::Bc2Srgb,
        0x033 => H::Bc3Unorm,
        0x433 => H::Bc3Srgb,
        0x034 => H::Bc4Unorm,
        0x234 => H::Bc4Snorm,
        0x035 => H::Bc5Unorm,
        0x235 => H::Bc5Snorm,
        other => {
            log::debug!("unmapped surface format {other:#05x}");
            H::Rgba8Unorm
        }
    }
}

/// `E_GX2SURFFMT` of a depth buffer → host format.
pub fn host_depth_format(gx2_format: u32) -> HostFormat {
    match gx2_format {
        0x005 => HostFormat::Depth16Unorm,
        0x011 => HostFormat::Depth24PlusStencil8,
        0x806 | 0x80E => HostFormat::Depth32Float,
        _ => HostFormat::Depth32FloatStencil8,
    }
}

/// Reconstructs `E_GX2SURFFMT` from `SQ_TEX_RESOURCE_WORD1/4`.
pub const fn format_from_resource(word1: u32, word4: u32) -> u32 {
    let hw = (word1 >> 26) & 0x3F;
    let mut format = hw;
    if word4 & 1 != 0 {
        format |= 0x200;
    }
    match (word4 >> 8) & 3 {
        1 => format |= 0x100,
        2 => format |= 0x800,
        _ => {}
    }
    if (word4 >> 11) & 1 != 0 {
        format |= 0x400;
    }
    format
}

/// Widens guest texel formats the host lacks; `None` means bytes pass through.
fn widen_texels(gx2_format: u32, data: &[u8]) -> Option<Vec<u8>> {
    let scale5 = |v: u16| ((u32::from(v) * 255 + 15) / 31) as u8;
    let scale6 = |v: u16| ((u32::from(v) * 255 + 31) / 63) as u8;
    let scale4 = |v: u16| (v * 17) as u8;
    let pixels16 = || data.as_chunks::<2>().0.iter().map(|p| u16::from_le_bytes([p[0], p[1]]));
    match gx2_format & 0xFF {
        // R in the low bits (see Cemu's "Vulkan has R in MSB, GPU7 has it in LSB").
        0x08 => Some(
            pixels16()
                .flat_map(|v| [scale5(v & 0x1F), scale6((v >> 5) & 0x3F), scale5(v >> 11), 255])
                .collect(),
        ),
        0x09 => Some(
            pixels16()
                .flat_map(|v| [scale6(v & 0x3F), scale5((v >> 6) & 0x1F), scale5(v >> 11), 255])
                .collect(),
        ),
        0x0A => Some(
            pixels16()
                .flat_map(|v| {
                    [
                        scale5(v & 0x1F),
                        scale5((v >> 5) & 0x1F),
                        scale5((v >> 10) & 0x1F),
                        if v >> 15 != 0 { 255 } else { 0 },
                    ]
                })
                .collect(),
        ),
        0x0C => Some(
            pixels16()
                .flat_map(|v| {
                    [
                        scale5(v >> 11),
                        scale5((v >> 6) & 0x1F),
                        scale5((v >> 1) & 0x1F),
                        if v & 1 != 0 { 255 } else { 0 },
                    ]
                })
                .collect(),
        ),
        0x0B => Some(
            pixels16()
                .flat_map(|v| {
                    [
                        scale4(v & 0xF),
                        scale4((v >> 4) & 0xF),
                        scale4((v >> 8) & 0xF),
                        scale4(v >> 12),
                    ]
                })
                .collect(),
        ),
        0x02 => Some(data.iter().flat_map(|v| [(v & 0xF) * 17, (v >> 4) * 17]).collect()),
        0x1B => Some(
            data.as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| {
                    let v = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
                    let swapped = (v >> 22)
                        | (((v >> 12) & 0x3FF) << 10)
                        | (((v >> 2) & 0x3FF) << 20)
                        | ((v & 3) << 30);
                    swapped.to_le_bytes()
                })
                .collect(),
        ),
        _ => None,
    }
}

fn dim_of(gx2_dim: u32) -> TextureDim {
    match gx2_dim {
        addrlib::dim::D3 => TextureDim::D3,
        addrlib::dim::CUBE => TextureDim::Cube,
        addrlib::dim::D1_ARRAY | addrlib::dim::D2_ARRAY | addrlib::dim::D2_ARRAY_MSAA => {
            TextureDim::D2Array
        }
        _ => TextureDim::D2,
    }
}

/// Copy of a guest buffer in the arena.
#[derive(Debug, Clone, Copy)]
struct ArenaSlice {
    offset: u32,
}

/// Where the CPU wrote memory for the GPU to read, as the title tells it by flushing its
/// cache (`DCFlushRange`, `GX2Invalidate`, ...). Cemu's buffer cache learns of writes the
/// same way, and only then looks at a buffer again.
#[derive(Debug, Default)]
struct WriteTracker {
    /// Per page, the [`now`](Self::now) of the last write; empty before the first one.
    stamps: Vec<u32>,
    now: u32,
}

impl WriteTracker {
    fn note(&mut self, address: u32, size: u32) {
        if size == 0 {
            return;
        }
        if self.stamps.is_empty() {
            self.stamps = vec![0; 1 << (32 - TRACK_PAGE_BITS)];
        }
        self.now = self.now.saturating_add(1);
        let first = (address >> TRACK_PAGE_BITS) as usize;
        let last = (address.saturating_add(size - 1) >> TRACK_PAGE_BITS) as usize;
        self.stamps[first..=last].fill(self.now);
    }

    /// Whether nothing in `[address, address + size)` was written after `stamp`.
    fn unchanged_since(&self, address: u32, size: u32, stamp: u32) -> bool {
        if self.stamps.is_empty() || size == 0 {
            return true;
        }
        let first = (address >> TRACK_PAGE_BITS) as usize;
        let last = (address.saturating_add(size - 1) >> TRACK_PAGE_BITS) as usize;
        self.stamps[first..=last].iter().all(|&written| written <= stamp)
    }
}

/// A guest buffer copied into the arena, and when that was known to be up to date.
#[derive(Debug, Clone, Copy)]
struct GuestBuffer {
    offset: u32,
    stamp: u32,
    checked: u64,
}

#[derive(Debug, Clone)]
struct KnownTexture {
    content_hash: u64,
    last_check: u64,
}

/// A guest buffer written by stream-out, whose contents only the host has.
#[derive(Debug, Clone, Copy)]
struct StreamBuffer {
    physical: u32,
    size: u32,
    /// Byte offset in the stream region.
    offset: u32,
    /// Guest bytes when last streamed to: a change means the CPU wrote new data there.
    guest_hash: u64,
}

/// A program that has been translated, by source hash and dependencies.
#[derive(Debug)]
struct ProgramPair {
    vertex: Arc<ShaderModule>,
    pixel: Arc<ShaderModule>,
    vs_samplers: Vec<(u8, u8)>,
    ps_samplers: Vec<(u8, u8)>,
    vs_textures: Vec<u8>,
    ps_textures: Vec<u8>,
    color_exports: u8,
    /// Uniform blocks each stage reads, as bit masks: the others are not worth storing.
    vs_blocks: u16,
    ps_blocks: u16,
}

/// GPU-side caches and what they need to stay coherent with the renderer.
#[derive(Debug)]
pub struct Latte {
    pub commands: Vec<RenderCommand>,
    /// Bound render targets as declared by GX2.
    pub color_targets: [SurfaceWords; 8],
    pub depth_target: SurfaceWords,
    frame: u64,
    arena_cursor: u32,
    arena_entries: HashMap<u64, ArenaSlice>,
    /// Guest buffers in the arena by `(address, size, alignment)`: no need to hash them again.
    guest_buffers: HashMap<(u32, u32, u32), GuestBuffer>,
    /// Converted index buffers: converting them on every draw cost as much as the draw.
    index_buffers: HashMap<IndexKey, HostIndices>,
    writes: WriteTracker,
    /// Whether guest buffers are trusted until written (`SHOCORA_NO_BUFFER_CACHE` turns it off).
    buffer_cache: bool,
    /// Surfaces rendered to: `(masked address, width, height)` → key.
    host_surfaces: HashMap<(u32, u32, u32), SurfaceKey>,
    /// Factor the user asked for.
    requested_factor: u32,
    /// Width of the TV picture, once the title has shown one.
    tv_width: u32,
    /// What the renderer was told last.
    resolution: ResolutionScale,
    textures: HashMap<SurfaceKey, KnownTexture>,
    programs: HashMap<u64, Arc<ProgramPair>>,
    /// By program hash, uniform blocks in use, and pixel stage or not.
    shader_uses: HashMap<(u64, bool, bool), ShaderUse>,
    fetch_cache: HashMap<u64, Arc<Vec<shader::Attribute>>>,
    /// Kept between draws: every draw builds a key in it.
    key_buffer: Vec<u8>,
    /// Hash of a shader program, valid for a guest address.
    code_cache: HashMap<(u32, u32), CachedCode>,
    next_module_id: u64,
    stream_buffers: Vec<StreamBuffer>,
    stream_cursor: u32,
    pub skipped_draws: u64,
}

impl Default for Latte {
    fn default() -> Self {
        Self {
            commands: Vec::new(),
            color_targets: [SurfaceWords::default(); 8],
            depth_target: SurfaceWords::default(),
            frame: 0,
            arena_cursor: 0,
            arena_entries: HashMap::new(),
            guest_buffers: HashMap::new(),
            index_buffers: HashMap::new(),
            writes: WriteTracker::default(),
            buffer_cache: std::env::var_os("SHOCORA_NO_BUFFER_CACHE").is_none(),
            host_surfaces: HashMap::new(),
            requested_factor: 1,
            tv_width: 0,
            resolution: ResolutionScale::default(),
            textures: HashMap::new(),
            programs: HashMap::new(),
            shader_uses: HashMap::new(),
            fetch_cache: HashMap::new(),
            key_buffer: Vec::new(),
            code_cache: HashMap::new(),
            next_module_id: 1,
            stream_buffers: Vec::new(),
            stream_cursor: 0,
            skipped_draws: 0,
        }
    }
}

/// A shader program in guest memory: its hash, its code, and the frame it was last checked.
type CachedCode = (u64, Arc<[u8]>, u64);

/// Which texture registers and shader bindings a stage uses.
#[derive(Debug, Clone, Copy)]
struct TextureStage {
    resource_base: u32,
    binding_base: u32,
    sampler_base: u32,
}

/// What a shader program reads and writes, whatever the state around it.
#[derive(Debug, Clone, Copy)]
struct ShaderUse {
    /// Texture units accessed, as a bit mask.
    textures: u32,
    /// Vertex shaders: parameters exported, as a bit mask.
    exports: u32,
    /// Pixel shaders: color targets written, as a bit mask.
    color_exports: u8,
}

fn unit_mask(units: &[u8]) -> u32 {
    units.iter().fold(0, |mask, unit| mask | (1 << unit))
}

/// Swizzle bits never take part in a surface's identity.
const fn masked_address(address: u32, tile: u32) -> u32 {
    if tile_mode::is_macro_tiled(tile) { address & !0x7FF } else { address }
}

impl Latte {
    pub fn end_frame(&mut self) {
        self.frame += 1;
        if self.frame.is_multiple_of(TEXTURE_SWEEP_FRAMES) {
            self.forget_unused_textures();
        }
    }

    /// Frees the host's copies of textures that were not used for long.
    fn forget_unused_textures(&mut self) {
        let frame = self.frame;
        let targets = &self.host_surfaces;
        let expired: Vec<SurfaceKey> = self
            .textures
            .iter()
            .filter(|(key, known)| {
                frame.wrapping_sub(known.last_check) >= TEXTURE_EXPIRE_FRAMES
                    && !targets.contains_key(&(key.address, key.width, key.height))
            })
            .take(MAX_TEXTURES_FORGOTTEN)
            .map(|(key, _)| *key)
            .collect();
        for key in expired {
            self.textures.remove(&key);
            self.commands.push(RenderCommand::ForgetTexture(key));
        }
    }

    fn arena_reserve(&mut self, size: u32, align: u32) -> u32 {
        let mut offset = self.arena_cursor.next_multiple_of(align);
        if u64::from(offset) + u64::from(size) > u64::from(STREAM_REGION_BASE) {
            self.commands.push(RenderCommand::ArenaWrap);
            self.arena_entries.clear();
            self.guest_buffers.clear();
            self.index_buffers.clear();
            offset = 0;
        }
        self.arena_cursor = offset + size;
        offset
    }

    /// Appends bytes, reusing an identical copy if there is one.
    fn arena_store(&mut self, data: &[u8], align: u32) -> u32 {
        if data.is_empty() {
            return 0;
        }
        let hash = xxh3_64(data) ^ (u64::from(align) << 56) ^ (data.len() as u64).rotate_left(23);
        if let Some(slice) = self.arena_entries.get(&hash) {
            return slice.offset;
        }
        let offset = self.arena_reserve(data.len() as u32, align);
        self.commands.push(RenderCommand::ArenaWrite { offset, bytes: Arc::from(data) });
        self.arena_entries.insert(hash, ArenaSlice { offset });
        offset
    }

    /// Every surface that exists on the host only.
    pub fn host_surface_keys(&self) -> Vec<SurfaceKey> {
        self.host_surfaces.values().copied().collect()
    }

    /// Brings back what [`Self::host_surface_keys`] named, after the renderer was reset.
    pub fn restore_surfaces(&mut self, surfaces: Vec<TextureUpload>) {
        for surface in surfaces {
            let key = surface.key;
            self.host_surfaces.insert((key.address, key.width, key.height), key);
            self.commands.push(RenderCommand::Upload(Box::new(surface)));
        }
    }

    /// The CPU wrote `[address, address + size)` (effective) for the GPU to read.
    pub fn note_cpu_write(&mut self, address: u32, size: u32) {
        self.writes.note(address, size);
    }

    /// [`arena_store`](Self::arena_store) of guest memory at `effective`: hashed again
    /// only after the title wrote there, like Cemu's buffer cache does.
    fn arena_store_guest(&mut self, effective: u32, data: &[u8], align: u32) -> u32 {
        let key = (effective, data.len() as u32, align);
        if !self.buffer_cache {
            return self.arena_store(data, align);
        }
        if let Some(known) = self.guest_buffers.get(&key)
            && self.frame.wrapping_sub(known.checked) < BUFFER_RECHECK_FRAMES
            && self.writes.unchanged_since(effective, key.1, known.stamp)
        {
            return known.offset;
        }
        let stamp = self.writes.now;
        let offset = self.arena_store(data, align);
        self.guest_buffers.insert(key, GuestBuffer { offset, stamp, checked: self.frame });
        offset
    }

    /// Indices of two triangles per rect. The second one is chosen in the vertex
    /// shader, which alone knows the corner positions: see `RECT_ENTRY_POINT`.
    fn rect_corners(&mut self, list: &[u32], base_vertex: u32) -> Vec<u32> {
        let corners: Vec<u32> = list.iter().map(|i| i.wrapping_add(base_vertex)).collect();
        let bytes: Vec<u8> = corners.iter().flat_map(|i| i.to_le_bytes()).collect();
        let first_word = self.arena_store(&bytes, 4) >> 2;
        let mut out = Vec::with_capacity(corners.len() * 2);
        for (rect, triangle) in corners.as_chunks::<3>().0.iter().enumerate() {
            let word = first_word + rect as u32 * 3;
            out.extend_from_slice(triangle);
            out.extend((0..3).map(|corner| {
                shader::RECT_CORNER_FLAG | (corner << shader::RECT_CORNER_SHIFT) | word
            }));
        }
        out
    }

    fn arena_write_unique(&mut self, data: &[u8], align: u32) -> u32 {
        let offset = self.arena_reserve(data.len() as u32, align);
        self.commands.push(RenderCommand::ArenaWrite { offset, bytes: Arc::from(data) });
        offset
    }

    fn key_of_target(words: &SurfaceWords, is_depth: bool) -> SurfaceKey {
        let format =
            if is_depth { host_depth_format(words.format) } else { host_format(words.format) };
        SurfaceKey {
            address: masked_address(words.address, words.tile_mode),
            width: words.width,
            height: words.height,
            layers: words.depth.max(1),
            format,
        }
    }

    fn note_target(&mut self, key: SurfaceKey) {
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

    #[inline]
    pub fn set_resolution_factor(&mut self, factor: u32) {
        self.requested_factor = factor.max(1);
        self.update_resolution();
    }

    /// Tells the renderer if the scale changed. It takes the TV's width to know, so a title
    /// starts out at its own resolution until it shows its first picture.
    fn update_resolution(&mut self) {
        if self.tv_width == 0 {
            return;
        }
        let new = ResolutionScale::for_tv(self.requested_factor, self.tv_width);
        if new != self.resolution {
            self.resolution = new;
            self.commands.push(RenderCommand::SetResolution(new));
        }
    }

    pub fn copy_to_scan(&mut self, words: &SurfaceWords, screen: crate::Screen) {
        if screen == crate::Screen::Tv && words.width != self.tv_width {
            self.tv_width = words.width;
            self.update_resolution();
        }
        let key = Self::key_of_target(words, false);
        self.commands.push(RenderCommand::CopyToScanBuffer {
            surface: key,
            slice: words.slice,
            screen,
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
    fn surface_for_read(&mut self, memory: &Memory, words: &SurfaceWords) -> SurfaceKey {
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
    fn upload_texture(
        &mut self,
        memory: &Memory,
        key: SurfaceKey,
        words: &SurfaceWords,
        dim: TextureDim,
        mip_count: u32,
        mip_base: u32,
    ) {
        let frame = self.frame;
        if let Some(known) = self.textures.get(&key) {
            if frame.wrapping_sub(known.last_check) < TEXTURE_RECHECK_FRAMES {
                return;
            }
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
        let compressed = addrlib::is_compressed(words.format & 0x3F);
        let mut hash = texture_hash(base, compressed);
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
                hash ^= texture_hash(mips, compressed).rotate_left(17);
            }
        }
        if let Some(known) = self.textures.get_mut(&key) {
            known.last_check = frame;
            if known.content_hash == hash {
                return;
            }
        }
        let mips = decode_mips(memory, words, dim, mip_count, mip_base);
        self.textures.insert(key, KnownTexture { content_hash: hash, last_check: frame });
        self.commands.push(RenderCommand::Upload(Box::new(TextureUpload { key, dim, mips })));
    }

    fn cached_code(
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
    fn texture_signature(
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
    fn vertex_use(&mut self, hash: u64, code: &[u8], uniform_blocks: bool) -> ShaderUse {
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
    fn pixel_use(&mut self, hash: u64, code: &[u8], uniform_blocks: bool) -> ShaderUse {
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
    fn fetch_attributes(&mut self, hash: u64, code: &[u8]) -> Arc<Vec<shader::Attribute>> {
        Arc::clone(
            self.fetch_cache
                .entry(hash)
                .or_insert_with(|| Arc::new(shader::parse_fetch_shader(code))),
        )
    }

    /// Signatures of the units in `used`. The others stay default: they hold whatever
    /// an earlier draw left there, and telling those apart would translate the same
    /// program over and over.
    fn used_signatures(
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

    /// Everything shader translation depends on, translated once. Only registers
    /// the shaders really read count, the same way: titles leave the rest as it was.
    fn program_pair(
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
        });
        self.programs.insert(hash, Arc::clone(&pair));
        pair
    }

    fn bind_textures(
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
                key
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
            let sampler =
                sampler_unit.map(|s| read_sampler(regs, sampler_base + u32::from(s), signature));
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

    /// Stores the uniform block of unit `index` and returns its arena offset.
    fn store_uniform_block(
        &mut self,
        memory: &Memory,
        regs: &RegisterFile,
        stage_base: u32,
        index: u32,
    ) -> u32 {
        let start = base::RESOURCE + stage_base + index * 7;
        let (physical, size) = (regs.get(start), regs.get(start + 1).wrapping_add(1));
        if physical == 0 || size == 0 || size > 0x10_0000 {
            return 0;
        }
        let Some(effective) = wiiu_mem_addr::physical_to_effective(physical) else { return 0 };
        let Ok(bytes) = memory.slice(effective, size) else { return 0 };
        // Titles endian-swap uniform blocks themselves (GX2EndianSwap): the GPU reads them as they are.
        // Copied whatever the cache says, like Cemu does: titles rewrite them without flushing.
        self.arena_store(bytes, 16)
    }

    /// Where a stream-out buffer lives in the stream region, reserving room the first time.
    fn stream_buffer(&mut self, physical: u32, size: u32) -> Option<usize> {
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

    /// Arena offset of attribute data the GPU produced itself, unless the CPU replaced it.
    fn streamed_attribute(&mut self, memory: &Memory, physical: u32) -> Option<u32> {
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
    fn stream_out_setup(
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

    /// Executes a draw from the current registers.
    /// Indices of a draw as the host takes them: little-endian `u32`, restarts as
    /// `u32::MAX`, primitives the host lacks made of triangles.
    fn host_indices(
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
                    params[8 + block as usize] =
                        self.store_uniform_block(memory, regs, resource::VS_UNIFORM_BLOCK, block);
                }
                if pair.ps_blocks & (1 << block) != 0 {
                    params[24 + block as usize] =
                        self.store_uniform_block(memory, regs, resource::PS_UNIFORM_BLOCK, block);
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

/// Indices of the draw being set up.
#[derive(Debug, Clone, Copy)]
pub struct DrawRequest<'a> {
    pub count: u32,
    pub instances: u32,
    pub base_vertex: u32,
    pub base_instance: u32,
    pub index_type: u32,
    /// Guest bytes of the index buffer (indexed draws).
    pub indices: Option<&'a [u8]>,
    /// The indices came embedded in the packet (`DRAW_INDEX_IMMD`).
    pub immediate: bool,
    /// Effective address of an index buffer in guest memory, read instead of `indices`.
    pub index_address: Option<u32>,
}

/// An index buffer as the host draws it: where in the arena, how many, and the highest one.
#[derive(Debug, Clone, Copy)]
struct HostIndices {
    offset: Option<u32>,
    count: u32,
    max_index: u32,
    stamp: u32,
    checked: u64,
}

/// What makes the host's indices of a draw: source, count, type, primitive, restart index
/// (`u32::MAX` without restart) and base vertex.
type IndexKey = (u32, u32, u32, u32, u32, u32);

/// Uniform blocks a translated stage reads (`P.<stage>_ubo[n]`), as a bit mask.
fn used_uniform_blocks(wgsl: &str, stage: &str) -> u16 {
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
fn exported_params(wgsl: &str) -> u32 {
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

const fn needs_expansion(mode: u32) -> bool {
    matches!(
        mode,
        primitive::TRIANGLE_FAN
            | primitive::RECTS
            | primitive::LINE_LOOP
            | primitive::QUADS
            | primitive::QUAD_STRIP
    )
}

fn read_indices(source: &[u8], count: u32, kind: u32, immediate: bool) -> Vec<u32> {
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

/// Primitive modes the host lacks become triangle or line lists.
fn expand_indices(mode: u32, indices: Option<Vec<u32>>) -> Option<Vec<u32>> {
    let indices = indices?;
    Some(match mode {
        primitive::TRIANGLE_FAN => {
            let mut out = Vec::with_capacity(indices.len().saturating_sub(2) * 3);
            for pair in indices.windows(2).skip(1) {
                out.extend([indices[0], pair[0], pair[1]]);
            }
            out
        }
        primitive::QUADS => {
            let mut out = Vec::with_capacity(indices.len() / 4 * 6);
            for quad in indices.as_chunks::<4>().0 {
                out.extend([quad[0], quad[1], quad[2], quad[0], quad[2], quad[3]]);
            }
            out
        }
        primitive::QUAD_STRIP => {
            let mut out = Vec::with_capacity(indices.len() * 3);
            for quad in indices.windows(4).step_by(2) {
                out.extend([quad[0], quad[1], quad[3], quad[0], quad[3], quad[2]]);
            }
            out
        }
        primitive::RECTS => indices.as_chunks::<3>().0.iter().flatten().copied().collect(),
        primitive::LINE_LOOP => {
            let mut out = indices.clone();
            if let Some(first) = indices.first() {
                out.push(*first);
            }
            out
        }
        _ => indices,
    })
}

const fn topology_of(mode: u32) -> Topology {
    match mode {
        primitive::POINTS => Topology::Points,
        primitive::LINES => Topology::Lines,
        primitive::LINE_STRIP | primitive::LINE_LOOP => Topology::LineStrip,
        primitive::TRIANGLE_STRIP => Topology::TriangleStrip,
        primitive::RECTS => Topology::Rects,
        _ => Topology::Triangles,
    }
}

fn viewport_of(regs: &RegisterFile) -> [f32; 6] {
    let v = |i: u32| f32::from_bits(regs.get(reg::PA_CL_VPORT_XSCALE + i));
    let (xs, xo, ys, yo, zs, zo) = (v(0), v(1), v(2), v(3), v(4), v(5));
    let half_z = regs.get(reg::PA_CL_CLIP_CNTL) & (1 << 19) != 0;
    let (near, far) = if half_z { (zo, zo + zs) } else { (zo - zs, zo + zs) };
    let height = ys.abs() * 2.0;
    [
        xo - xs,
        yo - ys.abs(),
        xs * 2.0,
        height,
        near.clamp(0.0, 1.0).min(far.clamp(0.0, 1.0)),
        far.clamp(0.0, 1.0).max(near.clamp(0.0, 1.0)),
    ]
}

/// The render target slot each pixel shader export goes to. Exports are numbered without
/// gaps, and the `n`th one lands in the `n`th slot `CB_SHADER_MASK` enables: Breath of the
/// Wild exports 0 to 3 under the mask `0xF0F0FF` (slots 0, 1, 3 and 5), and its normals are
/// lost if export 2 is taken for slot 2.
fn export_slots(shader_mask: u32, exports: u8) -> [Option<u8>; 8] {
    let enabled = (0..8_u8).filter(|slot| (shader_mask >> (4 * slot)) & 0xF != 0);
    let enabled_bits = enabled.clone().fold(0_u8, |bits, slot| bits | (1 << slot));
    // No gaps to close, or a mask that says nothing about where the exports go.
    if enabled_bits == exports || enabled_bits == 0 {
        return std::array::from_fn(|slot| Some(slot as u8));
    }
    let mut slots = [None; 8];
    for (export, slot) in enabled.enumerate() {
        slots[export] = Some(slot);
    }
    slots
}

fn pipeline_state(regs: &RegisterFile) -> PipelineState {
    let depth = regs.get(reg::DB_DEPTH_CONTROL);
    let mode = regs.get(reg::PA_SU_SC_MODE_CNTL);
    let blend_mask = (regs.get(reg::CB_COLOR_CONTROL) >> 8) & 0xFF;
    let blend = std::array::from_fn(|i| {
        let word = regs.get(reg::CB_BLEND0_CONTROL + i as u32);
        let separate = word & (1 << 29) != 0;
        let (color_src, color_op, color_dst) =
            ((word & 0x1F) as u8, ((word >> 5) & 7) as u8, ((word >> 8) & 0x1F) as u8);
        BlendState {
            enabled: blend_mask & (1 << i) != 0,
            color_src,
            color_dst,
            color_op,
            alpha_src: if separate { ((word >> 16) & 0x1F) as u8 } else { color_src },
            alpha_dst: if separate { ((word >> 24) & 0x1F) as u8 } else { color_dst },
            alpha_op: if separate { ((word >> 21) & 7) as u8 } else { color_op },
        }
    });
    let front = StencilFace {
        compare: ((depth >> 8) & 7) as u8,
        fail: ((depth >> 11) & 7) as u8,
        pass: ((depth >> 14) & 7) as u8,
        depth_fail: ((depth >> 17) & 7) as u8,
    };
    let back = if depth & (1 << 7) != 0 {
        StencilFace {
            compare: ((depth >> 20) & 7) as u8,
            fail: ((depth >> 23) & 7) as u8,
            pass: ((depth >> 26) & 7) as u8,
            depth_fail: ((depth >> 29) & 7) as u8,
        }
    } else {
        front
    };
    let stencil = regs.get(reg::DB_STENCILREFMASK);
    let bias_enabled = mode & (3 << 11) != 0;
    let bias = if bias_enabled {
        [
            regs.get(reg::PA_SU_POLY_OFFSET_FRONT_SCALE),
            regs.get(reg::PA_SU_POLY_OFFSET_FRONT_SCALE + 1),
            regs.get(reg::PA_SU_POLY_OFFSET_CLAMP),
        ]
    } else {
        [0; 3]
    };
    PipelineState {
        topology: Topology::Triangles,
        color_formats: [None; 8],
        color_masks: [0; 8],
        blend,
        depth_format: None,
        depth_test: depth & 0b10 != 0,
        depth_write: depth & 0b100 != 0,
        depth_compare: ((depth >> 4) & 7) as u8,
        stencil_test: depth & 1 != 0,
        stencil_front: front,
        stencil_back: back,
        stencil_read_mask: ((stencil >> 8) & 0xFF) as u8,
        stencil_write_mask: ((stencil >> 16) & 0xFF) as u8,
        cull_front: mode & 1 != 0,
        cull_back: mode & 2 != 0,
        front_face_cw: mode & 4 != 0,
        depth_bias: bias,
    }
}

const fn address_mode(bits: u32) -> AddressMode {
    match bits {
        0 => AddressMode::Repeat,
        1 | 3 | 5 | 7 => AddressMode::Mirror,
        6 => AddressMode::Border,
        _ => AddressMode::Clamp,
    }
}

fn read_sampler(regs: &RegisterFile, index: u32, signature: TextureSignature) -> SamplerDesc {
    let word0 = regs.get(base::SAMPLER + index * 3);
    let word1 = regs.get(base::SAMPLER + index * 3 + 1);
    let non_filterable = signature.kind != TextureKind::Float;
    let filter = |bits: u32| !non_filterable && matches!(bits & 7, 1 | 5);
    let (mag_linear, min_linear) = (filter((word0 >> 9) & 7), filter((word0 >> 12) & 7));
    let min_lod = (word1 & 0x3FF) as u16;
    let mip_filter = (word0 >> 17) & 3;
    // Without a mip filter only the first level of the range is read.
    let max_lod = if mip_filter == 0 { min_lod } else { ((word1 >> 10) & 0x3FF) as u16 };
    let aniso_ratio = (word0 >> 19) & 7;
    let anisotropy =
        if mag_linear && min_linear && mip_filter != 0 && aniso_ratio != 0 && !signature.comparison
        {
            1_u8 << aniso_ratio.min(4)
        } else {
            0
        };
    SamplerDesc {
        address: [
            address_mode(word0 & 7),
            address_mode((word0 >> 3) & 7),
            address_mode((word0 >> 6) & 7),
        ],
        mag_linear,
        min_linear,
        mip_linear: !non_filterable && mip_filter == 2,
        anisotropy,
        min_lod,
        max_lod,
        compare: signature.comparison.then_some(((word0 >> 26) & 7) as u8),
        border: border_color(regs, index, (word0 >> 22) & 3),
        non_filterable,
    }
}

/// The nearest host border color. Type 3 takes it from the unit's registers.
fn border_color(regs: &RegisterFile, index: u32, kind: u32) -> BorderColor {
    match kind {
        0 => BorderColor::TransparentBlack,
        1 => BorderColor::OpaqueBlack,
        2 => BorderColor::OpaqueWhite,
        _ => {
            let base = if index < 18 {
                config::TD_PS_SAMPLER0_BORDER_RED + index * 4
            } else {
                config::TD_VS_SAMPLER0_BORDER_RED + (index - 18) * 4
            };
            let channel = |i: u32| f32::from_bits(regs.get(base + i));
            if channel(3) < 0.5 {
                BorderColor::TransparentBlack
            } else if channel(0) >= 0.5 {
                BorderColor::OpaqueWhite
            } else {
                BorderColor::OpaqueBlack
            }
        }
    }
}

const fn gx2_dim(dim: TextureDim) -> u32 {
    match dim {
        TextureDim::D2 => addrlib::dim::D2,
        TextureDim::D2Array => addrlib::dim::D2_ARRAY,
        TextureDim::D3 => addrlib::dim::D3,
        TextureDim::Cube => addrlib::dim::CUBE,
    }
}

/// Samples a large surface: hashing all of it per frame would dominate.
/// Whether a texture's guest memory changed. Titles rewrite parts of large textures:
/// *Breath of the Wild* scrolls its terrain height maps (1024 x 1024 slices, addressed
/// with wrap-around) a strip at a time, which 256 samples over the whole texture miss,
/// leaving stale heights as walls. A word every 256 bytes (1 KiB for compressed blocks)
/// sees such strips in every tile row at little cost; Cemu samples similarly densely.
fn texture_hash(bytes: &[u8], compressed: bool) -> u64 {
    if bytes.len() <= 0x4000 {
        return xxh3_64(bytes);
    }
    let step = if compressed { 1024 } else { 256 };
    let mut hash = bytes.len() as u64;
    for chunk in bytes.chunks(step) {
        let Some(word) = chunk.first_chunk::<8>() else { break };
        hash = hash.wrapping_add(u64::from_ne_bytes(*word)).rotate_left(3);
    }
    hash
}

fn sampled_hash(bytes: &[u8]) -> u64 {
    if bytes.len() <= 0x4000 {
        return xxh3_64(bytes);
    }
    let step = (bytes.len() / 256).max(HASH_STRIDE);
    let mut hash = bytes.len() as u64;
    let mut at = 0;
    while at + HASH_STRIDE <= bytes.len() {
        hash = hash.rotate_left(7) ^ xxh3_64(&bytes[at..at + HASH_STRIDE]);
        at += step;
    }
    hash
}

/// Bytes from the first mip level after the largest to the end of the last one, laid out as
/// [`decode_mips`] reads them.
fn mip_chain_size(words: &SurfaceWords, dim: TextureDim, mip_count: u32) -> u32 {
    let (mut offset, mut previous_size) = (0_u32, 0_u32);
    let mut last_tile = words.tile_mode;
    let mut macro_stopped = false;
    for level in 1..mip_count {
        let info = addrlib::gx2_surface_info(
            words.format,
            words.width,
            words.height,
            words.depth.max(1),
            gx2_dim(dim),
            words.tile_mode,
            words.aa,
            level,
            false,
            false,
        );
        if level > 1 {
            let stop = tile_mode::is_macro_tiled(last_tile)
                && !tile_mode::is_macro_tiled(info.hw_tile_mode);
            if stop && !macro_stopped {
                macro_stopped = true;
                offset += words.address & 0x700;
            }
            offset = (offset + previous_size).next_multiple_of(info.base_align.max(1));
        }
        last_tile = info.hw_tile_mode;
        previous_size = info.surf_size as u32;
    }
    offset.saturating_add(previous_size).max(1)
}

fn decode_mips(
    memory: &Memory,
    words: &SurfaceWords,
    dim: TextureDim,
    mip_count: u32,
    mip_base: u32,
) -> Vec<MipData> {
    let is_compressed = addrlib::is_compressed(words.format & 0x3F);
    let hw_dim = gx2_dim(dim);
    let mut mips = Vec::with_capacity(mip_count as usize);
    let mut mip_offset = 0_u32;
    let mut previous_size = 0_u32;
    let base_tile = words.tile_mode;
    let swizzle = words.address & 0x700;
    let mut last_tile = base_tile;
    let mut macro_stopped = false;
    for level in 0..mip_count {
        let info = addrlib::gx2_surface_info(
            words.format,
            words.width,
            words.height,
            words.depth.max(1),
            hw_dim,
            base_tile,
            words.aa,
            level,
            false,
            false,
        );
        let align = info.base_align.max(1);
        let address = if level == 0 {
            masked_address(words.address, base_tile) & !0xFF
        } else {
            // Mips are laid out back to back, each on its alignment boundary.
            if level == 1 {
                mip_offset = 0;
            } else {
                let stop = tile_mode::is_macro_tiled(last_tile)
                    && !tile_mode::is_macro_tiled(info.hw_tile_mode);
                if stop && !macro_stopped {
                    macro_stopped = true;
                    mip_offset += swizzle;
                }
                mip_offset += previous_size;
                mip_offset = mip_offset.next_multiple_of(align);
            }
            (masked_address(mip_base, base_tile) & !0xFF).wrapping_add(mip_offset)
        };
        last_tile = info.hw_tile_mode;
        previous_size = info.surf_size as u32;
        let width = (words.width >> level).max(1);
        let height = (words.height >> level).max(1);
        let (blocks_x, blocks_y) =
            if is_compressed { (width.div_ceil(4), height.div_ceil(4)) } else { (width, height) };
        let layers = match dim {
            TextureDim::D3 => (words.depth >> level).max(1),
            TextureDim::D2 => 1,
            _ => words.depth.max(1),
        };
        let swizzled = tile_mode::is_macro_tiled(info.hw_tile_mode);
        let (pipe_swizzle, bank_swizzle) =
            if swizzled { swizzle_from_address(swizzle) } else { (0, 0) };
        let layout = Layout {
            tile_mode: info.hw_tile_mode,
            bpp: info.bpp,
            pitch: info.pitch,
            height: info.height,
            samples: 1,
            is_depth: false,
            pipe_swizzle,
            bank_swizzle,
        };
        let Some(effective) = wiiu_mem_addr::physical_to_effective(address) else { break };
        let size = (info.surf_size as u32).clamp(1, 0x0800_0000);
        let Ok(source) = memory.slice(effective, size) else {
            log::debug!("mip {level} at {address:#010x} is outside mapped memory");
            break;
        };
        let mut data = Vec::with_capacity(
            blocks_x as usize * blocks_y as usize * (info.bpp as usize / 8) * layers as usize,
        );
        for slice in 0..layers {
            detile(source, &layout, blocks_x, blocks_y, slice, &mut data);
        }
        let data = widen_texels(words.format, &data).unwrap_or(data);
        mips.push(MipData { width, height, layers, data });
    }
    mips
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(width: u32, height: u32) -> SurfaceKey {
        SurfaceKey { address: 0x1000, width, height, layers: 1, format: HostFormat::Rgba8Unorm }
    }

    #[test]
    fn exports_land_in_the_slots_the_shader_mask_enables() {
        // Exports 0 to 3 over slots 0, 1, 3 and 5.
        let slots = export_slots(0x00F0_F0FF, 0b1111);
        assert_eq!(slots[..5], [Some(0), Some(1), Some(3), Some(5), None]);
        // Nothing to close: exports stay where they are.
        assert_eq!(export_slots(0xFF, 0b11)[..2], [Some(0), Some(1)]);
        assert_eq!(export_slots(0xF0, 0b10)[1], Some(1));
        assert_eq!(export_slots(0, 0b1)[0], Some(0));
    }

    #[test]
    fn running_out_of_memory_steps_down_through_the_factors() {
        assert_eq!(ResolutionScale::lower_factor(8), 6);
        assert_eq!(ResolutionScale::lower_factor(5), 4);
        assert_eq!(ResolutionScale::lower_factor(2), 1);
        assert_eq!(ResolutionScale::lower_factor(1), 1);
    }

    #[test]
    fn the_scale_stays_within_10k() {
        assert_eq!(ResolutionScale::for_tv(8, 1280).factor, 8, "10240 x 5760");
        assert_eq!(ResolutionScale::for_tv(8, 1920).factor, 5, "9600 x 5400");
        assert_eq!(ResolutionScale::for_tv(2, 1920).factor, 2);
        assert_eq!(ResolutionScale::for_tv(1, 1280).factor, 1);
    }

    #[test]
    fn pictures_grow_and_shadow_maps_do_not() {
        let scale = ResolutionScale::for_tv(4, 1280);
        assert_eq!(scale.of(&target(1280, 720)), 4);
        assert_eq!(scale.of(&target(640, 360)), 4, "half-size effects keep their proportion");
        assert_eq!(scale.of(&target(1024, 1024)), 1, "square: a shadow map");
        assert_eq!(scale.of(&target(2048, 1024)), 1, "wider than the TV");
        assert_eq!(scale.of(&target(16, 9)), 1, "a tiny target holds data");
        let cascades = SurfaceKey { layers: 4, ..target(1280, 720) };
        assert_eq!(scale.of(&cascades), 1);
        assert_eq!(ResolutionScale::default().of(&target(1280, 720)), 1);
    }

    #[test]
    fn the_renderer_hears_of_the_scale_once_the_tv_is_known() {
        let mut latte = Latte::default();
        let scale_commands = |latte: &Latte| -> Vec<ResolutionScale> {
            latte
                .commands
                .iter()
                .filter_map(|c| match c {
                    RenderCommand::SetResolution(scale) => Some(*scale),
                    _ => None,
                })
                .collect()
        };
        latte.set_resolution_factor(4);
        assert!(scale_commands(&latte).is_empty(), "no picture yet");
        let tv = SurfaceWords { width: 1280, height: 720, ..SurfaceWords::default() };
        latte.copy_to_scan(&tv, crate::Screen::Tv);
        latte.copy_to_scan(&tv, crate::Screen::Tv);
        assert_eq!(scale_commands(&latte), [ResolutionScale { factor: 4, limit_width: 1280 }]);
        latte.set_resolution_factor(1);
        assert_eq!(scale_commands(&latte).len(), 2);
    }

    #[test]
    fn depth_read_as_a_texture_keeps_its_precision() {
        assert_eq!(host_format(0x011), HostFormat::R32Float);
        assert_eq!(host_format(0x811), HostFormat::R32Float);
        assert_eq!(host_depth_format(0x011), HostFormat::Depth24PlusStencil8);
    }

    #[test]
    fn a_program_is_translated_once_whatever_earlier_draws_left_in_the_registers() {
        let mut latte = Latte::default();
        let mut regs = RegisterFile::default();
        let code: Arc<[u8]> = Arc::from(vec![0_u8; 64]);
        let program = |latte: &mut Latte, regs: &RegisterFile| {
            latte.program_pair(
                regs,
                (1, Arc::clone(&code)),
                (2, Arc::clone(&code)),
                (3, Arc::clone(&code)),
            )
        };
        let first = program(&mut latte, &regs);

        // A texture unit, an input and a semantic these shaders do not use.
        let unit = base::RESOURCE + resource::PS_TEXTURE;
        regs.set(unit + 2, 0x100);
        regs.set(unit + 4, u32::MAX);
        regs.set(reg::SPI_PS_INPUT_CNTL_0 + 5, 0x1234);
        regs.set(reg::SQ_VTX_SEMANTIC_0 + 7, 9);
        assert!(Arc::ptr_eq(&first, &program(&mut latte, &regs)));

        // An input in use does make another program.
        regs.set(reg::SPI_PS_IN_CONTROL_0, 1);
        assert!(!Arc::ptr_eq(&first, &program(&mut latte, &regs)));
    }
}
