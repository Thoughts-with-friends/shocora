//! Unit heaps (fixed-size blocks) and `MEMAllocator` helpers.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ MEMUnitHeap calls  │   │ coreinit/mem/unit_api.rs │   │ equal-size blocks  │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ unit heap (fixed size)   │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! A heap of equal-sized blocks, plus the allocator helpers games pass around. Terms such as
//! HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
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
    // Titles read the header themselves: Paper Mario: Color Splash frees a pooled object
    // only when the block lies between the heap and its end, so a blank header leaked it
    // and left a stale handle that crashed after the title screen.
    super::write_header(ctx.memory, start, super::magic::UNIT, UNIT_HEAP_HEADER, start + size)?;
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

/// `MEMAllocator::funcs` of an expanded heap: `heap` at +4, alignment at +8.
#[hle_export(lib = "coreinit", name = "__shocora_AllocatorExpAlloc")]
fn allocator_exp_alloc(
    ctx: &mut HleContext<'_>,
    allocator: u32,
    size: u32,
) -> Result<u32, HleError> {
    let heap = ctx.memory.read_u32(allocator + 4)?;
    #[allow(clippy::cast_possible_wrap)]
    let align = ctx.memory.read_u32(allocator + 8)? as i32;
    let heap = ctx.os.coreinit.heaps.expanded_mut(heap);
    Ok(heap.and_then(|heap| heap.alloc(size, align)).unwrap_or(0))
}

#[hle_export(lib = "coreinit", name = "__shocora_AllocatorExpFree")]
fn allocator_exp_free(
    ctx: &mut HleContext<'_>,
    allocator: u32,
    block: u32,
) -> Result<(), HleError> {
    let heap = ctx.memory.read_u32(allocator + 4)?;
    if block != 0
        && let Some(heap) = ctx.os.coreinit.heaps.expanded_mut(heap)
    {
        heap.free(block);
    }
    Ok(())
}

/// The `(alloc, free)` table in `slot`, written to guest memory on first use.
fn allocator_functions(
    ctx: &mut HleContext<'_>,
    slot: fn(&mut crate::coreinit::CoreinitState) -> &mut u32,
    alloc: &str,
    free: &str,
) -> Result<u32, HleError> {
    let cached = *slot(&mut ctx.os.coreinit);
    if cached != 0 {
        return Ok(cached);
    }
    let table = ctx.os.registry.allocate_data(8, 4);
    let alloc = ctx.os.registry.function_stub("coreinit", alloc);
    let free = ctx.os.registry.function_stub("coreinit", free);
    ctx.os.registry.commit(ctx.memory)?;
    ctx.memory.write_u32(table, alloc)?;
    ctx.memory.write_u32(table + 4, free)?;
    *slot(&mut ctx.os.coreinit) = table;
    Ok(table)
}

/// Fills `MEMAllocator`: function table, heap, and two parameters.
fn init_allocator(
    ctx: &mut HleContext<'_>,
    allocator: u32,
    functions: u32,
    fields: [u32; 3],
) -> Result<(), HleError> {
    ctx.memory.write_u32(allocator, functions)?;
    for (i, value) in (1..).zip(fields) {
        ctx.memory.write_u32(allocator + i * 4, value)?;
    }
    Ok(())
}

#[hle_export(lib = "coreinit", name = "MEMInitAllocatorForDefaultHeap")]
fn mem_init_allocator_for_default_heap(
    ctx: &mut HleContext<'_>,
    allocator: u32,
) -> Result<(), HleError> {
    let functions = allocator_functions(
        ctx,
        |state| &mut state.default_allocator_functions,
        ALLOCATOR_DEFAULT_ALLOC.name,
        ALLOCATOR_DEFAULT_FREE.name,
    )?;
    init_allocator(ctx, allocator, functions, [0; 3])
}

#[hle_export(lib = "coreinit", name = "MEMInitAllocatorForExpHeap")]
fn mem_init_allocator_for_exp_heap(
    ctx: &mut HleContext<'_>,
    allocator: u32,
    heap: u32,
    align: u32,
) -> Result<(), HleError> {
    let functions = allocator_functions(
        ctx,
        |state| &mut state.exp_allocator_functions,
        ALLOCATOR_EXP_ALLOC.name,
        ALLOCATOR_EXP_FREE.name,
    )?;
    init_allocator(ctx, allocator, functions, [heap, align, 0])
}

