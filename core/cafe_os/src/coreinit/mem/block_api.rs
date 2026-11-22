//! Block-heap exports (`MEMBlockHeap`), as Cemu has them.
//!
//! The heap's header (`MEMBlockHeap`, 0x80 bytes) is guest memory of its own, apart from
//! the area it hands out. The blocks are kept host-side like an expanded heap's; the
//! tracking memory the title passes is not needed for that and is left alone, so the
//! heap never runs out of tracking entries.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ MEMBlockHeap calls │   │ coreinit/mem/block_api.r │   │ allocated ranges   │
//! │                    │──►│ s                        │──►│                    │
//! │                    │   │ ──────────────────────── │   │                    │
//! │                    │   │ block heap               │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! A heap that hands out ranges of memory tracked in a list of blocks. Terms such as HLE, JIT
//! or shader are explained in `docs/guide/wiiu_term.md`.
use wiiu_hle_macros::hle_export;
use wiiu_memory::GuestPtr;

use crate::hle::{HleContext, HleError};

/// `sizeof(MEMBlockHeap)`.
const BLOCK_HEAP_HEADER: u32 = 0x80;

/// Tracking entries reported left: blocks here need none.
const TRACKING_LEFT: u32 = 0x1000;

#[hle_export(lib = "coreinit", name = "MEMInitBlockHeap")]
fn mem_init_block_heap(
    ctx: &mut HleContext<'_>,
    heap: GuestPtr<u8>,
    start: GuestPtr<u8>,
    end: GuestPtr<u8>,
    _track_memory: GuestPtr<u8>,
    _track_size: u32,
    _flags: u32,
) -> Result<u32, HleError> {
    if heap.is_null() || start.is_null() || end.is_null() || start.address() >= end.address() {
        return Ok(0);
    }
    let handle = heap.address();
    let Some(handle) = ctx.os.coreinit.heaps.create_block(handle, start.address(), end.address())
    else {
        return Ok(0);
    };
    ctx.memory.fill(handle, BLOCK_HEAP_HEADER, 0)?;
    ctx.memory.write_u32(handle, super::magic::BLOCK)?;
    ctx.memory.write_u32(handle + 0x18, start.address())?;
    ctx.memory.write_u32(handle + 0x1C, end.address())?;
    Ok(handle)
}

#[hle_export(lib = "coreinit", name = "MEMDestroyBlockHeap")]
fn mem_destroy_block_heap(ctx: &mut HleContext<'_>, heap: u32) -> Result<u32, HleError> {
    if heap == 0 || ctx.os.coreinit.heaps.destroy(heap).is_none() {
        return Ok(0);
    }
    ctx.memory.fill(heap, BLOCK_HEAP_HEADER, 0)?;
    Ok(heap)
}

#[hle_export(lib = "coreinit", name = "MEMAddBlockHeapTracking")]
fn mem_add_block_heap_tracking(
    ctx: &mut HleContext<'_>,
    heap: u32,
    _track_memory: GuestPtr<u8>,
    _track_size: u32,
) -> i32 {
    if ctx.os.coreinit.heaps.expanded_mut(heap).is_some() { 0 } else { -4 }
}

#[hle_export(lib = "coreinit", name = "MEMGetTrackingLeftInBlockHeap")]
fn mem_get_tracking_left_in_block_heap(_ctx: &mut HleContext<'_>, _heap: u32) -> u32 {
    TRACKING_LEFT
}

#[hle_export(lib = "coreinit", name = "MEMAllocFromBlockHeapEx")]
fn mem_alloc_from_block_heap_ex(
    ctx: &mut HleContext<'_>,
    heap: u32,
    size: u32,
    align: i32,
) -> GuestPtr<u8> {
    if size == 0 {
        return GuestPtr::new(0);
    }
    let address = ctx
        .os
        .coreinit
        .heaps
        .expanded_mut(heap)
        .and_then(|heap| heap.alloc(size, align))
        .unwrap_or(0);
    if address == 0 {
        log::warn!("block heap {heap:#x}: cannot allocate {size:#x} bytes (align {align})");
    }
    GuestPtr::new(address)
}

#[hle_export(lib = "coreinit", name = "MEMFreeToBlockHeap")]
fn mem_free_to_block_heap(ctx: &mut HleContext<'_>, heap: u32, block: GuestPtr<u8>) {
    if block.is_null() {
        return;
    }
    let freed =
        ctx.os.coreinit.heaps.expanded_mut(heap).is_some_and(|heap| heap.free(block.address()));
    if !freed {
        log::warn!("block heap {heap:#x}: freeing unknown block {:#010x}", block.address());
    }
}

#[hle_export(lib = "coreinit", name = "MEMGetAllocatableSizeForBlockHeapEx")]
fn mem_get_allocatable_size_for_block_heap_ex(
    ctx: &mut HleContext<'_>,
    heap: u32,
    align: i32,
) -> u32 {
    ctx.os.coreinit.heaps.expanded_mut(heap).map_or(0, |heap| heap.allocatable_size(align))
}

#[hle_export(lib = "coreinit", name = "MEMGetTotalFreeSizeForBlockHeap")]
fn mem_get_total_free_size_for_block_heap(ctx: &mut HleContext<'_>, heap: u32) -> u32 {
    ctx.os.coreinit.heaps.expanded_mut(heap).map_or(0, |heap| heap.total_free())
}

#[cfg(test)]
mod tests {
    use crate::hle::test_env::{SCRATCH, TestEnv};

    /// Intent: a block heap hands out its whole area and takes blocks back.
    /// Pass: the first block starts at the area's start; after freeing it, the whole area
    /// is allocatable again; the header carries the block-heap magic.
    #[test]
    fn block_heap_allocates_and_frees() {
        let mut env = TestEnv::new();
        let header = SCRATCH;
        let (start, end) = (SCRATCH + 0x1000, SCRATCH + 0x2000);
        let heap = env.call("coreinit", "MEMInitBlockHeap", &[header, start, end, 0, 0, 0]);
        assert_eq!(heap, header);
        assert_eq!(env.memory.read_u32(header).unwrap(), super::super::magic::BLOCK);
        let block = env.call("coreinit", "MEMAllocFromBlockHeapEx", &[heap, 0x100, 4]);
        assert_eq!(block, start);
        assert_eq!(env.call("coreinit", "MEMGetAllocatableSizeForBlockHeapEx", &[heap, 4]), 0xF00);
        env.call("coreinit", "MEMFreeToBlockHeap", &[heap, block]);
        assert_eq!(env.call("coreinit", "MEMGetTotalFreeSizeForBlockHeap", &[heap]), 0x1000);
    }
}
