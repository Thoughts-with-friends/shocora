//! Cache maintenance and hardware queries.
//!
//! The emulator has no caches. Flushes still tell the GPU which memory the CPU wrote,
//! so it looks at those buffers again; `DCZeroRange` also zeroes whole cache lines.
use wiiu_hle_macros::hle_export;

use crate::hle::{HleContext, HleError};

pub(crate) const CACHE_LINE: u32 = 32;
/// Retail console (`OSGetConsoleType`).
const CONSOLE_TYPE_RETAIL: u32 = 0x0300_0050;
/// Latte hardware version reported by `bspGetHardwareVersion`.
const HARDWARE_VERSION: u32 = 0x1100_0000;

/// Memory barriers order the guest's own accesses: one host thread runs them in order.
#[hle_export(lib = "coreinit", name = "OSMemoryBarrier")]
fn os_memory_barrier(_ctx: &mut HleContext<'_>) {}

#[hle_export(lib = "coreinit", name = "OSCoherencyBarrier")]
fn os_coherency_barrier(_ctx: &mut HleContext<'_>) {}

#[hle_export(lib = "coreinit", name = "OSIsDebuggerPresent")]
fn os_is_debugger_present(_ctx: &mut HleContext<'_>) -> bool {
    false
}

#[hle_export(lib = "coreinit", name = "DCFlushRange")]
fn dc_flush_range(ctx: &mut HleContext<'_>, address: u32, size: u32) {
    ctx.gpu.note_cpu_write(address, size);
}

#[hle_export(lib = "coreinit", name = "DCFlushRangeNoSync")]
fn dc_flush_range_no_sync(ctx: &mut HleContext<'_>, address: u32, size: u32) {
    ctx.gpu.note_cpu_write(address, size);
}

#[hle_export(lib = "coreinit", name = "DCStoreRange")]
fn dc_store_range(ctx: &mut HleContext<'_>, address: u32, size: u32) {
    ctx.gpu.note_cpu_write(address, size);
}

#[hle_export(lib = "coreinit", name = "DCStoreRangeNoSync")]
fn dc_store_range_no_sync(ctx: &mut HleContext<'_>, address: u32, size: u32) {
    ctx.gpu.note_cpu_write(address, size);
}

#[hle_export(lib = "coreinit", name = "DCInvalidateRange")]
fn dc_invalidate_range(_ctx: &mut HleContext<'_>, _address: u32, _size: u32) {}

#[hle_export(lib = "coreinit", name = "DCTouchRange")]
fn dc_touch_range(_ctx: &mut HleContext<'_>, _address: u32, _size: u32) {}

#[hle_export(lib = "coreinit", name = "ICInvalidateRange")]
fn ic_invalidate_range(_ctx: &mut HleContext<'_>, _address: u32, _size: u32) {}

#[hle_export(lib = "coreinit", name = "DCZeroRange")]
fn dc_zero_range(ctx: &mut HleContext<'_>, address: u32, size: u32) -> Result<(), HleError> {
    let start = address & !(CACHE_LINE - 1);
    let end = address.saturating_add(size).next_multiple_of(CACHE_LINE);
    ctx.memory.fill(start, end - start, 0)?;
    ctx.gpu.note_cpu_write(start, end - start);
    Ok(())
}

#[hle_export(lib = "coreinit", name = "OSEnforceInorderIO")]
fn os_enforce_inorder_io(_ctx: &mut HleContext<'_>) {}

#[hle_export(lib = "coreinit", name = "OSGetConsoleType")]
fn os_get_console_type(_ctx: &mut HleContext<'_>) -> u32 {
    CONSOLE_TYPE_RETAIL
}

#[hle_export(lib = "coreinit", name = "bspGetHardwareVersion")]
fn bsp_get_hardware_version(ctx: &mut HleContext<'_>, version_out: u32) -> Result<bool, HleError> {
    ctx.memory.write_u32(version_out, HARDWARE_VERSION)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use crate::hle::test_env::{SCRATCH, TestEnv};

    /// Intent: `DCZeroRange` zeroes whole 32-byte cache lines, as `dcbz` does on the CPU,
    /// so bytes around an unaligned range are cleared too.
    /// Pass: zeroing 4 bytes at line offset 8 clears the whole line and nothing after it.
    #[test]
    fn zero_range_clears_whole_cache_lines() {
        let mut env = TestEnv::new();
        env.memory.fill(SCRATCH, 0x40, 0xAA).unwrap();
        env.call("coreinit", "DCZeroRange", &[SCRATCH + 8, 4]);
        assert_eq!(env.memory.read_u32(SCRATCH).unwrap(), 0);
        assert_eq!(env.memory.read_u32(SCRATCH + 0x1C).unwrap(), 0);
        assert_eq!(env.memory.read_u8(SCRATCH + 0x20).unwrap(), 0xAA);
    }

    /// Intent: titles that check the hardware see a retail console.
    /// Pass: `OSGetConsoleType` is retail (0x03000050); `bspGetHardwareVersion` writes a
    /// version and returns true.
    #[test]
    fn reports_a_retail_console() {
        let mut env = TestEnv::new();
        assert_eq!(env.call("coreinit", "OSGetConsoleType", &[]), 0x0300_0050);
        assert_eq!(env.call("coreinit", "bspGetHardwareVersion", &[SCRATCH]), 1);
        assert_ne!(env.memory.read_u32(SCRATCH).unwrap(), 0);
    }
}
