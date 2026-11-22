//! `MEM*` heaps and memory bounds.
//!
//! Heap headers are not kept in guest memory: a handle is the address passed
//! to `MEMCreate*HeapEx` and maps to host-side bookkeeping in [`HeapTable`].
//! The first bytes of the area are still reserved (as the real header would
//! be) so no allocation ever equals its heap's handle.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ game allocates     │   │ coreinit/mem/mod.rs      │   │ blocks of memory   │
//! │ memory             │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ memory heaps (MEM)       │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Games carve their memory into heaps and allocate from them. This folder implements the
//! console's heap types. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.
pub mod block_api;
pub mod bounds;
pub mod exp_api;
pub mod exp_heap;
pub mod frm_api;
pub mod frm_heap;
pub mod unit_api;

use std::collections::BTreeMap;

pub use exp_heap::ExpHeap;
pub use frm_heap::FrmHeap;

/// `sizeof(MEMExpHeap)`.
pub const EXP_HEAP_HEADER: u32 = 0x7C;
/// `sizeof(MEMFrmHeap)`.
pub const FRM_HEAP_HEADER: u32 = 0x4C;

/// `MEMBaseHeapType`: arenas with a base heap.
pub mod arena {
    pub const MEM1: u32 = 0;
    pub const MEM2: u32 = 1;
    pub const FOREGROUND: u32 = 8;
}

/// Number of `MEMGetBaseHeapHandle` slots (as in Cafe OS).
pub const BASE_HEAP_SLOTS: usize = 9;

/// `MEMHeapMagic` values.
pub mod magic {
    pub const EXPANDED: u32 = 0x4558_5048; // "EXPH"
    pub const FRAME: u32 = 0x4652_4D48; // "FRMH"
    pub const BLOCK: u32 = 0x424C_4B48; // "BLKH"
    pub const UNIT: u32 = 0x554E_5448; // "UNTH"
}

/// Offsets inside `MEMHeapBase`, which games read directly.
mod header {
    pub const MAGIC: u32 = 0x00;
    pub const HEAP_START: u32 = 0x18;
    pub const HEAP_END: u32 = 0x1C;
}

/// Writes the `MEMHeapBase` fields of a heap created at `handle`.
///
/// # Errors
/// Fails if the header is unmapped.
pub fn write_header(
    memory: &mut wiiu_memory::Memory,
    handle: u32,
    magic: u32,
    header_size: u32,
    end: u32,
) -> Result<(), wiiu_memory::MemoryError> {
    memory.fill(handle, header_size, 0)?;
    memory.write_u32(handle + header::MAGIC, magic)?;
    memory.write_u32(handle + header::HEAP_START, handle + header_size)?;
    memory.write_u32(handle + header::HEAP_END, end)
}

/// One heap.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Heap {
    Expanded(ExpHeap),
    Frame(FrmHeap),
}

impl Heap {
    /// First byte of the heap's area.
    const fn start(&self) -> u32 {
        match self {
            Self::Expanded(h) => h.start(),
            Self::Frame(h) => h.start(),
        }
    }

    const fn contains(&self, address: u32) -> bool {
        match self {
            Self::Expanded(h) => address >= h.start() && address < h.end(),
            Self::Frame(h) => h.contains(address),
        }
    }
}

/// All heaps of the process plus the base heap slots.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct HeapTable {
    heaps: BTreeMap<u32, Heap>,
    base: [u32; BASE_HEAP_SLOTS],
}

impl HeapTable {
    /// Creates an expanded heap over `[start, start + size)`; returns its handle.
    #[inline]
    pub fn create_expanded(&mut self, start: u32, size: u32) -> Option<u32> {
        let usable = size.checked_sub(EXP_HEAP_HEADER)?;
        self.heaps.insert(start, Heap::Expanded(ExpHeap::new(start + EXP_HEAP_HEADER, usable)));
        Some(start)
    }

    /// Creates a block heap at `handle` over `[start, end)`: its header lies outside the
    /// area, so all of it can be handed out. Blocks are kept as an expanded heap keeps them.
    pub fn create_block(&mut self, handle: u32, start: u32, end: u32) -> Option<u32> {
        let size = end.checked_sub(start).filter(|size| *size > 0)?;
        self.heaps.insert(handle, Heap::Expanded(ExpHeap::new(start, size)));
        Some(handle)
    }

