//! Memory primitives exported by coreinit.
#![allow(clippy::cast_possible_truncation)]

use wiiu_hle_macros::hle_export;
use wiiu_mem_addr::OVERLAY_ARENA;
use wiiu_memory::GuestPtr;

use crate::hle::{HleContext, HleError};

#[hle_export(lib = "coreinit", name = "memcpy")]
fn memcpy(
    ctx: &mut HleContext<'_>,
    dst: GuestPtr<u8>,
    src: GuestPtr<u8>,
    size: u32,
) -> Result<GuestPtr<u8>, HleError> {
    ctx.memory.copy_within(dst.address(), src.address(), size)?;
    ctx.gpu.note_cpu_write(dst.address(), size);
    Ok(dst)
}

#[hle_export(lib = "coreinit", name = "memmove")]
fn memmove(
    ctx: &mut HleContext<'_>,
    dst: GuestPtr<u8>,
    src: GuestPtr<u8>,
    size: u32,
) -> Result<GuestPtr<u8>, HleError> {
    ctx.memory.copy_within(dst.address(), src.address(), size)?;
    ctx.gpu.note_cpu_write(dst.address(), size);
    Ok(dst)
}

#[hle_export(lib = "coreinit", name = "memset")]
fn memset(
    ctx: &mut HleContext<'_>,
    dst: GuestPtr<u8>,
    value: u32,
    size: u32,
) -> Result<GuestPtr<u8>, HleError> {
    ctx.memory.fill(dst.address(), size, value as u8)?;
    ctx.gpu.note_cpu_write(dst.address(), size);
    Ok(dst)
}

#[hle_export(lib = "coreinit", name = "OSBlockMove")]
fn os_block_move(
    ctx: &mut HleContext<'_>,
    dst: GuestPtr<u8>,
    src: GuestPtr<u8>,
    size: u32,
    _flush: bool,
) -> Result<GuestPtr<u8>, HleError> {
    ctx.memory.copy_within(dst.address(), src.address(), size)?;
    ctx.gpu.note_cpu_write(dst.address(), size);
    Ok(dst)
}

#[hle_export(lib = "coreinit", name = "OSBlockSet")]
fn os_block_set(
    ctx: &mut HleContext<'_>,
    dst: GuestPtr<u8>,
    value: u32,
    size: u32,
) -> Result<GuestPtr<u8>, HleError> {
    ctx.memory.fill(dst.address(), size, value as u8)?;
    ctx.gpu.note_cpu_write(dst.address(), size);
    Ok(dst)
}

fn overlay_arena_enabled(ctx: &HleContext<'_>) -> bool {
    ctx.memory.regions().any(|region| region == OVERLAY_ARENA)
}

fn write_overlay_arena_range(
    ctx: &mut HleContext<'_>,
    start: GuestPtr<u32>,
    size: GuestPtr<u32>,
) -> Result<(), HleError> {
    if !start.is_null() {
        start.write(ctx.memory, OVERLAY_ARENA.start)?;
    }
    if !size.is_null() {
        size.write(ctx.memory, OVERLAY_ARENA.size)?;
    }
    Ok(())
}

#[hle_export(lib = "coreinit", name = "OSIsEnabledOverlayArena")]
fn os_is_enabled_overlay_arena(ctx: &mut HleContext<'_>) -> bool {
    overlay_arena_enabled(ctx)
}

/// Lends the title the overlay arena (memory the system menu would otherwise
/// use) and reports where it is.
#[hle_export(lib = "coreinit", name = "OSEnableOverlayArena")]
fn os_enable_overlay_arena(
    ctx: &mut HleContext<'_>,
    _unknown: u32,
    start: GuestPtr<u32>,
    size: GuestPtr<u32>,
) -> Result<(), HleError> {
    if !overlay_arena_enabled(ctx) {
        ctx.memory.map(OVERLAY_ARENA)?;
    }
    write_overlay_arena_range(ctx, start, size)
}

/// The arena stays mapped: freeing it would only let stale pointers fault.
#[hle_export(lib = "coreinit", name = "OSDisableOverlayArena")]
fn os_disable_overlay_arena(_ctx: &mut HleContext<'_>) {}

#[hle_export(lib = "coreinit", name = "OSGetOverlayArenaRange")]
fn os_get_overlay_arena_range(
    ctx: &mut HleContext<'_>,
    start: GuestPtr<u32>,
    size: GuestPtr<u32>,
) -> Result<(), HleError> {
    write_overlay_arena_range(ctx, start, size)
}

/// Whether `address` is mapped for the title.
#[hle_export(lib = "coreinit", name = "OSIsAddressValid")]
fn os_is_address_valid(ctx: &mut HleContext<'_>, address: u32) -> bool {
    ctx.memory.regions().any(|region| region.contains(address))
}

#[cfg(test)]
mod tests {
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn copy_and_fill() {
        let mut env = TestEnv::new();
        env.memory.write_c_string(SCRATCH, "abc").unwrap();
        assert_eq!(env.call("coreinit", "memcpy", &[SCRATCH + 16, SCRATCH, 4]), SCRATCH + 16);
        assert_eq!(env.memory.read_c_string(SCRATCH + 16, 8).unwrap(), "abc");
        env.call("coreinit", "memset", &[SCRATCH, u32::from(b'z'), 2]);
        assert_eq!(env.memory.read_c_string(SCRATCH, 8).unwrap(), "zzc");
    }
}
