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
//! │ image size &       │   │ addrlib/surface_info.rs  │   │ pitch, size,       │
//! │ format             │──►│ ──────────────────────── │──►│ alignment          │
//! │                    │   │ address library: surface │   │                    │
//! │                    │   │ info                     │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "surface info" share of mod.rs (address library), moved here so that no file is longer
//! than 500 lines. It works exactly as if it were still written in that file. AMD's rules for
//! how big an image is in memory and how it is aligned, ported exactly. Terms such as HLE, JIT
//! or shader are explained in `docs/guide/wiiu_term.md`.

use super::*;

pub(super) fn info_linear(
    tile_mode: u32,
    bpp: u32,
    samples: u32,
    pitch: u32,
    height: u32,
    slices: u32,
    level: u32,
    mut pad_dims: u32,
    flags: Flags,
) -> SurfaceInfo {
    let (base_align, mut pitch_align, height_align) = if tile_mode == TM_LINEAR_ALIGNED {
        (PIPE_INTERLEAVE_BYTES, 64.max(8 * PIPE_INTERLEAVE_BYTES / bpp), 1)
    } else {
        (1, 1, 1)
    };
    if flags.display {
        pitch_align = pow2_align(pitch_align, 32);
    }
    let (mut p, mut h, mut s) = (pitch, height, slices);
    if flags.linear_wa && level == 0 {
        p = next_pow2(p / 3);
    }
    if level != 0 {
        p = next_pow2(p);
        h = next_pow2(h);
        if flags.dim_cube {
            s = slices;
            pad_dims = if slices <= 1 { 2 } else { 0 };
        } else {
            s = next_pow2(slices);
        }
    }
    let thick = thickness(tile_mode);
    pad_dimensions(
        tile_mode,
        pad_dims,
        flags.dim_cube,
        &mut p,
        pitch_align,
        &mut h,
        height_align,
        &mut s,
        thick,
    );
    if flags.linear_wa && level == 0 {
        p *= 3;
    }
    let slice_count = s * samples / thick;
    SurfaceInfo {
        pitch: p,
        height: h,
        depth: s,
        surf_size: (u64::from(h)
            * u64::from(p)
            * u64::from(slice_count)
            * u64::from(bpp)
            * u64::from(samples))
        .div_ceil(8),
        hw_tile_mode: tile_mode,
        base_align,
        pitch_align,
        height_align,
        depth_align: thick,
        ..SurfaceInfo::default()
    }
}

pub(super) fn info_micro_tiled(
    tile_mode: u32,
    bpp: u32,
    samples: u32,
    pitch: u32,
    height: u32,
    slices: u32,
    level: u32,
    mut pad_dims: u32,
    flags: Flags,
) -> SurfaceInfo {
    let mut mode = tile_mode;
    let (mut p, mut h, mut s) = (pitch, height, slices);
    let mut thick = thickness(tile_mode);
    if level != 0 {
        p = next_pow2(pitch);
        h = next_pow2(height);
        if flags.dim_cube {
            s = slices;
            pad_dims = if slices <= 1 { 2 } else { 0 };
        } else {
            s = next_pow2(slices);
        }
        if mode == TM_1D_TILED_THICK && s < 4 {
            mode = TM_1D_TILED_THIN1;
            thick = 1;
        }
    }
    let b = if matches!(bpp, 96 | 48 | 24) { bpp / 3 } else { bpp };
    let base_align = PIPE_INTERLEAVE_BYTES;
    let mut pitch_align = 8.max(PIPE_INTERLEAVE_BYTES / b / samples / thickness(mode));
    let height_align = 8;
    if flags.display {
        pitch_align = pow2_align(pitch_align, 32);
    }
    pad_dimensions(
        mode,
        pad_dims,
        flags.dim_cube,
        &mut p,
        pitch_align,
        &mut h,
        height_align,
        &mut s,
        thick,
    );
    SurfaceInfo {
        pitch: p,
        height: h,
        depth: s,
        surf_size: (u64::from(h)
            * u64::from(p)
            * u64::from(s)
            * u64::from(bpp)
            * u64::from(samples))
        .div_ceil(8),
        hw_tile_mode: mode,
        base_align,
        pitch_align,
        height_align,
        depth_align: thick,
        ..SurfaceInfo::default()
    }
}

