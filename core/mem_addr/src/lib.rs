//! Wii U memory map.
//!
//! Cafe OS applications only ever see *effective* (virtual) addresses. The
//! layout below follows what the loader and kernel set up for a game process:
//!
//! ```text
//!  0x0100_0000 ┌───────────────────────┐
//!              │ TEXT   (RPX/RPL code) │
//!  0x0E00_0000 ├───────────────────────┤
//!              │ HLE    (stubs, OS data│  emulator-owned, not on hardware
//!  0x1000_0000 ├───────────────────────┤
//!              │ MEM2   (data, heaps)  │  physical 0x5000_0000
//!  0x5000_0000 └───────────────────────┘
//!  0xE000_0000 ┌───────────────────────┐
//!              │ FOREGROUND (64 MiB)   │  foreground bucket
//!  0xE400_0000 └───────────────────────┘
//!  0xF400_0000 ┌───────────────────────┐
//!              │ MEM1   (32 MiB)       │  physical 0x0000_0000
//!  0xF600_0000 ├───────────────────────┤
//!              │ LOADER (RPL metadata) │
//!  0xF800_0000 ├───────────────────────┤
//!              │ SHARED (system fonts) │
//!  0xFA00_0000 └───────────────────────┘
//! ```
//!
//! Latte and the other devices work on physical addresses; see
//! [`effective_to_physical`].

/// A contiguous address range `[start, start + size)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub name: &'static str,
    pub start: u32,
    pub size: u32,
}

impl Region {
    #[inline]
    pub const fn new(name: &'static str, start: u32, size: u32) -> Self {
        Self { name, start, size }
    }

    /// One past the last address (as `u64` so the top of memory does not overflow).
    #[inline]
    pub const fn end(&self) -> u64 {
        self.start as u64 + self.size as u64
    }

    #[inline]
    pub const fn contains(&self, address: u32) -> bool {
        address >= self.start && (address as u64) < self.end()
    }

    /// `true` if `[address, address + len)` lies entirely inside the region.
    #[inline]
    pub const fn contains_range(&self, address: u32, len: u32) -> bool {
        address >= self.start && address as u64 + len as u64 <= self.end()
    }
}

/// Executable code of the main RPX and every loaded RPL.
pub const TEXT: Region = Region::new("TEXT", 0x0100_0000, 0x0D00_0000);

/// Code caves of graphic pack patches (Cemu's `MEMORY_CODECAVEAREA`), inside [`TEXT`].
pub const CODE_CAVE: Region = Region::new("CODE_CAVE", 0x0180_0000, 0x0040_0000);

/// Where modules are loaded, inside [`TEXT`]: from 0x02000000 as on the console and in
/// Cemu. Graphic pack patches write to and branch to these addresses directly.
pub const CODE: Region = Region::new("CODE", 0x0200_0000, 0x0C00_0000);

/// Emulator-owned area: HLE call stubs and data exported by HLE libraries.
pub const HLE: Region = Region::new("HLE", 0x0E00_0000, 0x0200_0000);

/// MEM2 as seen by the application: `.data`, `.bss`, heaps and stacks.
pub const MEM2: Region = Region::new("MEM2", 0x1000_0000, 0x4000_0000);

/// Foreground bucket: memory owned by the foreground application (64 MiB).
pub const FOREGROUND: Region = Region::new("FOREGROUND", 0xE000_0000, 0x0400_0000);

/// Part of [`FOREGROUND`] free for the application (the rest holds OS save/copy areas).
pub const FOREGROUND_FREE_SIZE: u32 = 0x0280_0000;

/// MEM1: 32 MiB of fast embedded RAM, mostly used for GPU surfaces.
pub const MEM1: Region = Region::new("MEM1", 0xF400_0000, 0x0200_0000);

/// Loader-owned memory (RPL metadata, read-only `.rodata` of system modules).
pub const LOADER: Region = Region::new("LOADER", 0xF600_0000, 0x0200_0000);

/// Shared read-only data such as the system fonts.
pub const SHARED: Region = Region::new("SHARED", 0xF800_0000, 0x0200_0000);

/// Per-core locked cache windows (see `coreinit::lc`).
pub const LOCKED_CACHE: Region = Region::new("LOCKED_CACHE", 0xFFC0_0000, 0x000C_4000);

/// Linear windows onto tiled surfaces (see `gx2::aperture`).
pub const TILING_APERTURE: Region = Region::new("TILING_APERTURE", 0xE800_0000, 0x0200_0000);

/// Overlay arena: mapped only once the title calls `OSEnableOverlayArena`
/// (as on hardware, where it is memory borrowed from the system).
pub const OVERLAY_ARENA: Region = Region::new("OVERLAY_ARENA", 0xA000_0000, 0x1C00_0000);

/// Every region mapped for a game process.
pub const GAME_REGIONS: [Region; 9] =
    [TEXT, HLE, MEM2, FOREGROUND, TILING_APERTURE, MEM1, LOADER, SHARED, LOCKED_CACHE];

/// Physical base of MEM2 (the effective base is `MEM2.start`).
pub const MEM2_PHYSICAL_BASE: u32 = 0x5000_0000;

/// Physical base of MEM1.
pub const MEM1_PHYSICAL_BASE: u32 = 0x0000_0000;

/// Translates an effective address to the physical address Latte uses.
///
/// Returns `None` for addresses outside MEM1/MEM2 (code, HLE area, ...).
pub const fn effective_to_physical(address: u32) -> Option<u32> {
    if MEM2.contains(address) {
        Some(address - MEM2.start + MEM2_PHYSICAL_BASE)
    } else if MEM1.contains(address) {
        Some(address - MEM1.start + MEM1_PHYSICAL_BASE)
    } else if FOREGROUND.contains(address) || OVERLAY_ARENA.contains(address) {
        // Scan buffers live here; the bucket is not remapped. The overlay
        // arena is not remapped either: its range collides with no other.
        Some(address)
    } else {
        None
    }
}

/// Inverse of [`effective_to_physical`].
pub const fn physical_to_effective(address: u32) -> Option<u32> {
    if address >= MEM2_PHYSICAL_BASE && address - MEM2_PHYSICAL_BASE < MEM2.size {
        Some(address - MEM2_PHYSICAL_BASE + MEM2.start)
    } else if address < MEM1.size {
        Some(address + MEM1.start)
    } else if FOREGROUND.contains(address) || OVERLAY_ARENA.contains(address) {
        Some(address)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_do_not_overlap() {
        let all: Vec<Region> = GAME_REGIONS.into_iter().chain([OVERLAY_ARENA]).collect();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                let disjoint = a.end() <= b.start as u64 || b.end() <= a.start as u64;
                assert!(disjoint, "{} overlaps {}", a.name, b.name);
            }
        }
    }

    #[test]
    fn physical_round_trip() {
        for address in [MEM2.start, MEM2.start + 0x1234, MEM1.start, MEM1.start + 0x10] {
            let physical = effective_to_physical(address).unwrap();
            assert_eq!(physical_to_effective(physical), Some(address));
        }
        assert_eq!(effective_to_physical(TEXT.start), None);
    }
}
