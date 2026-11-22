//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!               ▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ segment sizes      │   │ allocator.rs             │   │ addresses          │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ load-address allocator   │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Chooses where in memory each loaded part goes. Terms such as HLE, JIT or shader are
//! explained in `docs/guide/wiiu_term.md`.

use wiiu_mem_addr::Region;

use crate::{LoadError, SegmentAllocator, SegmentKind};

/// Hands out segments from three regions with bump pointers.
///
/// Good enough for the loader's own needs: modules are never unloaded during
/// a run.
#[derive(Debug, Clone)]
pub struct LinearAllocator {
    regions: [Region; 3],
    cursors: [u32; 3],
}

impl LinearAllocator {
    #[inline]
    pub const fn new(text: Region, data: Region, load: Region) -> Self {
        Self { regions: [text, data, load], cursors: [text.start, data.start, load.start] }
    }

    const fn slot(kind: SegmentKind) -> usize {
        match kind {
            SegmentKind::Text => 0,
            SegmentKind::Data => 1,
            SegmentKind::Load => 2,
        }
    }

    /// First free address of `kind`'s region.
    #[inline]
    pub const fn cursor(&self, kind: SegmentKind) -> u32 {
        self.cursors[Self::slot(kind)]
    }
}

impl SegmentAllocator for LinearAllocator {
    fn allocate(&mut self, kind: SegmentKind, size: u32, align: u32) -> Result<u32, LoadError> {
        let slot = Self::slot(kind);
        let align = align.max(4).next_power_of_two();
        let start = self.cursors[slot].checked_next_multiple_of(align);
        let end = start.and_then(|s| s.checked_add(size));
        match (start, end) {
            (Some(start), Some(end)) if u64::from(end) <= self.regions[slot].end() => {
                self.cursors[slot] = end;
                Ok(start)
            }
            _ => Err(LoadError::OutOfMemory { kind, size }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_and_bounds() {
        let r = |s| Region::new("r", s, 0x100);
        let mut alloc = LinearAllocator::new(r(0x1000), r(0x2000), r(0x3000));
        assert_eq!(alloc.allocate(SegmentKind::Data, 3, 4).unwrap(), 0x2000);
        assert_eq!(alloc.allocate(SegmentKind::Data, 4, 0x40).unwrap(), 0x2040);
        assert!(alloc.allocate(SegmentKind::Data, 0x100, 4).is_err());
        assert_eq!(alloc.allocate(SegmentKind::Text, 0x100, 4).unwrap(), 0x1000);
    }
}
