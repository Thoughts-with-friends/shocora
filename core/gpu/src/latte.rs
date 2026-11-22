//! GPU: AMD Radeon, package codenamed "Latte".
//! Core clock is 549.999 MHz.
//! Turns register state into host work: render targets, shaders, textures,
//! vertex data and pipeline state of a draw.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                                        ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ GPU register state │   │ latte.rs                 │   │ textures, buffers, │
//! │ draw commands      │──►│ ──────────────────────── │──►│ draws              │
//! │                    │   │ draws → host work        │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The core of the GPU emulation: for each draw it gathers the game's shaders, textures and
//! buffers and describes the same draw for your graphics card. Terms such as HLE, JIT or shader
//! are explained in `docs/guide/wiiu_term.md`.
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
        HostFormat, MipData, PipelineState, ReadbackData, ReadbackReply, ReadbackTarget,
        RenderCommand, ResolutionScale, SamplerDesc, ShaderModule, StencilFace, StreamCopy,
        SurfaceKey, TextureUpload, Topology,
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
/// Frames a texture whose memory saw no noted write ([`WriteTracker`]) goes without being
/// hashed again. Hashing every byte of every sampled texture every other frame took a fifth
/// of the GPU thread in Breath of the Wild; this is the safety net for writes the title
/// never flushed.
const TEXTURE_TRUST_FRAMES: u64 = 10;
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

/// 32-bit colour formats whose bits the renderer can move into one another.
fn same_bits(format: HostFormat) -> bool {
    use HostFormat as H;
    matches!(
        format,
        H::Rgba8Unorm
            | H::Rgba8Snorm
            | H::Rg16Float
            | H::Rg16Unorm
            | H::R32Float
            | H::Rgb10a2Unorm
            | H::Rg11b10Float
    )
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
    /// Per 1 MiB, the latest of its pages' stamps: most checks of large vertex buffers end
    /// here instead of looking at hundreds of pages each draw.
    coarse: Vec<u32>,
    now: u32,
}

/// Granularity of [`WriteTracker::coarse`].
const TRACK_COARSE_BITS: u32 = 20;

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
    /// [`WriteTracker::now`] and frame of the last hash.
    stamp: u32,
    hashed: u64,
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
    /// Vec4s each stage reads of each block: see [`shader::TranslatedShader::ubo_vec4s`].
    vs_ubo_vec4s: [u32; 16],
    ps_ubo_vec4s: [u32; 16],
}

/// Render targets in linear memory, and what the renderer read back of them.
///
/// Titles draw small targets in linear memory for the CPU to read: Breath of the Wild has
/// about thirty (1x1 and 8x8 `R32G32B32A32_FLOAT`, 256x256 `R16_UNORM`, ...). Read nowhere
/// but on the host, the CPU saw what was there before; Cemu mirrors every such target back.
struct Readbacks {
    reply: std::sync::mpsc::Sender<ReadbackData>,
    results: std::sync::mpsc::Receiver<ReadbackData>,
    /// Drawn to since the last flip.
    pending: Vec<ReadbackTarget>,
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
    /// `[TextureRedefine]` rules of the enabled graphic packs.
    pub(crate) texture_rules: Vec<wiiu_graphic_pack::TextureRule>,
    /// The ones of them setting anisotropy: looked at on every texture bind.
    sampler_rules: Vec<wiiu_graphic_pack::TextureRule>,
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
    readbacks: Readbacks,
    pub skipped_draws: u64,
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

#[cfg(test)]
#[path = "latte_tests.rs"]
mod tests;

mod draw_state;
mod draws;
mod programs;
mod targets;
mod textures;
#[allow(unused_imports)]
pub(crate) use draw_state::*;
#[allow(unused_imports)]
pub(crate) use draws::*;
#[allow(unused_imports)]
pub(crate) use programs::*;
#[allow(unused_imports)]
pub(crate) use targets::*;
#[allow(unused_imports)]
pub(crate) use textures::*;
