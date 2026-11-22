//! `OSDynLoad`: run-time symbol lookup. Every system RPL is HLE, so lookups
//! resolve to HLE stubs (or a logged placeholder for unimplemented exports).
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ "find function X"  │   │ coreinit/dynload.rs      │   │ function addresses │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ run-time library lookup  │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Games can look up OS functions by name while running; this returns the matching emulated
//! function. Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
use wiiu_hle_macros::hle_export;
use wiiu_memory::GuestPtr;

use crate::hle::{HleContext, HleError};

#[hle_export(lib = "coreinit", name = "OSDynLoad_Acquire")]
fn os_dyn_load_acquire(
    ctx: &mut HleContext<'_>,
    name: GuestPtr<u8>,
    out: GuestPtr<u32>,
) -> Result<i32, HleError> {
    let library = ctx.memory.read_c_string(name.address(), 256)?;
    let handle = ctx.os.registry.library_handle(&library);
    if !out.is_null() {
        out.write(ctx.memory, handle)?;
    }
    Ok(0)
}

#[hle_export(lib = "coreinit", name = "OSDynLoad_FindExport")]
fn os_dyn_load_find_export(
    ctx: &mut HleContext<'_>,
    handle: u32,
    is_data: bool,
    name: GuestPtr<u8>,
    out: GuestPtr<u32>,
) -> Result<i32, HleError> {
    let symbol = ctx.memory.read_c_string(name.address(), 256)?;
    let Some(library) = ctx.os.registry.library_name(handle).map(str::to_owned) else {
        return Ok(-1);
    };
    let address = if is_data {
        ctx.os.registry.data_address(&library, &symbol)
    } else {
        ctx.os.registry.function_stub(&library, &symbol)
    };
    ctx.os.registry.commit(ctx.memory)?;
    out.write(ctx.memory, address)?;
    Ok(0)
}

#[hle_export(lib = "coreinit", name = "OSDynLoad_Release")]
fn os_dyn_load_release(_ctx: &mut HleContext<'_>, _handle: u32) {}

/// Stores the allocator the RPL loader would use for module memory.
#[hle_export(lib = "coreinit", name = "OSDynLoad_SetAllocator")]
fn os_dyn_load_set_allocator(ctx: &mut HleContext<'_>, alloc: u32, free: u32) -> i32 {
    ctx.os.coreinit.dynload_allocator = (alloc, free);
    0
}

#[hle_export(lib = "coreinit", name = "OSDynLoad_GetAllocator")]
fn os_dyn_load_get_allocator(
    ctx: &mut HleContext<'_>,
    alloc_out: GuestPtr<u32>,
    free_out: GuestPtr<u32>,
) -> Result<i32, HleError> {
    let (alloc, free) = ctx.os.coreinit.dynload_allocator;
    if !alloc_out.is_null() {
        alloc_out.write(ctx.memory, alloc)?;
    }
    if !free_out.is_null() {
        free_out.write(ctx.memory, free)?;
    }
    Ok(0)
}

/// TLS is not emulated; the allocator is accepted and ignored.
#[hle_export(lib = "coreinit", name = "OSDynLoad_SetTLSAllocator")]
fn os_dyn_load_set_tls_allocator(_ctx: &mut HleContext<'_>, _alloc: u32, _free: u32) -> i32 {
    0
}

#[cfg(test)]
mod tests {
    use wiiu_cpu::encode_hle_call;

    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn finds_hle_exports() {
        let mut env = TestEnv::new();
        env.memory.write_c_string(SCRATCH, "coreinit.rpl").unwrap();
        env.memory.write_c_string(SCRATCH + 0x20, "OSGetCoreId").unwrap();
        assert_eq!(env.call("coreinit", "OSDynLoad_Acquire", &[SCRATCH, SCRATCH + 0x40]), 0);
        let handle = env.memory.read_u32(SCRATCH + 0x40).unwrap();
        assert_eq!(
            env.call(
                "coreinit",
                "OSDynLoad_FindExport",
                &[handle, 0, SCRATCH + 0x20, SCRATCH + 0x44]
            ),
            0
        );
        let stub = env.memory.read_u32(SCRATCH + 0x44).unwrap();
        let index = env.os.registry.function_index("coreinit", "OSGetCoreId").unwrap();
        assert_eq!(env.memory.read_u32(stub).unwrap(), encode_hle_call(index));
    }
}
