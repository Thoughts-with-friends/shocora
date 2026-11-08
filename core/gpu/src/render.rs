//! API-neutral work for the host renderer.
//!
//! Latte keeps guest state; the renderer keeps host objects. A surface is
//! identified by [`SurfaceKey`], so a render target can later be sampled as a
//! texture without ever going through guest memory.
use std::sync::Arc;

use crate::shader::{TextureDim, TextureKind};

/// One of the console's two displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Screen {
    Tv,
    Drc,
}

impl Screen {
    pub const ALL: [Self; 2] = [Self::Tv, Self::Drc];

    /// Default resolution `(width, height)`.
    pub const fn default_size(self) -> (u32, u32) {
        match self {
            Self::Tv => (1280, 720),
            Self::Drc => (854, 480),
        }
    }
}

/// Host pixel formats (a subset every desktop adapter supports).
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostFormat {
    R8Unorm,
    R8Snorm,
    R8Uint,
    R8Sint,
    Rg8Unorm,
    Rg8Snorm,
    Rg8Uint,
    Rg8Sint,
    Rgba8Unorm,
    Rgba8Srgb,
    Rgba8Snorm,
    Rgba8Uint,
    Rgba8Sint,
    R16Uint,
    R16Sint,
    R16Float,
    R16Unorm,
    Rg16Uint,
    Rg16Sint,
    Rg16Float,
    Rg16Unorm,
    Rgba16Uint,
    Rgba16Sint,
    Rgba16Float,
    Rgba16Unorm,
    Rgba16Snorm,
    R32Uint,
    R32Sint,
    R32Float,
    Rg32Uint,
    Rg32Sint,
    Rg32Float,
    Rgba32Uint,
    Rgba32Sint,
    Rgba32Float,
    Rgb10a2Unorm,
    Rg11b10Float,
    Bc1Unorm,
    Bc1Srgb,
    Bc2Unorm,
    Bc2Srgb,
    Bc3Unorm,
    Bc3Srgb,
    Bc4Unorm,
    Bc4Snorm,
    Bc5Unorm,
    Bc5Snorm,
    Depth16Unorm,
    Depth24PlusStencil8,
    Depth32Float,
    Depth32FloatStencil8,
}

impl HostFormat {
    pub const fn is_depth(self) -> bool {
        matches!(
            self,
            Self::Depth16Unorm
                | Self::Depth24PlusStencil8
                | Self::Depth32Float
                | Self::Depth32FloatStencil8
        )
    }

    #[inline]
    pub const fn has_stencil(self) -> bool {
        matches!(self, Self::Depth24PlusStencil8 | Self::Depth32FloatStencil8)
    }

    /// The sample type shaders see.
    pub const fn kind(self) -> TextureKind {
        match self {
            Self::R8Uint
            | Self::Rg8Uint
            | Self::Rgba8Uint
            | Self::R16Uint
            | Self::Rg16Uint
            | Self::Rgba16Uint
            | Self::R32Uint
            | Self::Rg32Uint
            | Self::Rgba32Uint => TextureKind::Uint,
            Self::R8Sint
            | Self::Rg8Sint
            | Self::Rgba8Sint
            | Self::R16Sint
            | Self::Rg16Sint
            | Self::Rgba16Sint
            | Self::R32Sint
            | Self::Rg32Sint
            | Self::Rgba32Sint => TextureKind::Sint,
            f if f.is_depth() => TextureKind::Depth,
            _ => TextureKind::Float,
        }
    }
}

/// Identity of a surface. The swizzle bits are masked out of `address`, so a
/// render target and a texture view of it compare equal.
/// How much larger than on the console the host draws.
///
/// Surfaces the title renders to are made `factor` times as wide and high; textures it brings
/// along stay as they are. Which ones is a matter of size only, so that the shader translator
/// and the host renderer agree without talking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolutionScale {
    pub factor: u32,
    /// Width of the TV picture. What is wider is no picture but something like an atlas.
    pub limit_width: u32,
}

impl Default for ResolutionScale {
    fn default() -> Self {
        Self { factor: 1, limit_width: 0 }
    }
}

impl ResolutionScale {
    /// The factors on offer, smallest first.
    pub const FACTORS: [u32; 7] = [1, 2, 3, 4, 5, 6, 8];
    /// Widest picture the host draws: "10K".
    pub const MAX_WIDTH: u32 = 10_240;
    /// Smaller targets hold data rather than pictures: light levels, lookup tables.
    const MIN_SIDE: u32 = 32;

