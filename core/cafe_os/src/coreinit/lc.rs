//! Locked cache: 16 KiB of scratch memory per core, allocated in 512-byte
//! blocks. The DMA engine that fills it is a plain copy here.
use wiiu_hle_macros::hle_export;
use wiiu_mem_addr::LOCKED_CACHE;

use crate::hle::{HleContext, HleError};

/// Allocation granularity.
const GRANULARITY: u32 = 0x200;
/// Size of each core's window.
const CACHE_SIZE: u32 = 0x4000;
/// Distance between the windows of consecutive cores.
const CORE_STRIDE: u32 = 0x4_0000;
const BLOCK_COUNT: usize = (CACHE_SIZE / GRANULARITY) as usize;
/// `LCLoadDMABlocks` counts in 32-byte blocks; zero stands for 128 of them.
const DMA_BLOCK_SIZE: u32 = 32;
const DMA_MAX_BLOCKS: u32 = 128;

/// Allocation state of every core's window.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct LockedCache {
    /// Length of the allocation starting at each block, or 0 when none does.
    allocations: [[u8; BLOCK_COUNT]; 3],
    used: [[bool; BLOCK_COUNT]; 3],
    dma_enabled: [u32; 3],
}

impl LockedCache {
    fn alloc(&mut self, core: usize, size: u32) -> Option<u32> {
        if size == 0 || size > CACHE_SIZE {
            return None;
        }
        let blocks = size.div_ceil(GRANULARITY) as usize;
        let start = (0..=BLOCK_COUNT - blocks)
            .find(|i| self.used[core][*i..*i + blocks].iter().all(|used| !used))?;
        self.used[core][start..start + blocks].fill(true);
        #[allow(clippy::cast_possible_truncation)]
        {
            self.allocations[core][start] = blocks as u8;
        }
        Some(base(core) + start as u32 * GRANULARITY)
    }

    fn free(&mut self, core: usize, address: u32) {
        let Some(offset) = address.checked_sub(base(core)).filter(|o| *o < CACHE_SIZE) else {
            log::warn!("LCDealloc: {address:#010x} is outside core {core}'s window");
            return;
        };
        let start = (offset / GRANULARITY) as usize;
        let blocks = usize::from(self.allocations[core][start]);
        self.used[core][start..start + blocks].fill(false);
        self.allocations[core][start] = 0;
    }

    #[allow(clippy::cast_possible_truncation)]
    fn unallocated(&self, core: usize) -> u32 {
        self.used[core].iter().filter(|used| !**used).count() as u32 * GRANULARITY
    }
}

const fn base(core: usize) -> u32 {
    LOCKED_CACHE.start + core as u32 * CORE_STRIDE
}

fn core(ctx: &HleContext<'_>) -> usize {
    (ctx.cpu.core_id() as usize).min(2)
}

#[hle_export(lib = "coreinit", name = "LCGetMaxSize")]
fn lc_get_max_size(_ctx: &mut HleContext<'_>) -> u32 {
    CACHE_SIZE
}

#[hle_export(lib = "coreinit", name = "LCAlloc")]
fn lc_alloc(ctx: &mut HleContext<'_>, size: u32) -> u32 {
    let core = core(ctx);
    ctx.os.coreinit.locked_cache.alloc(core, size).unwrap_or(0)
}

#[hle_export(lib = "coreinit", name = "LCDealloc")]
fn lc_dealloc(ctx: &mut HleContext<'_>, address: u32) {
    let core = core(ctx);
    ctx.os.coreinit.locked_cache.free(core, address);
}

#[hle_export(lib = "coreinit", name = "LCGetUnallocated")]
fn lc_get_unallocated(ctx: &mut HleContext<'_>) -> u32 {
    let core = core(ctx);
    ctx.os.coreinit.locked_cache.unallocated(core)
}

#[hle_export(lib = "coreinit", name = "LCGetAllocatableSize")]
fn lc_get_allocatable_size(ctx: &mut HleContext<'_>) -> u32 {
    let core = core(ctx);
    ctx.os.coreinit.locked_cache.unallocated(core)
}

