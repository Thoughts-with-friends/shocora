//! Expanded-heap and base-heap exports.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ MEMExpHeap calls   │   │ coreinit/mem/exp_api.rs  │   │ allocations        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ expanded heap API        │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The general-purpose heap functions most games allocate from. Terms such as HLE, JIT or
//! shader are explained in `docs/guide/wiiu_term.md`.
use wiiu_hle_macros::hle_export;
use wiiu_memory::GuestPtr;

use crate::hle::{HleContext, HleError};

fn alloc(ctx: &mut HleContext<'_>, handle: Option<u32>, size: u32, align: i32) -> GuestPtr<u8> {
    let heaps = &mut ctx.os.coreinit.heaps;
    let heap = match handle {
        Some(handle) => heaps.expanded_mut(handle),
        None => heaps.default_heap_mut(),
    };
    let address = heap.and_then(|heap| heap.alloc(size, align)).unwrap_or(0);
    if address == 0 {
        log::warn!("expanded heap {handle:x?}: cannot allocate {size:#x} bytes (align {align})");
    }
    GuestPtr::new(address)
}

fn free(ctx: &mut HleContext<'_>, handle: Option<u32>, ptr: GuestPtr<u8>) {
    if ptr.is_null() {
        return;
    }
    let heaps = &mut ctx.os.coreinit.heaps;
    let heap = match handle {
        Some(handle) => heaps.expanded_mut(handle),
        None => heaps.default_heap_mut(),
    };
    if !heap.is_some_and(|heap| heap.free(ptr.address())) {
        log::warn!("expanded heap {handle:x?}: freeing unknown block {:#010x}", ptr.address());
    }
}

/// Target of the `MEMAllocFromDefaultHeapEx` function pointer.
#[hle_export(lib = "coreinit", name = "__shocora_DefaultHeapAllocEx")]
fn default_alloc_ex(ctx: &mut HleContext<'_>, size: u32, align: i32) -> GuestPtr<u8> {
    alloc(ctx, None, size, align)
}

/// Target of the `MEMAllocFromDefaultHeap` function pointer.
#[hle_export(lib = "coreinit", name = "__shocora_DefaultHeapAlloc")]
fn default_alloc(ctx: &mut HleContext<'_>, size: u32) -> GuestPtr<u8> {
    alloc(ctx, None, size, 0x40)
}

/// Target of the `MEMFreeToDefaultHeap` function pointer.
#[hle_export(lib = "coreinit", name = "__shocora_DefaultHeapFree")]
fn default_free(ctx: &mut HleContext<'_>, ptr: GuestPtr<u8>) {
    free(ctx, None, ptr);
}

#[hle_export(lib = "coreinit", name = "MEMGetBaseHeapHandle")]
fn mem_get_base_heap_handle(ctx: &mut HleContext<'_>, arena: u32) -> u32 {
    ctx.os.coreinit.heaps.base(arena)
}

#[hle_export(lib = "coreinit", name = "MEMSetBaseHeapHandle")]
fn mem_set_base_heap_handle(ctx: &mut HleContext<'_>, arena: u32, heap: u32) -> u32 {
    ctx.os.coreinit.heaps.set_base(arena, heap)
}

#[hle_export(lib = "coreinit", name = "MEMFindContainHeap")]
fn mem_find_contain_heap(ctx: &mut HleContext<'_>, block: GuestPtr<u8>) -> u32 {
    ctx.os.coreinit.heaps.find_containing(block.address()).unwrap_or(0)
}

#[hle_export(lib = "coreinit", name = "MEMCreateExpHeapEx")]
fn mem_create_exp_heap_ex(
    ctx: &mut HleContext<'_>,
    start: GuestPtr<u8>,
    size: u32,
    _flags: u16,
) -> Result<u32, HleError> {
    if start.is_null() {
        return Ok(0);
    }
    let Some(handle) = ctx.os.coreinit.heaps.create_expanded(start.address(), size) else {
        return Ok(0);
    };
    super::write_header(
        ctx.memory,
        handle,
        super::magic::EXPANDED,
        super::EXP_HEAP_HEADER,
        (handle + size) & !3,
    )?;
    Ok(handle)
}

