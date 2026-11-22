//! Latte tiling: pixel coordinates → byte offsets (port of Cemu's
//! `LatteAddrLib_Coord`), and detiling whole mip levels.
#![allow(clippy::cast_possible_truncation, clippy::too_many_arguments)]

use crate::addrlib::{bank_swapped_width, rotation_from_tile_mode, tile_mode::*};

const BANKS: u32 = 4;
const PIPES: u32 = 2;
const PIPE_INTERLEAVE_BITS: u32 = 8;
const PIPES_BITS: u32 = 1;
const BANKS_BITS: u32 = 2;
const BANK_SWAP_ORDER: [u32; 4] = [0, 1, 3, 2];

/// Offset of a pixel inside its 8x8 (or 8x8x4) micro tile, in pixels.
fn pixel_index_in_micro_tile(
    x: u32,
    y: u32,
    z: u32,
    bpp: u32,
    tile_mode: u32,
    is_depth: bool,
) -> u32 {
    let bit = |v: u32, n: u32| (v >> n) & 1;
    let bits: [u32; 6] = if is_depth {
        [bit(x, 0), bit(y, 0), bit(x, 1), bit(y, 1), bit(x, 2), bit(y, 2)]
    } else {
        match bpp {
            8 => [bit(x, 0), bit(x, 1), bit(x, 2), bit(y, 1), bit(y, 0), bit(y, 2)],
            16 => [bit(x, 0), bit(x, 1), bit(x, 2), bit(y, 0), bit(y, 1), bit(y, 2)],
            64 => [bit(x, 0), bit(y, 0), bit(x, 1), bit(x, 2), bit(y, 1), bit(y, 2)],
            128 => [bit(y, 0), bit(x, 0), bit(x, 1), bit(x, 2), bit(y, 1), bit(y, 2)],
            _ => [bit(x, 0), bit(x, 1), bit(y, 0), bit(x, 2), bit(y, 1), bit(y, 2)],
        }
    };
    let mut index = bits[0]
        | (bits[1] << 1)
        | (bits[2] << 2)
        | (bits[3] << 3)
        | (bits[4] << 4)
        | (bits[5] << 5);
    if thickness(tile_mode) > 1 {
        index |= (bit(z, 0) << 6) | (bit(z, 1) << 7);
    }
    index
}

/// Everything constant while decoding one surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub tile_mode: u32,
    pub bpp: u32,
    pub pitch: u32,
    pub height: u32,
    pub samples: u32,
    pub is_depth: bool,
    /// Pipe swizzle bits of the base address.
    pub pipe_swizzle: u32,
    pub bank_swizzle: u32,
}

impl Layout {
    /// Byte offset of pixel `(x, y)` of `slice` relative to the surface base.
    pub fn address(&self, x: u32, y: u32, slice: u32) -> u32 {
        match self.tile_mode {
            TM_LINEAR_GENERAL | TM_LINEAR_ALIGNED => {
                let index = u64::from(x)
                    + u64::from(self.pitch) * u64::from(y)
                    + u64::from(slice) * u64::from(self.height) * u64::from(self.pitch);
                ((index * u64::from(self.bpp)) / 8) as u32
            }
            TM_1D_TILED_THIN1 | TM_1D_TILED_THICK => self.address_micro(x, y, slice),
            _ => self.address_macro(x, y, slice),
        }
    }

    fn address_micro(&self, x: u32, y: u32, slice: u32) -> u32 {
        let depth = thickness(self.tile_mode);
        let tile_bytes = depth * (((self.bpp << 6) + 7) >> 3);
        let tile_offset = tile_bytes * ((x >> 3) + (self.pitch >> 3) * (y >> 3));
        let slice_bytes = (u64::from(self.height)
            * u64::from(self.pitch)
            * u64::from(depth)
            * u64::from(self.bpp))
        .div_ceil(8) as u32;
        let slice_offset = slice_bytes * (slice / depth);
        let index = pixel_index_in_micro_tile(x, y, slice, self.bpp, self.tile_mode, self.is_depth);
        ((self.bpp * index) >> 3) + tile_offset + slice_offset
    }

