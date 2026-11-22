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
//! │ GPU register state │   │ latte/draw_state.rs      │   │ textures, buffers, │
//! │ draw commands      │──►│ ──────────────────────── │──►│ draws              │
//! │                    │   │ draws → host work: draw  │   │                    │
//! │                    │   │ state                    │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "draw state" share of latte.rs (draws → host work), moved here so that no file is longer
//! than 500 lines. It works exactly as if it were still written in that file. The core of the
//! GPU emulation: for each draw it gathers the game's shaders, textures and buffers and
//! describes the same draw for your graphics card. Terms such as HLE, JIT or shader are
//! explained in `docs/guide/wiiu_term.md`.

use super::*;

/// Primitive modes the host lacks become triangle or line lists.
pub(super) fn expand_indices(mode: u32, indices: Option<Vec<u32>>) -> Option<Vec<u32>> {
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

pub(super) const fn topology_of(mode: u32) -> Topology {
    match mode {
        primitive::POINTS => Topology::Points,
        primitive::LINES => Topology::Lines,
        primitive::LINE_STRIP | primitive::LINE_LOOP => Topology::LineStrip,
        primitive::TRIANGLE_STRIP => Topology::TriangleStrip,
        primitive::RECTS => Topology::Rects,
        _ => Topology::Triangles,
    }
}

pub(super) fn viewport_of(regs: &RegisterFile) -> [f32; 6] {
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
pub(super) fn export_slots(shader_mask: u32, exports: u8) -> [Option<u8>; 8] {
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

pub(super) fn pipeline_state(regs: &RegisterFile) -> PipelineState {
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

pub(super) const fn address_mode(bits: u32) -> AddressMode {
    match bits {
        0 => AddressMode::Repeat,
        1 | 3 | 5 | 7 => AddressMode::Mirror,
        6 => AddressMode::Border,
        _ => AddressMode::Clamp,
    }
}

pub(super) fn read_sampler(
    regs: &RegisterFile,
    index: u32,
    signature: TextureSignature,
) -> SamplerDesc {
    let word0 = regs.get(base::SAMPLER + index * 3);
    let word1 = regs.get(base::SAMPLER + index * 3 + 1);
    let non_filterable = signature.kind != TextureKind::Float;
    // As Cemu: only POINT (0) and ANISO_POINT (4) are nearest; every other mode filters.
    let filter = |bits: u32| !non_filterable && !matches!(bits & 7, 0 | 4);
    let (mag_linear, min_linear) = (filter((word0 >> 9) & 7), filter((word0 >> 12) & 7));
    let mip_filter = (word0 >> 17) & 3;
    // Without a mip filter only the base level is read, whatever the LOD range says (as Cemu).
    let (min_lod, max_lod) = if mip_filter == 0 {
        (0, 0)
    } else {
        ((word1 & 0x3FF) as u16, ((word1 >> 10) & 0x3FF) as u16)
    };
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
pub(super) fn border_color(regs: &RegisterFile, index: u32, kind: u32) -> BorderColor {
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

pub(super) const fn gx2_dim(dim: TextureDim) -> u32 {
    match dim {
        TextureDim::D2 => addrlib::dim::D2,
        TextureDim::D2Array => addrlib::dim::D2_ARRAY,
        TextureDim::D3 => addrlib::dim::D3,
        TextureDim::Cube => addrlib::dim::CUBE,
    }
}

pub(super) fn sampled_hash(bytes: &[u8]) -> u64 {
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
pub(super) fn mip_chain_size(words: &SurfaceWords, dim: TextureDim, mip_count: u32) -> u32 {
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

pub(super) fn decode_mips(
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
