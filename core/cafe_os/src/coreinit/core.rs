//! Core identification.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ "which core am I?" │   │ coreinit/core.rs         │   │ core number 0-2    │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ core identification      │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Answers which of the three CPU cores the calling thread runs on. Terms such as HLE, JIT or
//! shader are explained in `docs/guide/wiiu_term.md`.
use wiiu_hle_macros::hle_export;

use crate::hle::{HleContext, HleError};

/// Games run their main thread on core 1.
pub const MAIN_CORE: u32 = 1;
pub const CORE_COUNT: u32 = 3;

#[hle_export(lib = "coreinit", name = "OSGetCoreId")]
fn os_get_core_id(ctx: &mut HleContext<'_>) -> u32 {
    ctx.cpu.core_id()
}

#[hle_export(lib = "coreinit", name = "OSGetCoreCount")]
fn os_get_core_count(_ctx: &mut HleContext<'_>) -> u32 {
    CORE_COUNT
}

#[hle_export(lib = "coreinit", name = "OSGetMainCoreId")]
fn os_get_main_core_id(_ctx: &mut HleContext<'_>) -> u32 {
    MAIN_CORE
}

#[hle_export(lib = "coreinit", name = "OSIsMainCore")]
fn os_is_main_core(ctx: &mut HleContext<'_>) -> bool {
    ctx.cpu.core_id() == MAIN_CORE
}

/// Espresso bus clock in Hz.
const BUS_CLOCK: u32 = 248_625_000;
/// Espresso core clock in Hz.
const CORE_CLOCK: u32 = 1_243_125_000;
/// L2 cache size per core (core 1 has 2 MiB).
const L2_SIZES: [u32; 3] = [0x8_0000, 0x20_0000, 0x8_0000];
/// `sizeof(OSSystemInfo)`.
const SYSTEM_INFO_SIZE: u32 = 0x20;

/// Pointer to the static `OSSystemInfo`, created on first use.
#[hle_export(lib = "coreinit", name = "OSGetSystemInfo")]
fn os_get_system_info(ctx: &mut HleContext<'_>) -> Result<u32, HleError> {
    if ctx.os.coreinit.system_info == 0 {
        let info = ctx.os.registry.allocate_data(SYSTEM_INFO_SIZE, 8);
        ctx.memory.write_u32(info, BUS_CLOCK)?;
        ctx.memory.write_u32(info + 4, CORE_CLOCK)?;
        ctx.memory.write_u64(info + 8, 0)?;
        for (i, size) in (0_u32..).zip(L2_SIZES) {
            ctx.memory.write_u32(info + 0x10 + 4 * i, size)?;
        }
        ctx.memory.write_u32(info + 0x1C, 5)?; // CPU ratio (core / bus clock)
        ctx.os.coreinit.system_info = info;
    }
    Ok(ctx.os.coreinit.system_info)
}

#[hle_export(lib = "coreinit", name = "OSIsDebuggerInitialized")]
fn os_is_debugger_initialized(_ctx: &mut HleContext<'_>) -> bool {
    false
}

/// Shared no-op target for C runtime hook pointers (`__cpp_exception_init_ptr`, ...).
#[hle_export(lib = "coreinit", name = "__shocora_Nop")]
fn nop(_ctx: &mut HleContext<'_>) {}

#[cfg(test)]
mod tests {
    use crate::hle::test_env::TestEnv;

    #[test]
    fn system_info_is_stable() {
        let mut env = TestEnv::new();
        let info = env.call("coreinit", "OSGetSystemInfo", &[]);
        assert_eq!(env.call("coreinit", "OSGetSystemInfo", &[]), info);
        assert_eq!(env.memory.read_u32(info).unwrap(), super::BUS_CLOCK);
    }
}