    fn address_macro(&self, x: u32, y: u32, slice: u32) -> u32 {
        let depth = thickness(self.tile_mode);
        let samples = self.samples;
        let index = pixel_index_in_micro_tile(x, y, slice, self.bpp, self.tile_mode, self.is_depth);
        let pixel_offset =
            if self.is_depth { samples * self.bpp * index } else { self.bpp * index };
        let element_offset = pixel_offset >> 3;

        let pipe = ((y >> 3) ^ (x >> 3)) & 1;
        let bank_bits = (y >> 4) & 3;
        let bank = (((bank_bits >> 1) | (bank_bits << 1)) ^ (x >> 3)) & 3;
        let mut bank_pipe = pipe + PIPES * bank;
        let rotation = rotation_from_tile_mode(self.tile_mode);
        let swizzle = self.pipe_swizzle + PIPES * self.bank_swizzle;
        let slice_in = if is_thick_and_macro_tiled(self.tile_mode) { slice >> 2 } else { slice };
        bank_pipe ^= swizzle + slice_in * rotation;
        bank_pipe %= PIPES * BANKS;
        let pipe = bank_pipe % PIPES;
        let mut bank = bank_pipe / PIPES;

        let slice_bytes = (u64::from(self.height)
            * u64::from(self.pitch)
            * u64::from(depth)
            * u64::from(self.bpp)
            * u64::from(samples))
        .div_ceil(8);
        let slice_offset = slice_bytes * u64::from(slice / depth);
        let (mut macro_pitch, mut macro_height) = (8 * BANKS, 8 * PIPES);
        match self.tile_mode {
            TM_2D_TILED_THIN2 | TM_2B_TILED_THIN2 => {
                macro_pitch >>= 1;
                macro_height <<= 1;
            }
            TM_2D_TILED_THIN4 | TM_2B_TILED_THIN4 => {
                macro_pitch >>= 2;
                macro_height <<= 2;
            }
            _ => {}
        }
        let macro_bytes = (samples * depth * self.bpp * macro_height * macro_pitch + 7) >> 3;
        let macro_x = x / macro_pitch;
        let macro_offset = u64::from(macro_x + self.pitch / macro_pitch * (y / macro_height))
            * u64::from(macro_bytes);
        if is_bank_swapped(self.tile_mode) {
            let swap_width =
                bank_swapped_width(self.tile_mode, self.bpp, samples, self.pitch).max(1);
            let swap_index = macro_pitch * macro_x / swap_width;
            bank ^= BANK_SWAP_ORDER[(swap_index & (BANKS - 1)) as usize];
        }
        let pipe_offset = pipe << PIPE_INTERLEAVE_BITS;
        let bank_offset = bank << (PIPES_BITS + PIPE_INTERLEAVE_BITS);
        let swizzle_bits = BANKS_BITS + PIPES_BITS;
        let slice_part = ((macro_offset + slice_offset) >> swizzle_bits) as u32 + element_offset;
        let high = slice_part & !((1 << PIPE_INTERLEAVE_BITS) - 1);
        let low = slice_part & ((1 << PIPE_INTERLEAVE_BITS) - 1);
        (high << swizzle_bits) | low | pipe_offset | bank_offset
    }
}

impl Layout {
    /// Calls `visit(x, y, byte offset)` for every pixel, one micro tile at a time.
    ///
    /// Only the tile's first pixel needs the full address calculation: within a
    /// tile the offsets depend on the low coordinate bits alone.
    pub fn for_each_pixel(
        &self,
        width: u32,
        height: u32,
        slice: u32,
        mut visit: impl FnMut(u32, u32, u32),
    ) {
        let origin = self.address(0, 0, slice);
        let pattern: [u32; 64] = std::array::from_fn(|i| {
            self.address(i as u32 % 8, i as u32 / 8, slice).wrapping_sub(origin)
        });
        for tile_y in (0..height).step_by(8) {
            for tile_x in (0..width).step_by(8) {
                let base = self.address(tile_x, tile_y, slice);
                for y in 0..(height - tile_y).min(8) {
                    for x in 0..(width - tile_x).min(8) {
                        let offset = base.wrapping_add(pattern[(y * 8 + x) as usize]);
                        visit(tile_x + x, tile_y + y, offset);
                    }
                }
            }
        }
    }
}

/// Splits the low address bits Latte uses as pipe/bank swizzle.
#[inline]
pub const fn swizzle_from_address(address: u32) -> (u32, u32) {
    ((address >> 8) & 1, (address >> 9) & 3)
}