pub(super) fn info_macro_tiled(
    tile_mode: u32,
    base_tile_mode: u32,
    bpp: u32,
    samples: u32,
    pitch: u32,
    height: u32,
    slices: u32,
    level: u32,
    mut pad_dims: u32,
    flags: Flags,
) -> SurfaceInfo {
    let (mut p, mut h, mut s) = (pitch, height, slices);
    let mut mode = tile_mode;
    let mut thick = thickness(tile_mode);
    if level != 0 {
        p = next_pow2(pitch);
        h = next_pow2(height);
        s = next_pow2(slices);
        if flags.dim_cube {
            s = slices;
            pad_dims = if slices <= 1 { 2 } else { 0 };
        }
        if mode == TM_2D_TILED_THICK && s < 4 {
            mode = TM_2D_TILED_THIN1;
            thick = 1;
        }
    }
    let pitch_align_factor = ((PIPE_INTERLEAVE_BYTES >> 3) / bpp).max(1);
    if tile_mode != base_tile_mode
        && level != 0
        && is_thick_and_macro_tiled(base_tile_mode)
        && !is_thick_and_macro_tiled(tile_mode)
    {
        let (_, pitch_align, height_align) = alignments_macro_tiled(base_tile_mode, bpp, samples);
        if p < pitch_align * pitch_align_factor || h < height_align {
            return info_micro_tiled(
                TM_1D_TILED_THIN1,
                bpp,
                samples,
                pitch,
                height,
                slices,
                level,
                pad_dims,
                flags,
            );
        }
    }
    let (base_align, mut pitch_align, height_align) =
        alignments_macro_tiled(tile_mode, bpp, samples);
    let swapped = bank_swapped_width(tile_mode, bpp, samples, pitch);
    if swapped > pitch_align {
        pitch_align = swapped;
    }
    pad_dimensions(
        tile_mode,
        pad_dims,
        flags.dim_cube,
        &mut p,
        pitch_align,
        &mut h,
        height_align,
        &mut s,
        thick,
    );
    SurfaceInfo {
        pitch: p,
        height: h,
        depth: s,
        surf_size: (u64::from(h)
            * u64::from(p)
            * u64::from(s)
            * u64::from(bpp)
            * u64::from(samples))
        .div_ceil(8),
        hw_tile_mode: mode,
        base_align,
        pitch_align,
        height_align,
        depth_align: thick,
        ..SurfaceInfo::default()
    }
}

/// `ComputeSurfaceInfo`: layout of mip `level` described by the inputs.
pub fn compute_surface_info(
    tile_mode: u32,
    format: u32,
    mut bpp: u32,
    samples: u32,
    mut width: u32,
    mut height: u32,
    mut slices: u32,
    slice: u32,
    level: u32,
    mut flags: Flags,
) -> SurfaceInfo {
    compute_mip_level_dimensions(&mut width, &mut height, &mut slices, flags, format, level);
    let pixel_bits = bpp;
    let (mut elem_mode, mut expand_x, mut expand_y) = (3, 1, 1);
    if format != 0 {
        let (b, e, x, y) = bits_per_pixel(format);
        (bpp, elem_mode, expand_x, expand_y) = (b, e, x, y);
        if tile_mode == TM_LINEAR_ALIGNED && elem_mode == 4 && expand_x == 3 {
            flags.linear_wa = true;
        }
        adjust_surface_info(elem_mode, expand_x, expand_y, &mut bpp, &mut width, &mut height);
    } else if bpp != 0 {
        width = width.max(1);
        height = height.max(1);
    } else {
        return SurfaceInfo::default();
    }

    let samples = samples.max(1);
    let pad_dims = if flags.dim_cube && level == 0 { 2 } else { 0 };
    let mode = if flags.fmask {
        non_bank_swapped(tile_mode)
    } else {
        mip_level_tile_mode(
            tile_mode,
            bpp,
            level,
            width,
            height,
            slices,
            samples,
            flags.depth,
            false,
        )
    };
    let mut out = match mode {
        TM_LINEAR_GENERAL | TM_LINEAR_ALIGNED => {
            info_linear(mode, bpp, samples, width, height, slices, level, pad_dims, flags)
        }
        TM_1D_TILED_THIN1 | TM_1D_TILED_THICK => {
            info_micro_tiled(mode, bpp, samples, width, height, slices, level, pad_dims, flags)
        }
        TM_2D_TILED_THIN1..=TM_3B_TILED_THICK => info_macro_tiled(
            mode, tile_mode, bpp, samples, width, height, slices, level, pad_dims, flags,
        ),
        _ => return SurfaceInfo::default(),
    };
    out.bpp = bpp;
    out.pixel_bits = pixel_bits;
    out.pixel_pitch = out.pitch;
    out.pixel_height = out.height;
    if format != 0 && (!flags.linear_wa || level == 0) {
        restore_surface_info(
            elem_mode,
            expand_x,
            expand_y,
            &mut out.pixel_pitch,
            &mut out.pixel_height,
        );
    }
    out.slice_size = if flags.dim_3d {
        out.surf_size as u32
    } else if out.depth == 0 {
        0
    } else {
        let mut size = (out.surf_size / u64::from(out.depth)) as u32;
        if slice + 1 == slices && slices > 1 {
            size += size * (out.depth - slices);
        }
        size
    };
    out
}

