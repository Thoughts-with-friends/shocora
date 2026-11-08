//! `GX2Surface` and the surface layout exports (port of Cemu's GX2_Surface).
#![allow(clippy::cast_possible_truncation)]

use wiiu_gpu::addrlib::{self, SurfaceInfo, dim, tile_mode};
use wiiu_hle_macros::hle_export;
use wiiu_memory::{GuestPtr, Memory, MemoryError};

use crate::hle::{HleContext, HleError};

/// `GX2_RESFLAG_USAGE_*`.
pub mod resource_flag {
    pub const COLOR_BUFFER: u32 = 1 << 1;
    pub const DEPTH_BUFFER: u32 = 1 << 2;
    pub const SCAN_BUFFER: u32 = 1 << 3;
}

/// `GX2SurfaceFormat` NV12 (video).
const FORMAT_NV12: u32 = 0x81;

/// All fields of `GX2Surface` (0x74 bytes).
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Gx2Surface {
    pub dim: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub num_levels: u32,
    pub format: u32,
    pub aa: u32,
    pub resource_flags: u32,
    pub image_size: u32,
    pub image: u32,
    pub mip_size: u32,
    pub mip_ptr: u32,
    pub tile_mode: u32,
    pub swizzle: u32,
    pub alignment: u32,
    pub pitch: u32,
    pub mip_offset: [u32; 13],
}

impl Gx2Surface {
    pub const SIZE: u32 = 0x74;

    /// Reads the surface at `address` (also the start of `GX2ColorBuffer`/`GX2Texture`).
    ///
    /// # Errors
    /// Fails if the structure is unmapped.
    pub fn read(memory: &Memory, address: u32) -> Result<Self, MemoryError> {
        let field = |i: u32| memory.read_u32(address + 4 * i);
        let mut mip_offset = [0; 13];
        for (i, offset) in mip_offset.iter_mut().enumerate() {
            *offset = field(16 + i as u32)?;
        }
        Ok(Self {
            dim: field(0)?,
            width: field(1)?,
            height: field(2)?,
            depth: field(3)?,
            num_levels: field(4)?,
            format: field(5)?,
            aa: field(6)?,
            resource_flags: field(7)?,
            image_size: field(8)?,
            image: field(9)?,
            mip_size: field(10)?,
            mip_ptr: field(11)?,
            tile_mode: field(12)?,
            swizzle: field(13)?,
            alignment: field(14)?,
            pitch: field(15)?,
            mip_offset,
        })
    }

    /// # Errors
    /// Fails if the structure is unmapped.
    pub fn write(&self, memory: &mut Memory, address: u32) -> Result<(), MemoryError> {
        let words = [
            self.dim,
            self.width,
            self.height,
            self.depth,
            self.num_levels,
            self.format,
            self.aa,
            self.resource_flags,
            self.image_size,
            self.image,
            self.mip_size,
            self.mip_ptr,
            self.tile_mode,
            self.swizzle,
            self.alignment,
            self.pitch,
        ];
        for (i, word) in words.iter().chain(self.mip_offset.iter()).enumerate() {
            memory.write_u32(address + 4 * i as u32, *word)?;
        }
        Ok(())
    }

    /// Layout of mip `level`.
    pub fn info(&self, level: u32) -> SurfaceInfo {
        addrlib::gx2_surface_info(
            self.format,
            self.width,
            self.height,
            self.depth,
            self.dim,
            self.tile_mode,
            self.aa,
            level,
            self.resource_flags & resource_flag::DEPTH_BUFFER != 0,
            self.resource_flags & resource_flag::SCAN_BUFFER != 0,
        )
    }

    fn level_count_limit(&self) -> u32 {
        if self.num_levels <= 1 {
            return 1;
        }
        let bits = |v: u32| 32 - v.leading_zeros();
        let mut levels = bits(self.width).max(bits(self.height));
        if self.dim == dim::D3 {
            levels = levels.max(bits(self.depth));
        }
        levels
    }