    /// The scale for a title showing a TV picture `tv_width` wide, for a `requested` factor.
    #[inline]
    pub const fn for_tv(requested: u32, tv_width: u32) -> Self {
        let most = Self::MAX_WIDTH / if tv_width == 0 { 1 } else { tv_width };
        let factor = if requested < most { requested } else { most };
        Self { factor: if factor == 0 { 1 } else { factor }, limit_width: tv_width }
    }

    /// The factor on offer below `factor`, for when video memory runs out.
    #[inline]
    pub fn lower_factor(factor: u32) -> u32 {
        Self::FACTORS.into_iter().rev().find(|f| *f < factor).unwrap_or(1)
    }

    /// Whether the target `key` is drawn larger. Square targets and arrays are mostly shadow
    /// maps, which would eat the memory of everything else at these factors.
    pub const fn applies(&self, key: &SurfaceKey) -> bool {
        self.factor > 1
            && key.layers == 1
            && key.width != key.height
            && key.width <= self.limit_width
            && key.width >= Self::MIN_SIDE
            && key.height >= Self::MIN_SIDE
    }

    /// The factor of the target `key`.
    #[inline]
    pub const fn of(&self, key: &SurfaceKey) -> u32 {
        if self.applies(key) { self.factor } else { 1 }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SurfaceKey {
    pub address: u32,
    pub width: u32,
    pub height: u32,
    pub layers: u32,
    pub format: HostFormat,
}

/// A clear of a color and/or depth target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DepthClear {
    pub surface: SurfaceKey,
    pub slice: u32,
    pub depth: Option<f32>,
    pub stencil: Option<u32>,
}

/// One mip level, tightly packed in the host format.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MipData {
    pub width: u32,
    pub height: u32,
    pub layers: u32,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// New contents of a texture that came from guest memory.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TextureUpload {
    pub key: SurfaceKey,
    pub dim: TextureDim,
    pub mips: Vec<MipData>,
}

/// Texture addressing of `SQ_TEX_SAMPLER_WORD0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AddressMode {
    Repeat,
    Mirror,
    #[default]
    Clamp,
    Border,
}

/// The border colors hosts offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BorderColor {
    #[default]
    TransparentBlack,
    OpaqueBlack,
    OpaqueWhite,
}

/// A host sampler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SamplerDesc {
    pub address: [AddressMode; 3],
    pub mag_linear: bool,
    pub min_linear: bool,
    pub mip_linear: bool,
    pub anisotropy: u8,
    /// LOD range in 1/64ths.
    pub min_lod: u16,
    pub max_lod: u16,
    /// `Some(compare function)` for comparison samplers (`E_COMPAREFUNC`).
    pub compare: Option<u8>,
    /// What [`AddressMode::Border`] reads outside the texture.
    pub border: BorderColor,
    /// The texture cannot be filtered (depth or integer formats).
    pub non_filterable: bool,
}

/// One texture and sampler pair of a draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BoundTexture {
    /// Binding slot: `(stage base + unit) * 2`; the sampler follows it.
    pub binding: u32,
    pub key: SurfaceKey,
    pub dim: TextureDim,
    pub base_level: u32,
    pub level_count: u32,
    pub base_layer: u32,
    pub layer_count: u32,
    /// `None` if the program only loads texels.
    pub sampler: Option<SamplerDesc>,
}

/// Blending of one color target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct BlendState {
    pub enabled: bool,
    pub color_src: u8,
    pub color_dst: u8,
    pub color_op: u8,
    pub alpha_src: u8,
    pub alpha_dst: u8,
    pub alpha_op: u8,
}

/// One side of the stencil test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct StencilFace {
    pub compare: u8,
    pub fail: u8,
    pub depth_fail: u8,
    pub pass: u8,
}

/// Everything fixed in a host pipeline object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PipelineState {
    /// `VGT_PRIMITIVE_TYPE` after index expansion.
    pub topology: Topology,
    pub color_formats: [Option<HostFormat>; 8],
    pub color_masks: [u8; 8],
    pub blend: [BlendState; 8],
    pub depth_format: Option<HostFormat>,
    pub depth_test: bool,
    pub depth_write: bool,
    pub depth_compare: u8,
    pub stencil_test: bool,
    pub stencil_front: StencilFace,
    pub stencil_back: StencilFace,
    pub stencil_read_mask: u8,
    pub stencil_write_mask: u8,
    pub cull_front: bool,
    pub cull_back: bool,
    pub front_face_cw: bool,
    /// Scale, offset and clamp of `PA_SU_POLY_OFFSET_*` as raw bits.
    pub depth_bias: [u32; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Topology {
    Points,
    Lines,
    LineStrip,
    Triangles,
    /// Triangles drawn through the vertex shader's rect entry point.
    Rects,
    TriangleStrip,
}

/// Stream-out data a draw leaves, to be moved where later draws read vertices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamCopy {
    /// Byte offset in the stream-out buffer; the data goes to the same offset of the
    /// arena's stream region.
    pub offset: u32,
    pub size: u32,
}

