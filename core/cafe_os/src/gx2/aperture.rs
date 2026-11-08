//! Tiling apertures: a linear window onto one level of a tiled surface.
//!
//! A port of Cemu's GX2_TilingAperture. The hardware translates every access; here
//! the texels are copied in on allocation and written back on free.
use wiiu_gpu::{addrlib, tiling::Layout};
use wiiu_hle_macros::hle_export;
use wiiu_mem_addr::TILING_APERTURE;
use wiiu_memory::{GuestPtr, Memory, MemoryError};

use super::Gx2Surface;
use crate::hle::{HleContext, HleError};

/// An open aperture.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy)]
struct Aperture {
    address: u32,
    size: u32,
    handle: u32,
    surface: Gx2Surface,
    level: u32,
    slice: u32,
}

/// Levels recently retiled. Titles reading a render target through an aperture
/// every frame find it unchanged, as the host renders outside guest memory:
/// comparing bytes is far cheaper than walking the tiles again. Several are kept
/// because titles go round their swap chain, a different surface every frame.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
struct RetileCache {
    /// Most recently used last.
    levels: Vec<CachedLevel>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct CachedLevel {
    /// `(image, level, slice)`.
    key: (u32, u32, u32),
    tiled: Vec<u8>,
    linear: Vec<u8>,
}

/// Levels kept: more than the longest swap chain (three buffers, TV and GamePad).
const CACHED_LEVELS: usize = 6;

impl RetileCache {
    fn position(&self, key: (u32, u32, u32)) -> Option<usize> {
        self.levels.iter().position(|level| level.key == key)
    }

    fn remember(&mut self, level: CachedLevel) {
        if let Some(index) = self.position(level.key) {
            self.levels.remove(index);
        } else if self.levels.len() >= CACHED_LEVELS {
            self.levels.remove(0);
        }
        self.levels.push(level);
    }

    /// Moves the level to the most recently used place.
    fn touch(&mut self, index: usize) {
        let level = self.levels.remove(index);
        self.levels.push(level);
    }
}

/// Open apertures of the process.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct Apertures {
    open: Vec<Aperture>,
    last_handle: u32,
    cache: RetileCache,
}

impl Apertures {
    /// First free page-aligned range of the aperture region.
    fn find_space(&self, size: u32) -> Option<u32> {
        let mut offset = 0_u32;
        loop {
            let start = (offset + 0xFFF) & !0xFFF;
            if u64::from(start) + u64::from(size) >= u64::from(TILING_APERTURE.size) {
                return None;
            }
            let address = TILING_APERTURE.start + start;
            let overlapping = self
                .open
                .iter()
                .filter(|a| address < a.address + a.size && a.address < address + size);
            match overlapping.map(|a| a.address + a.size).max() {
                Some(end) => offset = end - TILING_APERTURE.start,
                None => return Some(address),
            }
        }
    }
}

/// Copies a pixel when both ranges exist. The const length lets the copy compile
/// to a single move, which a loop over millions of pixels needs.
#[inline]
fn copy_pixel<const BYTES: usize>(from: &[u8], at: usize, to: &mut [u8], into: usize) {
    if let (Some(source), Some(target)) = (from.get(at..at + BYTES), to.get_mut(into..into + BYTES))
    {
        target.copy_from_slice(source);
    }
}

