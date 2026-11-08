//! Unit heaps (fixed-size blocks) and `MEMAllocator` helpers.
use std::collections::HashMap;

use wiiu_hle_macros::hle_export;

use crate::hle::{HleContext, HleError};

/// Room for the heap header at the start of the managed range.
const UNIT_HEAP_HEADER: u32 = 0x40;

/// One unit heap; the free list lives on the host.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct UnitHeap {
    free: Vec<u32>,
    capacity: u32,
}

/// Every unit heap, by handle.
pub type UnitHeaps = HashMap<u32, UnitHeap>;

#[hle_export(lib = "coreinit", name = "MEMCreateUnitHeapEx")]
fn mem_create_unit_heap_ex(
    ctx: &mut HleContext<'_>,
    start: u32,
    size: u32,
    block_size: u32,
    align: u32,
    _flags: u32,
) -> Result<u32, HleError> {
    let align = align.max(4);
    let first = (start + UNIT_HEAP_HEADER).next_multiple_of(align);
    let stride = block_size.next_multiple_of(align);
    if stride == 0 || first >= start.saturating_add(size) {
        return Ok(0);
    }
    let capacity = (start + size - first) / stride;
    // Allocation order matches the guest's list: lowest address first.
    let free = (0..capacity).rev().map(|i| first + i * stride).collect();
    ctx.os.coreinit.unit_heaps.insert(start, UnitHeap { free, capacity });
    Ok(start)
}

#[hle_export(lib = "coreinit", name = "MEMDestroyUnitHeap")]
fn mem_destroy_unit_heap(ctx: &mut HleContext<'_>, heap: u32) -> u32 {
    ctx.os.coreinit.unit_heaps.remove(&heap);
    heap
}

#[hle_export(lib = "coreinit", name = "MEMAllocFromUnitHeap")]
fn mem_alloc_from_unit_heap(ctx: &mut HleContext<'_>, heap: u32) -> u32 {
    ctx.os.coreinit.unit_heaps.get_mut(&heap).and_then(|h| h.free.pop()).unwrap_or(0)
}

#[hle_export(lib = "coreinit", name = "MEMFreeToUnitHeap")]
fn mem_free_to_unit_heap(ctx: &mut HleContext<'_>, heap: u32, block: u32) {
    if block != 0 {
        if let Some(heap) = ctx.os.coreinit.unit_heaps.get_mut(&heap) {
            heap.free.push(block);
        }
    }
}

#[hle_export(lib = "coreinit", name = "MEMCountFreeBlockForUnitHeap")]
#[allow(clippy::cast_possible_truncation)]
fn mem_count_free_block_for_unit_heap(ctx: &mut HleContext<'_>, heap: u32) -> u32 {
    ctx.os.coreinit.unit_heaps.get(&heap).map_or(0, |h| {
        debug_assert!(h.free.len() as u32 <= h.capacity);
        h.free.len() as u32
    })
}

#[hle_export(lib = "coreinit", name = "MEMCalcHeapSizeForUnitHeap")]
fn mem_calc_heap_size_for_unit_heap(
    _ctx: &mut HleContext<'_>,
    block_size: u32,
    count: u32,
    align: u32,
) -> u32 {
    let align = align.max(4);
    UNIT_HEAP_HEADER.next_multiple_of(align) + block_size.next_multiple_of(align) * count
}

/// `MEMAllocator::funcs`: `(*alloc)(allocator, size)`.
#[hle_export(lib = "coreinit", name = "__shocora_AllocatorDefaultAlloc")]
fn allocator_default_alloc(ctx: &mut HleContext<'_>, _allocator: u32, size: u32) -> u32 {
    ctx.os.coreinit.heaps.default_heap_mut().and_then(|heap| heap.alloc(size, 0x40)).unwrap_or(0)
}

/// `MEMAllocator::funcs`: `(*free)(allocator, block)`.
#[hle_export(lib = "coreinit", name = "__shocora_AllocatorDefaultFree")]
fn allocator_default_free(ctx: &mut HleContext<'_>, _allocator: u32, block: u32) {
    if let Some(heap) = ctx.os.coreinit.heaps.default_heap_mut() {
        heap.free(block);
    }
}

#[hle_export(lib = "coreinit", name = "MEMInitAllocatorForDefaultHeap")]
fn mem_init_allocator_for_default_heap(
    ctx: &mut HleContext<'_>,
    allocator: u32,
) -> Result<(), HleError> {
    let functions = match ctx.os.coreinit.default_allocator_functions {
        0 => {
            let table = ctx.os.registry.allocate_data(8, 4);
            let alloc = ctx.os.registry.function_stub("coreinit", ALLOCATOR_DEFAULT_ALLOC.name);
            let free = ctx.os.registry.function_stub("coreinit", ALLOCATOR_DEFAULT_FREE.name);
            ctx.os.registry.commit(ctx.memory)?;
            ctx.memory.write_u32(table, alloc)?;
            ctx.memory.write_u32(table + 4, free)?;
            ctx.os.coreinit.default_allocator_functions = table;
            table
        }
        table => table,
    };
    ctx.memory.write_u32(allocator, functions)?;
    ctx.memory.write_u32(allocator + 4, 0)?;
    ctx.memory.write_u32(allocator + 8, 0)?;
    ctx.memory.write_u32(allocator + 12, 0)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn unit_heap_hands_out_and_recycles_blocks() {
        let mut env = TestEnv::new();
        let heap = env.call("coreinit", "MEMCreateUnitHeapEx", &[SCRATCH, 0x140, 0x40, 0x40, 0]);
        assert_eq!(heap, SCRATCH);
        assert_eq!(env.call("coreinit", "MEMCountFreeBlockForUnitHeap", &[heap]), 4);
        let first = env.call("coreinit", "MEMAllocFromUnitHeap", &[heap]);
        assert_eq!(first, SCRATCH + 0x40);
        let second = env.call("coreinit", "MEMAllocFromUnitHeap", &[heap]);
        assert_eq!(second, SCRATCH + 0x80);
        env.call("coreinit", "MEMFreeToUnitHeap", &[heap, first]);
        assert_eq!(env.call("coreinit", "MEMAllocFromUnitHeap", &[heap]), first);
    }
}