    /// `GX2CalcSurfaceSizeAndAlignment`: fills size, alignment, pitch, tile
    /// mode, swizzle and mip offsets.
    pub fn calc_size_and_alignment(&mut self) {
        if self.dim >= 50
            || self.aa >= 0x100
            || self.width >= 0x0100_0000
            || self.height >= 0x0100_0000
            || self.format >= 0x1_0000
        {
            log::warn!("GX2CalcSurfaceSizeAndAlignment: uninitialised surface");
            *self = Self {
                dim: dim::D2,
                width: 8,
                height: 8,
                depth: 1,
                tile_mode: tile_mode::TM_2D_TILED_THIN1,
                pitch: 8,
                format: 0x1A,
                alignment: 0x400,
                ..*self
            };
        }
        let mut change_tile_mode = false;
        let mut last_tile_mode = self.tile_mode;
        let has_mode32 = self.tile_mode == tile_mode::TM_32_SPECIAL;
        if self.tile_mode == tile_mode::TM_LINEAR_GENERAL || has_mode32 {
            if self.dim != dim::D1
                || self.resource_flags & resource_flag::DEPTH_BUFFER != 0
                || self.aa != 0
            {
                self.tile_mode = if self.dim != dim::D3
                    || self.resource_flags & resource_flag::COLOR_BUFFER != 0
                {
                    tile_mode::TM_2D_TILED_THIN1
                } else {
                    tile_mode::TM_2D_TILED_THICK
                };
                change_tile_mode = true;
            } else {
                self.tile_mode = tile_mode::TM_LINEAR_ALIGNED;
            }
            last_tile_mode = self.tile_mode;
        }
        if self.num_levels == 0 {
            self.num_levels = 1;
        }
        self.num_levels = self.num_levels.min(self.level_count_limit());
        self.mip_offset[0] = 0;
        self.swizzle = if tile_mode::is_macro_tiled(self.tile_mode) {
            (self.swizzle & 0xFF00_FFFF) | 0xD_0000
        } else {
            self.swizzle & 0xFF00_FFFF
        };
        let fix32 = if has_mode32 && addrlib::is_compressed(self.format) { 2 } else { 0 };

        let mut first_mip_offset = 0;
        let mut prev_size = 0_u32;
        for level in 0..self.num_levels {
            let mut info = self.info(level);
            if level == 0 {
                if change_tile_mode {
                    if self.tile_mode != info.hw_tile_mode {
                        self.tile_mode = info.hw_tile_mode;
                        info = self.info(0);
                        if !tile_mode::is_macro_tiled(self.tile_mode) {
                            self.swizzle &= 0xFF00_FFFF;
                        }
                        last_tile_mode = self.tile_mode;
                    }
                    if self.width < (info.pitch_align << fix32)
                        && self.height < (info.height_align << fix32)
                    {
                        self.tile_mode = if self.tile_mode == tile_mode::TM_2D_TILED_THICK {
                            tile_mode::TM_1D_TILED_THICK
                        } else {
                            tile_mode::TM_1D_TILED_THIN1
                        };
                        info = self.info(0);
                        self.swizzle &= 0xFF00_FFFF;
                        last_tile_mode = self.tile_mode;
                    }
                }
                self.image_size = info.surf_size as u32;
                self.alignment = info.base_align;
                self.pitch = info.pitch;
            } else {
                let mut pad = 0;
                if tile_mode::is_macro_tiled(last_tile_mode)
                    && !tile_mode::is_macro_tiled(info.hw_tile_mode)
                {
                    self.swizzle = (self.swizzle & 0xFF00_FFFF) | (level << 16);
                    last_tile_mode = info.hw_tile_mode;
                    if level > 1 {
                        pad = self.swizzle & 0xFFFF;
                    }
                }
                let align = info.base_align.max(1);
                pad += (align - prev_size % align) % align;
                if level == 1 {
                    first_mip_offset = pad + prev_size;
                } else {
                    let i = level as usize;
                    self.mip_offset[i - 1] = pad + prev_size + self.mip_offset[i - 2];
                }
            }
            prev_size = info.surf_size as u32;
        }
        self.mip_size = if self.num_levels > 1 {
            prev_size + self.mip_offset[self.num_levels as usize - 2]
        } else {
            0
        };
        self.mip_offset[0] = first_mip_offset;
        if self.format == FORMAT_NV12 {
            let align = self.alignment.max(1);
            let padding = (align - self.image_size % align) % align;
            self.mip_offset[0] = padding + self.image_size;
            self.image_size = self.mip_offset[0] + (self.image_size >> 1);
        }
    }
}