/// One draw call.
#[derive(Debug, Clone, PartialEq)]
pub struct DrawCommand {
    /// Only the vertex shader's stream-out writes count: nothing is rasterized.
    pub stream_only: bool,
    pub stream_copies: Vec<StreamCopy>,
    pub vertex: Arc<ShaderModule>,
    pub pixel: Arc<ShaderModule>,
    pub state: PipelineState,
    pub color_targets: [Option<(SurfaceKey, u32)>; 8],
    pub depth_target: Option<(SurfaceKey, u32)>,
    pub textures: Vec<BoundTexture>,
    /// Byte offset of the `DrawParams` block in the arena.
    pub params_offset: u32,
    /// `(x, y, width, height, min depth, max depth)`.
    pub viewport: [f32; 6],
    /// `(x, y, width, height)`.
    pub scissor: [u32; 4],
    pub stencil_reference: u8,
    pub blend_constant: [f32; 4],
    pub count: u32,
    pub instances: u32,
    pub first_vertex: u32,
    /// Byte offset of `u32` indices in the arena, if indexed.
    pub index_offset: Option<u32>,
    /// Restart index handling requires an index format in strip pipelines.
    pub base_vertex: i32,
}

/// WGSL plus the identity used for pipeline caching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaderModule {
    pub id: u64,
    pub wgsl: String,
    /// Uses the stream-out buffer in `group(2)`.
    pub stream_out: bool,
    /// Binding numbers of `group(1)` this module uses: `(binding, is sampler)`.
    pub bindings: Vec<(u32, BindingKind)>,
}

/// What a `group(1)` binding holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindingKind {
    Texture { dim: TextureDim, kind: TextureKind },
    Sampler { comparison: bool, filtering: bool },
}

/// Where the renderer answers [`RenderCommand::ReadSurfaces`].
#[derive(Debug, Clone)]
pub struct SurfaceReply(pub std::sync::mpsc::Sender<Vec<TextureUpload>>);

impl PartialEq for SurfaceReply {
    fn eq(&self, _: &Self) -> bool {
        false
    }
}

/// Work for the host renderer, independent of the graphics API.
#[derive(Debug, Clone, PartialEq)]
pub enum RenderCommand {
    /// Reads surfaces that exist on the host only, for a saved state: depth stays out, as it can
    /// not be written back, and titles draw it anew every frame.
    ReadSurfaces {
        keys: Vec<SurfaceKey>,
        reply: SurfaceReply,
    },
    /// Draw at this scale from now on: what the old one drew is gone.
    SetResolution(ResolutionScale),
    /// Fill a screen with a solid color (linear RGBA in `0.0..=1.0`).
    ClearScreen {
        screen: Screen,
        color: [f32; 4],
    },
    /// Copies `bytes` to the arena at `offset`. Everything recorded so far has
    /// been drawn before bytes are overwritten: see [`Self::ArenaWrap`].
    ArenaWrite {
        offset: u32,
        bytes: Arc<[u8]>,
    },
    /// The arena restarts: flush work that still reads its old contents.
    ArenaWrap,
    /// The console rebooted: shader ids start over and no surface survives.
    Reset,
    Upload(Box<TextureUpload>),
    /// A texture nobody used for long: it is uploaded again from guest memory when needed.
    ForgetTexture(SurfaceKey),
    Clear {
        color: Option<(SurfaceKey, u32, [f32; 4])>,
        depth: Option<DepthClear>,
    },
    Draw(Box<DrawCommand>),
    CopySurface {
        source: SurfaceKey,
        source_level: u32,
        source_slice: u32,
        target: SurfaceKey,
        target_level: u32,
        target_slice: u32,
    },
    /// Queues `surface` as the next image of `screen`.
    CopyToScanBuffer {
        surface: SurfaceKey,
        slice: u32,
        screen: Screen,
    },
    /// Shows the queued scan buffers.
    Flip,
}

/// A host renderer (wgpu, ...).
pub trait RenderBackend {
    /// Executes commands in order.
    fn execute(&mut self, commands: &[RenderCommand]);
}