#[hle_export(lib = "coreinit", name = "LCHardwareIsAvailable")]
fn lc_hardware_is_available(_ctx: &mut HleContext<'_>) -> bool {
    true
}

#[hle_export(lib = "coreinit", name = "LCEnableDMA")]
fn lc_enable_dma(ctx: &mut HleContext<'_>) -> bool {
    let core = core(ctx);
    ctx.os.coreinit.locked_cache.dma_enabled[core] += 1;
    true
}

#[hle_export(lib = "coreinit", name = "LCDisableDMA")]
fn lc_disable_dma(ctx: &mut HleContext<'_>) {
    let core = core(ctx);
    let count = &mut ctx.os.coreinit.locked_cache.dma_enabled[core];
    *count = count.saturating_sub(1);
}

#[hle_export(lib = "coreinit", name = "LCIsDMAEnabled")]
fn lc_is_dma_enabled(ctx: &mut HleContext<'_>) -> bool {
    let core = core(ctx);
    ctx.os.coreinit.locked_cache.dma_enabled[core] != 0
}

fn dma_copy(ctx: &mut HleContext<'_>, dst: u32, src: u32, blocks: u32) -> Result<(), HleError> {
    let size = if blocks == 0 { DMA_MAX_BLOCKS } else { blocks } * DMA_BLOCK_SIZE;
    let mut data = vec![0; size as usize];
    ctx.memory.read_bytes(src, &mut data)?;
    ctx.memory.write_bytes(dst, &data)?;
    Ok(())
}

#[hle_export(lib = "coreinit", name = "LCLoadDMABlocks")]
fn lc_load_dma_blocks(
    ctx: &mut HleContext<'_>,
    cache: u32,
    memory: u32,
    blocks: u32,
) -> Result<(), HleError> {
    dma_copy(ctx, cache, memory, blocks)
}

#[hle_export(lib = "coreinit", name = "LCStoreDMABlocks")]
fn lc_store_dma_blocks(
    ctx: &mut HleContext<'_>,
    memory: u32,
    cache: u32,
    blocks: u32,
) -> Result<(), HleError> {
    dma_copy(ctx, memory, cache, blocks)
}

/// Copies finish immediately.
#[hle_export(lib = "coreinit", name = "LCWaitDMAQueue")]
fn lc_wait_dma_queue(_ctx: &mut HleContext<'_>, _length: u32) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn allocates_blocks_and_reuses_freed_ones() {
        let mut env = TestEnv::new();
        let first = env.call("coreinit", "LCAlloc", &[0x300]);
        let second = env.call("coreinit", "LCAlloc", &[0x200]);
        assert_eq!(second - first, 0x400, "sizes round up to 512-byte blocks");
        assert_eq!(env.call("coreinit", "LCGetUnallocated", &[]), CACHE_SIZE - 0x600);
        env.call("coreinit", "LCDealloc", &[first]);
        assert_eq!(env.call("coreinit", "LCAlloc", &[0x200]), first);
        assert_eq!(env.call("coreinit", "LCAlloc", &[CACHE_SIZE + 1]), 0);
    }

    #[test]
    fn dma_copies_exactly_the_blocks_asked_for() {
        let mut env = TestEnv::new();
        env.memory.map(LOCKED_CACHE).unwrap();
        let cache = env.call("coreinit", "LCAlloc", &[0x200]);
        env.memory.fill(cache, 0x80, 0xAA).unwrap();
        env.memory.fill(SCRATCH, 0x80, 0x55).unwrap();
        env.call("coreinit", "LCStoreDMABlocks", &[SCRATCH, cache, 2]);
        assert_eq!(env.memory.read_u8(SCRATCH + 0x3F).unwrap(), 0xAA);
        assert_eq!(env.memory.read_u8(SCRATCH + 0x40).unwrap(), 0x55, "nothing past 2 blocks");
    }
}