/// Tail-calls `funcs[index]` of the allocator in `r3`, keeping the caller's `lr`, as
/// coreinit does: titles may fill `MEMAllocator` with functions of their own.
fn call_allocator(ctx: &mut HleContext<'_>, allocator: u32, index: u32) -> Result<(), HleError> {
    let functions = ctx.memory.read_u32(allocator)?;
    ctx.cpu.pc = ctx.memory.read_u32(functions + index * 4)?;
    Ok(())
}

#[hle_export(lib = "coreinit", name = "MEMAllocFromAllocator")]
fn mem_alloc_from_allocator(
    ctx: &mut HleContext<'_>,
    allocator: u32,
    size: u32,
) -> Result<(), HleError> {
    ctx.cpu.gpr[4] = size;
    call_allocator(ctx, allocator, 0)
}

#[hle_export(lib = "coreinit", name = "MEMFreeToAllocator")]
fn mem_free_to_allocator(
    ctx: &mut HleContext<'_>,
    allocator: u32,
    block: u32,
) -> Result<(), HleError> {
    ctx.cpu.gpr[4] = block;
    call_allocator(ctx, allocator, 1)
}

#[cfg(test)]
mod tests {
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn unit_heap_hands_out_and_recycles_blocks() {
        let mut env = TestEnv::new();
        let heap = env.call("coreinit", "MEMCreateUnitHeapEx", &[SCRATCH, 0x140, 0x40, 0x40, 0]);
        assert_eq!(heap, SCRATCH);
        assert_eq!(env.memory.read_u32(heap).unwrap(), 0x554E_5448);
        assert_eq!(env.memory.read_u32(heap + 0x1C).unwrap(), SCRATCH + 0x140);
        assert_eq!(env.call("coreinit", "MEMCountFreeBlockForUnitHeap", &[heap]), 4);
        let first = env.call("coreinit", "MEMAllocFromUnitHeap", &[heap]);
        assert_eq!(first, SCRATCH + 0x40);
        let second = env.call("coreinit", "MEMAllocFromUnitHeap", &[heap]);
        assert_eq!(second, SCRATCH + 0x80);
        env.call("coreinit", "MEMFreeToUnitHeap", &[heap, first]);
        assert_eq!(env.call("coreinit", "MEMAllocFromUnitHeap", &[heap]), first);
    }

    /// Intent: an expanded-heap allocator hands `MEMAllocFromAllocator` and
    /// `MEMFreeToAllocator` on to its function table, as a guest call would.
    /// Pass: the allocator call jumps to the table's alloc stub with the size in r4, and
    /// that stub allocates from the heap with the allocator's alignment.
    #[test]
    fn exp_heap_allocator_dispatches_through_its_table() {
        let mut env = TestEnv::new();
        let heap = env.call("coreinit", "MEMCreateExpHeapEx", &[SCRATCH, 0x1000, 0]);
        let allocator = SCRATCH + 0x2000;
        env.call("coreinit", "MEMInitAllocatorForExpHeap", &[allocator, heap, 0x40]);
        let table = env.memory.read_u32(allocator).unwrap();
        assert_eq!(env.memory.read_u32(allocator + 4).unwrap(), heap);
        env.cpu.pc = 0;
        env.call("coreinit", "MEMAllocFromAllocator", &[allocator, 0x30]);
        assert_eq!(env.cpu.pc, env.memory.read_u32(table).unwrap());
        assert_eq!(env.cpu.gpr[4], 0x30);
        let block = env.call("coreinit", "__shocora_AllocatorExpAlloc", &[allocator, 0x30]);
        assert!(block > heap && block.is_multiple_of(0x40), "{block:#x}");
        env.call("coreinit", "MEMFreeToAllocator", &[allocator, block]);
        assert_eq!(env.cpu.pc, env.memory.read_u32(table + 4).unwrap());
    }
}