/// Copies pixels of `(width, height)` (block counts for compressed formats) of
/// one slice from tiled `source` to tightly packed `out`.
pub fn detile(
    source: &[u8],
    layout: &Layout,
    width: u32,
    height: u32,
    slice: u32,
    out: &mut Vec<u8>,
) {
    let bytes = (layout.bpp / 8) as usize;
    let start = out.len();
    out.resize(start + width as usize * height as usize * bytes, 0);
    let target = &mut out[start..];
    if matches!(layout.tile_mode, TM_LINEAR_GENERAL | TM_LINEAR_ALIGNED) {
        for y in 0..height {
            let offset = layout.address(0, y, slice) as usize;
            let row = y as usize * width as usize * bytes;
            let end = offset + width as usize * bytes;
            if let Some(line) = source.get(offset..end) {
                target[row..row + width as usize * bytes].copy_from_slice(line);
            }
        }
        return;
    }
    layout.for_each_pixel(width, height, slice, |x, y, offset| {
        let out_offset = (y as usize * width as usize + x as usize) * bytes;
        if let Some(pixel) = source.get(offset as usize..offset as usize + bytes) {
            target[out_offset..out_offset + bytes].copy_from_slice(pixel);
        }
    });
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_addresses_follow_the_pitch() {
        let layout = Layout {
            tile_mode: TM_LINEAR_ALIGNED,
            bpp: 32,
            pitch: 16,
            height: 8,
            samples: 1,
            is_depth: false,
            pipe_swizzle: 0,
            bank_swizzle: 0,
        };
        assert_eq!(layout.address(3, 2, 0), (3 + 2 * 16) * 4);
    }

    #[test]
    fn micro_tile_layout_of_32_bit_pixels() {
        let layout = Layout {
            tile_mode: TM_1D_TILED_THIN1,
            bpp: 32,
            pitch: 64,
            height: 64,
            samples: 1,
            is_depth: false,
            pipe_swizzle: 0,
            bank_swizzle: 0,
        };
        // x bit 0, x bit 1, y bit 0, x bit 2, y bit 1, y bit 2.
        assert_eq!(layout.address(1, 0, 0), 4);
        assert_eq!(layout.address(0, 1, 0), 4 * 4);
        assert_eq!(layout.address(4, 0, 0), 8 * 4);
        // Second tile of the row.
        assert_eq!(layout.address(8, 0, 0), 64 * 4);
    }

    #[test]
    fn tile_walk_matches_the_address_of_every_pixel() {
        let modes = [
            TM_1D_TILED_THIN1,
            TM_1D_TILED_THICK,
            TM_2D_TILED_THIN1,
            TM_2D_TILED_THIN2,
            TM_2D_TILED_THIN4,
            TM_2B_TILED_THIN1,
            TM_2B_TILED_THIN2,
            TM_2D_TILED_THICK,
            TM_3D_TILED_THIN1,
            TM_3B_TILED_THICK,
        ];
        for tile_mode in modes {
            for bpp in [8, 16, 32, 64, 128] {
                for (pipe_swizzle, bank_swizzle, slice) in [(0, 0, 0), (1, 2, 0), (1, 3, 5)] {
                    let layout = Layout {
                        tile_mode,
                        bpp,
                        pitch: 256,
                        height: 128,
                        samples: 1,
                        is_depth: false,
                        pipe_swizzle,
                        bank_swizzle,
                    };
                    let mut visited = 0;
                    layout.for_each_pixel(250, 120, slice, |x, y, offset| {
                        assert_eq!(
                            offset,
                            layout.address(x, y, slice),
                            "mode {tile_mode} bpp {bpp} ({x},{y}) slice {slice}"
                        );
                        visited += 1;
                    });
                    assert_eq!(visited, 250 * 120);
                }
            }
        }
    }

    #[test]
    fn macro_tiling_covers_every_byte_exactly_once() {
        let layout = Layout {
            tile_mode: TM_2D_TILED_THIN1,
            bpp: 32,
            pitch: 64,
            height: 64,
            samples: 1,
            is_depth: false,
            pipe_swizzle: 0,
            bank_swizzle: 0,
        };
        let mut seen = std::collections::HashSet::new();
        for y in 0..64 {
            for x in 0..64 {
                let address = layout.address(x, y, 0);
                assert_eq!(address % 4, 0);
                assert!(address < 64 * 64 * 4, "({x},{y}) -> {address:#x}");
                assert!(seen.insert(address), "({x},{y}) collides at {address:#x}");
            }
        }
    }
}