/// `GX2CalculateSurfaceInfo`: layout of mip `level` of a GX2 surface.
pub fn gx2_surface_info(
    format: u32,
    width: u32,
    height: u32,
    depth: u32,
    surface_dim: u32,
    gx2_tile_mode: u32,
    aa: u32,
    level: u32,
    is_depth: bool,
    is_scan_buffer: bool,
) -> SurfaceInfo {
    let hw_format = format & 0x3F;
    if gx2_tile_mode == TM_LINEAR_SPECIAL {
        return linear_special(hw_format, width, height, depth, surface_dim, aa, level);
    }
    let tile_mode = gx2_tile_mode & 0xF;
    let samples = 1 << aa;
    let w = (width >> level).max(1);
    let h = (height >> level).max(1);
    let mut flags = Flags {
        input_is_base: level == 0,
        depth: is_depth,
        display: is_scan_buffer,
        ..Flags::default()
    };
    let (h, slices) = match surface_dim {
        dim::D1 => (1, 1),
        dim::D2 | dim::D2_MSAA => (h, 1),
        dim::D3 => {
            flags.dim_3d = true;
            (h, (depth >> level).max(1))
        }
        dim::CUBE => {
            flags.dim_cube = true;
            (h, depth.max(6))
        }
        dim::D1_ARRAY => (1, depth),
        dim::D2_ARRAY | dim::D2_ARRAY_MSAA => (h, depth),
        _ => (h, 1),
    };
    compute_surface_info(
        tile_mode,
        hw_format,
        format_bits(hw_format),
        samples,
        w,
        h,
        slices,
        0,
        level,
        flags,
    )
}

pub(super) fn linear_special(
    hw_format: u32,
    width: u32,
    height: u32,
    depth: u32,
    surface_dim: u32,
    aa: u32,
    level: u32,
) -> SurfaceInfo {
    let samples: u32 = 1 << aa;
    let block = if is_compressed(hw_format) { 4 } else { 1 };
    let bpp = format_bits(hw_format);
    let w = pow2_align(width >> level, block);
    let mut out = SurfaceInfo {
        bpp,
        pitch: w / block,
        pixel_bits: bpp,
        base_align: 1,
        pitch_align: 1,
        height_align: 1,
        depth_align: 1,
        ..SurfaceInfo::default()
    };
    (out.height, out.depth) = match surface_dim {
        dim::D1 => (1, 1),
        dim::D2 => ((height >> level).max(1), 1),
        dim::D3 => ((height >> level).max(1), (depth >> level).max(1)),
        dim::CUBE => ((height >> level).max(1), depth.max(6)),
        dim::D1_ARRAY => (1, depth),
        dim::D2_ARRAY => ((height >> level).max(1), depth),
        _ => (0, 0),
    };
    out.height = pow2_align(out.height, block) / block;
    out.pixel_pitch = pow2_align(width >> level, block).max(block);
    out.pixel_height = pow2_align(height >> level, block).max(block);
    out.pitch = out.pitch.max(1);
    out.height = out.height.max(1);
    out.surf_size = (u64::from(out.bpp)
        * u64::from(samples)
        * u64::from(out.depth)
        * u64::from(out.height)
        * u64::from(out.pitch))
        >> 3;
    out.slice_size = if surface_dim == dim::D3 {
        out.surf_size as u32
    } else if out.depth == 0 {
        0
    } else {
        (out.surf_size / u64::from(out.depth)) as u32
    };
    out
}