#[hle_export(lib = "gx2", name = "GX2CalcSurfaceSizeAndAlignment")]
fn gx2_calc_surface_size_and_alignment(
    ctx: &mut HleContext<'_>,
    surface: u32,
) -> Result<(), HleError> {
    let mut s = Gx2Surface::read(ctx.memory, surface)?;
    s.calc_size_and_alignment();
    s.write(ctx.memory, surface)?;
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2GetSurfaceMipPitch")]
fn gx2_get_surface_mip_pitch(
    ctx: &mut HleContext<'_>,
    surface: u32,
    level: u32,
) -> Result<u32, HleError> {
    Ok(Gx2Surface::read(ctx.memory, surface)?.info(level).pitch)
}

#[hle_export(lib = "gx2", name = "GX2GetSurfaceMipSliceSize")]
fn gx2_get_surface_mip_slice_size(
    ctx: &mut HleContext<'_>,
    surface: u32,
    level: u32,
) -> Result<u32, HleError> {
    let s = Gx2Surface::read(ctx.memory, surface)?;
    let info = s.info(level);
    Ok((1 << s.aa) * (info.bpp >> 3) * info.height * info.pitch)
}

#[hle_export(lib = "gx2", name = "GX2GetSurfaceFormatBits")]
fn gx2_get_surface_format_bits(_ctx: &mut HleContext<'_>, format: u32) -> u32 {
    let bits = addrlib::format_bits(format);
    if addrlib::is_compressed(format) { bits / 16 } else { bits }
}

#[hle_export(lib = "gx2", name = "GX2GetSurfaceSwizzleOffset")]
fn gx2_get_surface_swizzle_offset(
    ctx: &mut HleContext<'_>,
    surface: u32,
    level: u32,
) -> Result<u32, HleError> {
    let s = Gx2Surface::read(ctx.memory, surface)?;
    if !tile_mode::is_macro_tiled(s.tile_mode) || level >= ((s.swizzle >> 16) & 0xFF) {
        return Ok(0);
    }
    Ok(s.swizzle & 0xFFFF)
}

#[hle_export(lib = "gx2", name = "GX2GetSurfaceSwizzle")]
fn gx2_get_surface_swizzle(ctx: &mut HleContext<'_>, surface: u32) -> Result<u32, HleError> {
    Ok((Gx2Surface::read(ctx.memory, surface)?.swizzle >> 8) & 0xFF)
}

#[hle_export(lib = "gx2", name = "GX2SetSurfaceSwizzle")]
fn gx2_set_surface_swizzle(
    ctx: &mut HleContext<'_>,
    surface: u32,
    swizzle: u32,
) -> Result<(), HleError> {
    let field = surface + 0x34;
    let current = ctx.memory.read_u32(field)?;
    Ok(ctx.memory.write_u32(field, (current & !0xFF00) | (swizzle << 8))?)
}

#[hle_export(lib = "gx2", name = "GX2SurfaceIsCompressed")]
fn gx2_surface_is_compressed(_ctx: &mut HleContext<'_>, format: u32) -> bool {
    addrlib::is_compressed(format)
}

#[hle_export(lib = "gx2", name = "GX2CheckSurfaceUseVsFormat")]
fn gx2_check_surface_use_vs_format(_ctx: &mut HleContext<'_>, _flags: u32, _format: u32) -> u32 {
    1
}

fn write_size_align(
    ctx: &mut HleContext<'_>,
    size: GuestPtr<u32>,
    align: GuestPtr<u32>,
) -> Result<(), HleError> {
    size.write(ctx.memory, 0x1000)?;
    align.write(ctx.memory, 0x100)?;
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2CalcDepthBufferHiZInfo")]
fn gx2_calc_depth_buffer_hiz_info(
    ctx: &mut HleContext<'_>,
    _buffer: u32,
    size: GuestPtr<u32>,
    align: GuestPtr<u32>,
) -> Result<(), HleError> {
    write_size_align(ctx, size, align)
}

#[hle_export(lib = "gx2", name = "GX2CalcColorBufferAuxInfo")]
fn gx2_calc_color_buffer_aux_info(
    ctx: &mut HleContext<'_>,
    _buffer: u32,
    size: GuestPtr<u32>,
    align: GuestPtr<u32>,
) -> Result<(), HleError> {
    write_size_align(ctx, size, align)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calc_fills_layout_like_cemu() {
        let mut s = Gx2Surface {
            dim: dim::D2,
            width: 1280,
            height: 720,
            depth: 1,
            num_levels: 1,
            format: 0x1A,
            resource_flags: resource_flag::COLOR_BUFFER,
            tile_mode: tile_mode::TM_LINEAR_GENERAL,
            ..Gx2Surface::default()
        };
        s.calc_size_and_alignment();
        assert_eq!(s.tile_mode, tile_mode::TM_2D_TILED_THIN1);
        assert_eq!((s.pitch, s.image_size, s.alignment), (1280, 1280 * 720 * 4, 0x800));
        assert_eq!(s.swizzle & 0x00FF_0000, 0xD_0000);

        let mut tex = Gx2Surface {
            dim: dim::D2,
            width: 64,
            height: 64,
            depth: 1,
            num_levels: 4,
            format: 0x1A,
            tile_mode: tile_mode::TM_2D_TILED_THIN1,
            ..Gx2Surface::default()
        };
        tex.calc_size_and_alignment();
        assert_eq!(tex.num_levels, 4);
        assert!(tex.mip_size > 0);
        // mipOffset[0] is relative to the image, the rest to mipPtr (as in Cafe OS).
        assert!(tex.mip_offset[0] >= tex.image_size);
        assert!(tex.mip_offset[1] > 0 && tex.mip_offset[2] > tex.mip_offset[1]);
    }
}