/// Copies the level's texels between the aperture and the surface.
fn retile(
    memory: &mut Memory,
    cache: &mut RetileCache,
    aperture: &Aperture,
    write_back: bool,
) -> Result<(), MemoryError> {
    let surface = &aperture.surface;
    let level = aperture.level;
    let image = match level {
        0 => surface.image,
        1 => surface.mip_ptr,
        _ => surface
            .mip_ptr
            .wrapping_add(surface.mip_offset.get(level as usize - 1).copied().unwrap_or(0)),
    };
    if image == 0 {
        return Ok(());
    }
    let info = surface.info(level);
    let layout = Layout {
        tile_mode: info.hw_tile_mode,
        bpp: info.bpp,
        pitch: info.pitch,
        height: info.height,
        samples: 1,
        is_depth: false,
        pipe_swizzle: (surface.swizzle >> 8) & 1,
        bank_swizzle: (surface.swizzle >> 9) & 3,
    };
    let step = if addrlib::is_compressed(surface.format) { 4 } else { 1 };
    let width = (surface.width >> level).max(1).div_ceil(step);
    let height = (surface.height >> level).max(1).div_ceil(step);
    let bytes = (info.bpp / 8) as usize;
    let pitch = info.pitch;
    let linear_at = move |x: u32, y: u32| {
        // 8-bit apertures are addressed with the swap of the hardware's endian mode.
        let (column, row) = if bytes == 1 { (x ^ 8, y ^ 2) } else { (x, y) };
        (column + row * pitch) as usize * bytes
    };
    // Tiling scatters pixels over the level, so the view spans all of it. The low bits of a slice's
    // first address are swizzle, which stays inside the level.
    let tiled_size = (layout.address(0, 0, aperture.slice) & !0x7FF) + info.surf_size as u32;
    let key = (image, level, aperture.slice);
    if write_back {
        let linear = memory.slice(aperture.address, aperture.size)?;
        let index = cache.position(key);
        if index.is_some_and(|index| cache.levels[index].linear == linear) {
            return Ok(());
        }
        let linear = linear.to_vec();
        let tiled = memory.slice_mut(image, tiled_size)?;
        macro_rules! store {
            ($n:literal) => {
                layout.for_each_pixel(width, height, aperture.slice, |x, y, offset| {
                    copy_pixel::<$n>(&linear, linear_at(x, y), tiled, offset as usize);
                })
            };
        }
        match bytes {
            1 => store!(1),
            2 => store!(2),
            4 => store!(4),
            8 => store!(8),
            _ => store!(16),
        }
        if let Some(index) = index {
            cache.levels.remove(index);
        }
    } else {
        let tiled = memory.slice(image, tiled_size)?;
        let known = cache.position(key).filter(|index| {
            let level = &cache.levels[*index];
            level.tiled == tiled && level.linear.len() == aperture.size as usize
        });
        if let Some(index) = known {
            cache.touch(index);
        } else {
            let mut linear = vec![0; aperture.size as usize];
            macro_rules! load {
                ($n:literal) => {
                    layout.for_each_pixel(width, height, aperture.slice, |x, y, offset| {
                        copy_pixel::<$n>(tiled, offset as usize, &mut linear, linear_at(x, y));
                    })
                };
            }
            match bytes {
                1 => load!(1),
                2 => load!(2),
                4 => load!(4),
                8 => load!(8),
                _ => load!(16),
            }
            cache.remember(CachedLevel { key, tiled: tiled.to_vec(), linear });
        }
        if let Some(level) = cache.levels.last() {
            memory.write_bytes(aperture.address, &level.linear)?;
        }
    }
    Ok(())
}
#[hle_export(lib = "gx2", name = "GX2AllocateTilingApertureEx")]
fn gx2_allocate_tiling_aperture_ex(
    ctx: &mut HleContext<'_>,
    surface: u32,
    level: u32,
    slice: u32,
    _endian: u32,
    handle: GuestPtr<u32>,
    address: GuestPtr<u32>,
) -> Result<(), HleError> {
    let surface = Gx2Surface::read(ctx.memory, surface)?;
    let bits = addrlib::format_bits(surface.format);
    let bits = if addrlib::is_compressed(surface.format) { bits / 16 } else { bits };
    let aligned_height = ((surface.height >> level).max(1) + 3) & !3;
    let size = (u64::from(surface.info(0).pitch) * u64::from(aligned_height) * u64::from(bits))
        .div_ceil(8) as u32;
    let Some(start) = ctx.os.gx2.apertures.find_space(size) else {
        log::warn!("no tiling aperture space for {size:#x} bytes");
        handle.write(ctx.memory, 0)?;
        address.write(ctx.memory, 0)?;
        return Ok(());
    };
    ctx.os.gx2.apertures.last_handle += 1;
    let aperture = Aperture {
        address: start,
        size,
        handle: ctx.os.gx2.apertures.last_handle,
        surface,
        level,
        slice,
    };
    ctx.os.gx2.apertures.open.push(aperture);
    handle.write(ctx.memory, aperture.handle)?;
    address.write(ctx.memory, start)?;
    retile(ctx.memory, &mut ctx.os.gx2.apertures.cache, &aperture, false)?;
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2FreeTilingAperture")]
fn gx2_free_tiling_aperture(ctx: &mut HleContext<'_>, handle: u32) -> Result<(), HleError> {
    let apertures = &mut ctx.os.gx2.apertures;
    if let Some(index) = apertures.open.iter().position(|a| a.handle == handle) {
        let aperture = apertures.open.swap_remove(index);
        retile(ctx.memory, &mut apertures.cache, &aperture, true)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use wiiu_gpu::addrlib::{dim, tile_mode};

    use super::*;
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn an_unchanged_level_is_served_again_and_later_writes_still_land() {
        let mut env = TestEnv::new();
        let image = SCRATCH + 0x1_0000;
        let mut surface = Gx2Surface {
            dim: dim::D2,
            width: 64,
            height: 64,
            depth: 1,
            num_levels: 1,
            format: 0x1A,
            tile_mode: tile_mode::TM_1D_TILED_THIN1,
            ..Gx2Surface::default()
        };
        surface.calc_size_and_alignment();
        surface.image = image;
        surface.write(&mut env.memory, SCRATCH).unwrap();
        env.memory.write_u32(image, 0xAABB_CCDD).unwrap();
        let (handle, address) = (SCRATCH + 0x100, SCRATCH + 0x104);
        let open = |env: &mut TestEnv| {
            env.call("gx2", "GX2AllocateTilingApertureEx", &[SCRATCH, 0, 0, 0, handle, address]);
            (env.memory.read_u32(handle).unwrap(), env.memory.read_u32(address).unwrap())
        };

        // Unchanged twice: the first pixel reads back both times.
        for _ in 0..2 {
            let (id, linear) = open(&mut env);
            assert_eq!(env.memory.read_u32(linear).unwrap(), 0xAABB_CCDD);
            env.call("gx2", "GX2FreeTilingAperture", &[id]);
        }
        // The guest changes the surface directly: the copy must not be stale.
        env.memory.write_u32(image, 0x1122_3344).unwrap();
        let (id, linear) = open(&mut env);
        assert_eq!(env.memory.read_u32(linear).unwrap(), 0x1122_3344);
        // A write through the aperture reaches the surface and is seen on reopening.
        env.memory.write_u32(linear, 0x5566_7788).unwrap();
        env.call("gx2", "GX2FreeTilingAperture", &[id]);
        assert_eq!(env.memory.read_u32(image).unwrap(), 0x5566_7788);
        let (_, linear) = open(&mut env);
        assert_eq!(env.memory.read_u32(linear).unwrap(), 0x5566_7788);
    }

    #[test]
    fn surfaces_taking_turns_each_stay_correct() {
        let mut env = TestEnv::new();
        let (handle, address) = (SCRATCH + 0x100, SCRATCH + 0x104);
        let surfaces =
            [(SCRATCH, SCRATCH + 0x1_0000, 0x11), (SCRATCH + 0x200, SCRATCH + 0x2_0000, 0x22)];
        for (at, image, value) in surfaces {
            let mut surface = Gx2Surface {
                dim: dim::D2,
                width: 64,
                height: 64,
                depth: 1,
                num_levels: 1,
                format: 0x1A,
                tile_mode: tile_mode::TM_1D_TILED_THIN1,
                ..Gx2Surface::default()
            };
            surface.calc_size_and_alignment();
            surface.image = image;
            surface.write(&mut env.memory, at).unwrap();
            env.memory.write_u32(image, value).unwrap();
        }
        for (at, _, value) in surfaces.iter().chain(&surfaces).chain(&surfaces) {
            env.call("gx2", "GX2AllocateTilingApertureEx", &[*at, 0, 0, 0, handle, address]);
            let linear = env.memory.read_u32(address).unwrap();
            assert_eq!(env.memory.read_u32(linear).unwrap(), *value);
            let id = env.memory.read_u32(handle).unwrap();
            env.call("gx2", "GX2FreeTilingAperture", &[id]);
        }
        assert_eq!(env.os.gx2.apertures.cache.levels.len(), 2);
    }

    #[test]
    fn writes_through_the_aperture_reach_the_tiled_surface() {
        let mut env = TestEnv::new();
        let image = SCRATCH + 0x1_0000;
        let mut surface = Gx2Surface {
            dim: dim::D2,
            width: 64,
            height: 64,
            depth: 1,
            num_levels: 1,
            format: 0x1A,
            tile_mode: tile_mode::TM_1D_TILED_THIN1,
            ..Gx2Surface::default()
        };
        surface.calc_size_and_alignment();
        surface.image = image;
        surface.write(&mut env.memory, SCRATCH).unwrap();

        let (handle, address) = (SCRATCH + 0x100, SCRATCH + 0x104);
        env.call("gx2", "GX2AllocateTilingApertureEx", &[SCRATCH, 0, 0, 0, handle, address]);
        let linear = env.memory.read_u32(address).unwrap();
        assert!(TILING_APERTURE.contains(linear));
        // Pixel (9, 2): the second micro tile, third row.
        env.memory.write_u32(linear + (9 + 2 * surface.pitch) * 4, 0x1234_5678).unwrap();
        let handle = env.memory.read_u32(handle).unwrap();
        env.call("gx2", "GX2FreeTilingAperture", &[handle]);

        let layout = Layout {
            tile_mode: tile_mode::TM_1D_TILED_THIN1,
            bpp: 32,
            pitch: surface.pitch,
            height: surface.info(0).height,
            samples: 1,
            is_depth: false,
            pipe_swizzle: 0,
            bank_swizzle: 0,
        };
        assert_eq!(env.memory.read_u32(image + layout.address(9, 2, 0)).unwrap(), 0x1234_5678);
    }
}