    /// Creates a frame heap over `[start, start + size)`; returns its handle.
    #[inline]
    pub fn create_frame(&mut self, start: u32, size: u32) -> Option<u32> {
        let usable = size.checked_sub(FRM_HEAP_HEADER)?;
        self.heaps.insert(start, Heap::Frame(FrmHeap::new(start + FRM_HEAP_HEADER, usable)));
        Some(start)
    }

    /// Removes a heap; base heaps are kept.
    pub fn destroy(&mut self, handle: u32) -> Option<Heap> {
        if self.base.contains(&handle) {
            return None;
        }
        self.heaps.remove(&handle)
    }

    pub fn expanded_mut(&mut self, handle: u32) -> Option<&mut ExpHeap> {
        match self.heaps.get_mut(&handle)? {
            Heap::Expanded(heap) => Some(heap),
            Heap::Frame(_) => None,
        }
    }

    pub fn frame_mut(&mut self, handle: u32) -> Option<&mut FrmHeap> {
        match self.heaps.get_mut(&handle)? {
            Heap::Frame(heap) => Some(heap),
            Heap::Expanded(_) => None,
        }
    }

    /// The MEM2 base heap, target of `MEMAllocFromDefaultHeap`.
    #[inline]
    pub fn default_heap_mut(&mut self) -> Option<&mut ExpHeap> {
        let handle = self.base(arena::MEM2);
        self.expanded_mut(handle)
    }

    #[inline]
    pub fn base(&self, arena: u32) -> u32 {
        self.base.get(arena as usize).copied().unwrap_or(0)
    }

    /// Sets a base heap slot unconditionally (used after `__preinit_user`).
    #[inline]
    pub fn replace_base(&mut self, arena: u32, handle: u32) {
        if let Some(slot) = self.base.get_mut(arena as usize) {
            *slot = handle;
        }
    }

    /// Fills an empty base heap slot. Like Cafe OS, an occupied slot is left
    /// alone and its handle returned; success returns 0.
    pub fn set_base(&mut self, arena: u32, handle: u32) -> u32 {
        match self.base.get_mut(arena as usize) {
            Some(slot) if *slot == 0 => {
                *slot = handle;
                0
            }
            Some(slot) => *slot,
            None => 0,
        }
    }

    /// Handle of the innermost heap whose area contains `address` (`MEMFindContainHeap`
    /// searches the heaps made inside a heap first). A heap made inside another starts
    /// after it, so the containing heap starting last is the innermost: Wii Sports Club
    /// finds the parent of its heaps this way, and frees nothing back to an outer one.
    #[inline]
    pub fn find_containing(&self, address: u32) -> Option<u32> {
        self.heaps
            .iter()
            .filter(|(_, heap)| heap.contains(address))
            .max_by_key(|(_, heap)| heap.start())
            .map(|(handle, _)| *handle)
    }

    /// `(start, size)` of the MEM2 base heap area including its header.
    pub fn mem2_bounds(&self) -> (u32, u32) {
        let handle = self.base(arena::MEM2);
        match self.heaps.get(&handle) {
            Some(Heap::Expanded(heap)) => (handle, heap.end() - handle),
            _ => (0, 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_reserve_their_header() {
        let mut table = HeapTable::default();
        let exp = table.create_expanded(0x1000, 0x1000).unwrap();
        table.set_base(arena::MEM2, exp);
        let block = table.default_heap_mut().unwrap().alloc(0x10, 4).unwrap();
        assert_eq!(block, 0x1000 + EXP_HEAP_HEADER);
        assert_eq!(table.find_containing(block), Some(exp));
        assert!(table.frame_mut(exp).is_none());
        assert!(table.destroy(exp).is_none(), "base heaps survive destroy");
        assert_eq!(table.mem2_bounds(), (0x1000, 0x1000));
    }

    #[test]
    fn finds_the_innermost_heap() {
        let mut table = HeapTable::default();
        let outer = table.create_expanded(0x1000, 0x4000).unwrap();
        let block = table.expanded_mut(outer).unwrap().alloc(0x2000, 4).unwrap();
        let inner = table.create_expanded(block, 0x2000).unwrap();
        let inner_block = table.expanded_mut(inner).unwrap().alloc(0x10, 4).unwrap();
        assert_eq!(table.find_containing(inner_block), Some(inner));
        assert_eq!(table.find_containing(block + 0x3000), Some(outer));
    }
}
