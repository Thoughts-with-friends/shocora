//! `OS*Atomic*`: read-modify-write of guest words. Guest code runs between HLE calls only,
//! so each call is atomic as it is.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ shared counters    │   │ coreinit/atomic.rs       │   │ safely updated     │
//! │                    │──►│ ──────────────────────── │──►│ values             │
//! │                    │   │ atomic operations        │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Read-modify-write of a value that several threads share, without one undoing another's
//! change. Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
use wiiu_hle_macros::hle_export;
use wiiu_memory::GuestPtr;

use crate::hle::{HleContext, HleError};

fn read64(ctx: &HleContext<'_>, ptr: GuestPtr<u8>) -> Result<u64, HleError> {
    let high = ctx.memory.read_u32(ptr.address())?;
    let low = ctx.memory.read_u32(ptr.address().wrapping_add(4))?;
    Ok((u64::from(high) << 32) | u64::from(low))
}

fn write64(ctx: &mut HleContext<'_>, ptr: GuestPtr<u8>, value: u64) -> Result<(), HleError> {
    ctx.memory.write_u32(ptr.address(), (value >> 32) as u32)?;
    ctx.memory.write_u32(ptr.address().wrapping_add(4), value as u32)?;
    Ok(())
}

/// Replaces the word at `ptr` with `update(old)`; returns the old value.
fn update32(
    ctx: &mut HleContext<'_>,
    ptr: GuestPtr<u8>,
    update: impl FnOnce(u32) -> u32,
) -> Result<u32, HleError> {
    let old = ctx.memory.read_u32(ptr.address())?;
    ctx.memory.write_u32(ptr.address(), update(old))?;
    Ok(old)
}

/// Replaces the doubleword at `ptr` with `update(old)`; returns the old value.
fn update64(
    ctx: &mut HleContext<'_>,
    ptr: GuestPtr<u8>,
    update: impl FnOnce(u64) -> u64,
) -> Result<u64, HleError> {
    let old = read64(ctx, ptr)?;
    write64(ctx, ptr, update(old))?;
    Ok(old)
}

#[hle_export(lib = "coreinit", name = "OSSwapAtomic")]
fn os_swap_atomic(
    ctx: &mut HleContext<'_>,
    ptr: GuestPtr<u8>,
    value: u32,
) -> Result<u32, HleError> {
    update32(ctx, ptr, |_| value)
}

#[hle_export(lib = "coreinit", name = "OSAddAtomic")]
fn os_add_atomic(ctx: &mut HleContext<'_>, ptr: GuestPtr<u8>, value: u32) -> Result<u32, HleError> {
    update32(ctx, ptr, |old| old.wrapping_add(value))
}

#[hle_export(lib = "coreinit", name = "OSCompareAndSwapAtomic")]
fn os_compare_and_swap_atomic(
    ctx: &mut HleContext<'_>,
    ptr: GuestPtr<u8>,
    compare: u32,
    swap: u32,
) -> Result<bool, HleError> {
    let old = update32(ctx, ptr, |old| if old == compare { swap } else { old })?;
    Ok(old == compare)
}

#[hle_export(lib = "coreinit", name = "OSCompareAndSwapAtomicEx")]
fn os_compare_and_swap_atomic_ex(
    ctx: &mut HleContext<'_>,
    ptr: GuestPtr<u8>,
    compare: u32,
    swap: u32,
    previous: GuestPtr<u8>,
) -> Result<bool, HleError> {
    let old = update32(ctx, ptr, |old| if old == compare { swap } else { old })?;
    ctx.memory.write_u32(previous.address(), old)?;
    Ok(old == compare)
}

#[hle_export(lib = "coreinit", name = "OSGetAtomic64")]
fn os_get_atomic64(ctx: &mut HleContext<'_>, ptr: GuestPtr<u8>) -> Result<u64, HleError> {
    read64(ctx, ptr)
}

#[hle_export(lib = "coreinit", name = "OSSetAtomic64")]
fn os_set_atomic64(
    ctx: &mut HleContext<'_>,
    ptr: GuestPtr<u8>,
    value: u64,
) -> Result<u64, HleError> {
    update64(ctx, ptr, |_| value)
}

#[hle_export(lib = "coreinit", name = "OSSwapAtomic64")]
fn os_swap_atomic64(
    ctx: &mut HleContext<'_>,
    ptr: GuestPtr<u8>,
    value: u64,
) -> Result<u64, HleError> {
    update64(ctx, ptr, |_| value)
}

#[hle_export(lib = "coreinit", name = "OSAddAtomic64")]
fn os_add_atomic64(
    ctx: &mut HleContext<'_>,
    ptr: GuestPtr<u8>,
    value: u64,
) -> Result<u64, HleError> {
    update64(ctx, ptr, |old| old.wrapping_add(value))
}

#[hle_export(lib = "coreinit", name = "OSAndAtomic64")]
fn os_and_atomic64(
    ctx: &mut HleContext<'_>,
    ptr: GuestPtr<u8>,
    value: u64,
) -> Result<u64, HleError> {
    update64(ctx, ptr, |old| old & value)
}

#[hle_export(lib = "coreinit", name = "OSOrAtomic64")]
fn os_or_atomic64(
    ctx: &mut HleContext<'_>,
    ptr: GuestPtr<u8>,
    value: u64,
) -> Result<u64, HleError> {
    update64(ctx, ptr, |old| old | value)
}

#[hle_export(lib = "coreinit", name = "OSCompareAndSwapAtomic64")]
fn os_compare_and_swap_atomic64(
    ctx: &mut HleContext<'_>,
    ptr: GuestPtr<u8>,
    compare: u64,
    swap: u64,
) -> Result<bool, HleError> {
    let old = update64(ctx, ptr, |old| if old == compare { swap } else { old })?;
    Ok(old == compare)
}

#[hle_export(lib = "coreinit", name = "OSCompareAndSwapAtomicEx64")]
fn os_compare_and_swap_atomic_ex64(
    ctx: &mut HleContext<'_>,
    ptr: GuestPtr<u8>,
    compare: u64,
    swap: u64,
    previous: GuestPtr<u8>,
) -> Result<bool, HleError> {
    let old = update64(ctx, ptr, |old| if old == compare { swap } else { old })?;
    write64(ctx, previous, old)?;
    Ok(old == compare)
}

#[cfg(test)]
mod tests {
    use crate::hle::test_env::{SCRATCH, TestEnv};

    /// Intent: the 64-bit swap stores the new value and hands back the old one, split
    /// across r3 (high) and r4 (low) as the guest ABI returns doublewords.
    /// Pass: memory holds the new value; r3:r4 is the old one.
    #[test]
    fn swap_atomic64_returns_the_old_value() {
        let mut env = TestEnv::new();
        let word = SCRATCH;
        env.memory.write_u32(word, 0x1122_3344).unwrap();
        env.memory.write_u32(word + 4, 0x5566_7788).unwrap();
        // The doubleword argument takes the aligned pair r5:r6.
        env.call("coreinit", "OSSwapAtomic64", &[word, 0, 0xAABB_CCDD, 0xEEFF_0011]);
        assert_eq!(env.cpu.gpr[3], 0x1122_3344);
        assert_eq!(env.cpu.gpr[4], 0x5566_7788);
        assert_eq!(env.memory.read_u32(word).unwrap(), 0xAABB_CCDD);
        assert_eq!(env.memory.read_u32(word + 4).unwrap(), 0xEEFF_0011);
    }
}