#[hle_export(lib = "coreinit", name = "MEMDestroyExpHeap")]
fn mem_destroy_exp_heap(ctx: &mut HleContext<'_>, heap: u32) -> u32 {
    ctx.os.coreinit.heaps.destroy(heap).map_or(0, |_| heap)
}

#[hle_export(lib = "coreinit", name = "MEMAllocFromExpHeapEx")]
fn mem_alloc_from_exp_heap_ex(
    ctx: &mut HleContext<'_>,
    heap: u32,
    size: u32,
    align: i32,
) -> GuestPtr<u8> {
    alloc(ctx, Some(heap), size, align)
}

#[hle_export(lib = "coreinit", name = "MEMFreeToExpHeap")]
fn mem_free_to_exp_heap(ctx: &mut HleContext<'_>, heap: u32, ptr: GuestPtr<u8>) {
    free(ctx, Some(heap), ptr);
}

#[hle_export(lib = "coreinit", name = "MEMGetAllocatableSizeForExpHeapEx")]
fn mem_get_allocatable_size_for_exp_heap_ex(
    ctx: &mut HleContext<'_>,
    heap: u32,
    align: i32,
) -> u32 {
    ctx.os.coreinit.heaps.expanded_mut(heap).map_or(0, |h| h.allocatable_size(align))
}

#[hle_export(lib = "coreinit", name = "MEMGetTotalFreeSizeForExpHeap")]
fn mem_get_total_free_size_for_exp_heap(ctx: &mut HleContext<'_>, heap: u32) -> u32 {
    ctx.os.coreinit.heaps.expanded_mut(heap).map_or(0, |h| h.total_free())
}

#[hle_export(lib = "coreinit", name = "MEMGetSizeForMBlockExpHeap")]
fn mem_get_size_for_mblock_exp_heap(ctx: &mut HleContext<'_>, block: GuestPtr<u8>) -> u32 {
    let heaps = &mut ctx.os.coreinit.heaps;
    heaps
        .find_containing(block.address())
        .and_then(|handle| heaps.expanded_mut(handle))
        .and_then(|heap| heap.allocation_size(block.address()))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use crate::{
        coreinit::mem::EXP_HEAP_HEADER,
        hle::test_env::{HEAP_START, SCRATCH, TestEnv},
    };

    #[test]
    fn default_heap_and_created_heaps() {
        let mut env = TestEnv::new();
        let base = env.call("coreinit", "MEMGetBaseHeapHandle", &[1]);
        assert_eq!(base, HEAP_START);
        let a = env.call("coreinit", "__shocora_DefaultHeapAllocEx", &[0x100, 0x40]);
        assert_eq!(a % 0x40, 0);
        assert_eq!(env.call("coreinit", "MEMGetSizeForMBlockExpHeap", &[a]), 0x100);
        let b =
            env.call("coreinit", "MEMAllocFromExpHeapEx", &[base, 0x10, (-4_i32).cast_unsigned()]);
        assert!(b > a);
        env.call("coreinit", "__shocora_DefaultHeapFree", &[a]);

        let heap = env.call("coreinit", "MEMCreateExpHeapEx", &[SCRATCH, 0x1000, 0]);
        let c = env.call("coreinit", "MEMAllocFromExpHeapEx", &[heap, 0x20, 4]);
        assert_eq!(c, SCRATCH + EXP_HEAP_HEADER);
        assert_eq!(
            env.call("coreinit", "MEMGetAllocatableSizeForExpHeapEx", &[heap, 4]),
            0x1000 - EXP_HEAP_HEADER - 0x20
        );
        assert_eq!(env.call("coreinit", "MEMFindContainHeap", &[c]), heap);
    }
}
