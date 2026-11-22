//! Frame-heap exports.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ MEMFrmHeap calls   │   │ coreinit/mem/frm_api.rs  │   │ allocations        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ frame heap API           │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Functions for frame heaps, which allocate quickly and are freed all at once. Terms such as
//! HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
use wiiu_hle_macros::hle_export;
use wiiu_memory::GuestPtr;

use super::frm_heap::FreeMode;
use crate::hle::{HleContext, HleError};

#[hle_export(lib = "coreinit", name = "MEMCreateFrmHeapEx")]
fn mem_create_frm_heap_ex(
    ctx: &mut HleContext<'_>,
    start: GuestPtr<u8>,
    size: u32,
    _flags: u16,
) -> Result<u32, HleError> {
    if start.is_null() {
        return Ok(0);
    }
    let Some(handle) = ctx.os.coreinit.heaps.create_frame(start.address(), size) else {
        return Ok(0);
    };
    super::write_header(
        ctx.memory,
        handle,
        super::magic::FRAME,
        super::FRM_HEAP_HEADER,
        (handle + size) & !3,
    )?;
    Ok(handle)
}

#[hle_export(lib = "coreinit", name = "MEMDestroyFrmHeap")]
fn mem_destroy_frm_heap(ctx: &mut HleContext<'_>, heap: u32) -> u32 {
    ctx.os.coreinit.heaps.destroy(heap).map_or(0, |_| heap)
}

#[hle_export(lib = "coreinit", name = "MEMAllocFromFrmHeapEx")]
fn mem_alloc_from_frm_heap_ex(
    ctx: &mut HleContext<'_>,
    heap: u32,
    size: u32,
    align: i32,
) -> GuestPtr<u8> {
    let address =
        ctx.os.coreinit.heaps.frame_mut(heap).and_then(|h| h.alloc(size, align)).unwrap_or(0);
    if address == 0 {
        log::warn!("frame heap {heap:#010x}: cannot allocate {size:#x} bytes (align {align})");
    }
    GuestPtr::new(address)
}

#[hle_export(lib = "coreinit", name = "MEMFreeToFrmHeap")]
fn mem_free_to_frm_heap(ctx: &mut HleContext<'_>, heap: u32, mode: u32) {
    if let Some(heap) = ctx.os.coreinit.heaps.frame_mut(heap) {
        heap.free(FreeMode(mode));
    }
}

#[hle_export(lib = "coreinit", name = "MEMGetAllocatableSizeForFrmHeapEx")]
fn mem_get_allocatable_size_for_frm_heap_ex(
    ctx: &mut HleContext<'_>,
    heap: u32,
    align: i32,
) -> u32 {
    ctx.os.coreinit.heaps.frame_mut(heap).map_or(0, |h| h.allocatable_size(align))
}

#[hle_export(lib = "coreinit", name = "MEMRecordStateForFrmHeap")]
fn mem_record_state_for_frm_heap(ctx: &mut HleContext<'_>, heap: u32, id: u32) -> bool {
    ctx.os.coreinit.heaps.frame_mut(heap).map(|h| h.record_state(id)).is_some()
}

#[hle_export(lib = "coreinit", name = "MEMFreeByStateToFrmHeap")]
fn mem_free_by_state_to_frm_heap(ctx: &mut HleContext<'_>, heap: u32, id: u32) -> bool {
    ctx.os.coreinit.heaps.frame_mut(heap).is_some_and(|h| h.free_by_state(id))
}

#[hle_export(lib = "coreinit", name = "MEMAdjustFrmHeap")]
fn mem_adjust_frm_heap(ctx: &mut HleContext<'_>, heap: u32) -> u32 {
    ctx.os.coreinit.heaps.frame_mut(heap).map_or(0, super::frm_heap::FrmHeap::adjust)
}

#[hle_export(lib = "coreinit", name = "MEMResizeForMBlockFrmHeap")]
fn mem_resize_for_mblock_frm_heap(
    ctx: &mut HleContext<'_>,
    heap: u32,
    block: GuestPtr<u8>,
    size: u32,
) -> u32 {
    ctx.os.coreinit.heaps.frame_mut(heap).map_or(0, |h| h.resize_block(block.address(), size))
}

#[cfg(test)]
mod tests {
    use crate::{
        coreinit::mem::FRM_HEAP_HEADER,
        hle::test_env::{SCRATCH, TestEnv},
    };

    #[test]
    fn frame_heap_lifecycle() {
        let mut env = TestEnv::new();
        let heap = env.call("coreinit", "MEMCreateFrmHeapEx", &[SCRATCH, 0x1000, 0]);
        assert_eq!(heap, SCRATCH);
        let a = env.call("coreinit", "MEMAllocFromFrmHeapEx", &[heap, 0x100, 0x20]);
        assert_eq!(a, (SCRATCH + FRM_HEAP_HEADER).next_multiple_of(0x20));
        assert_eq!(env.call("coreinit", "MEMRecordStateForFrmHeap", &[heap, 1]), 1);
        env.call("coreinit", "MEMAllocFromFrmHeapEx", &[heap, 0x200, 4]);
        assert_eq!(env.call("coreinit", "MEMFreeByStateToFrmHeap", &[heap, 1]), 1);
        let free = env.call("coreinit", "MEMGetAllocatableSizeForFrmHeapEx", &[heap, 4]);
        assert_eq!(free, SCRATCH + 0x1000 - (a + 0x100));
        env.call("coreinit", "MEMFreeToFrmHeap", &[heap, 3]);
        assert_eq!(
            env.call("coreinit", "MEMAllocFromFrmHeapEx", &[heap, 4, 4]),
            SCRATCH + FRM_HEAP_HEADER
        );
    }
}
