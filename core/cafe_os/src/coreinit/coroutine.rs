//! `OSCoroutine`: user-mode context switching (port of Cemu `coreinit_Coroutine.cpp`).
//!
//! A coroutine holds the non-volatile registers: lr, cr, gqr1, r1, r2, r13, r14..r31,
//! f14..f31 and their paired singles.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ game switches      │   │ coreinit/coroutine.rs    │   │ saved/restored     │
//! │ context            │──►│ ──────────────────────── │──►│ registers          │
//! │                    │   │ coroutines               │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Lets a game pause a piece of work and resume it later, by saving and restoring CPU
//! registers. Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
use wiiu_hle_macros::hle_export;

use crate::{
    hle::{HleContext, HleError},
    process::INTERNAL_LIBRARY,
};

/// `OSCoroutine` field offsets (0x180 bytes).
mod field {
    pub const LR: u32 = 0x00;
    pub const CR: u32 = 0x04;
    pub const GQR1: u32 = 0x08;
    pub const R1: u32 = 0x0C;
    pub const R2: u32 = 0x10;
    pub const R13: u32 = 0x14;
    pub const GPR: u32 = 0x18;
    pub const FPR: u32 = GPR + 18 * 4;
    pub const PSR: u32 = FPR + 18 * 8;
}

fn write64(ctx: &mut HleContext<'_>, address: u32, value: u64) -> Result<(), HleError> {
    ctx.memory.write_u32(address, (value >> 32) as u32)?;
    ctx.memory.write_u32(address + 4, value as u32)?;
    Ok(())
}

fn read64(ctx: &HleContext<'_>, address: u32) -> Result<u64, HleError> {
    let high = ctx.memory.read_u32(address)?;
    let low = ctx.memory.read_u32(address + 4)?;
    Ok((u64::from(high) << 32) | u64::from(low))
}

fn save(ctx: &mut HleContext<'_>, co: u32) -> Result<(), HleError> {
    let cpu = &*ctx.cpu;
    let words = [
        (field::LR, cpu.lr),
        (field::CR, cpu.cr),
        (field::GQR1, cpu.gqr[1]),
        (field::R1, cpu.gpr[1]),
        (field::R2, cpu.gpr[2]),
        (field::R13, cpu.gpr[13]),
    ];
    let gpr: [u32; 18] = std::array::from_fn(|i| cpu.gpr[14 + i]);
    let fpr: [(u64, u64); 18] =
        std::array::from_fn(|i| (cpu.fpr[14 + i].ps0.to_bits(), cpu.fpr[14 + i].ps1.to_bits()));
    for (offset, value) in words {
        ctx.memory.write_u32(co + offset, value)?;
    }
    for (i, (value, (ps0, ps1))) in (0_u32..).zip(gpr.into_iter().zip(fpr)) {
        ctx.memory.write_u32(co + field::GPR + i * 4, value)?;
        write64(ctx, co + field::FPR + i * 8, ps0)?;
        write64(ctx, co + field::PSR + i * 8, ps1)?;
    }
    Ok(())
}

fn load(ctx: &mut HleContext<'_>, co: u32) -> Result<(), HleError> {
    let lr = ctx.memory.read_u32(co + field::LR)?;
    let cr = ctx.memory.read_u32(co + field::CR)?;
    let gqr1 = ctx.memory.read_u32(co + field::GQR1)?;
    let r1 = ctx.memory.read_u32(co + field::R1)?;
    let r2 = ctx.memory.read_u32(co + field::R2)?;
    let r13 = ctx.memory.read_u32(co + field::R13)?;
    let mut gpr = [0_u32; 18];
    let mut fpr = [(0_u64, 0_u64); 18];
    for i in 0..18_u32 {
        gpr[i as usize] = ctx.memory.read_u32(co + field::GPR + i * 4)?;
        fpr[i as usize] =
            (read64(ctx, co + field::FPR + i * 8)?, read64(ctx, co + field::PSR + i * 8)?);
    }
    let cpu = &mut *ctx.cpu;
    cpu.lr = lr;
    cpu.cr = cr;
    cpu.gqr[1] = gqr1;
    cpu.gpr[1] = r1;
    cpu.gpr[2] = r2;
    cpu.gpr[13] = r13;
    for (i, (value, (ps0, ps1))) in gpr.into_iter().zip(fpr).enumerate() {
        cpu.gpr[14 + i] = value;
        cpu.fpr[14 + i].ps0 = f64::from_bits(ps0);
        cpu.fpr[14 + i].ps1 = f64::from_bits(ps1);
    }
    Ok(())
}

#[hle_export(lib = "coreinit", name = "OSInitCoroutine")]
fn os_init_coroutine(
    ctx: &mut HleContext<'_>,
    co: u32,
    entry: u32,
    stack: u32,
) -> Result<(), HleError> {
    ctx.memory.write_u32(co + field::LR, entry)?;
    ctx.memory.write_u32(co + field::R1, stack)?;
    Ok(())
}

/// `OSSwitchCoroutine(current, next)`: as on hardware it keeps r31 and lr on a 0x10-byte
/// stack frame, stores `next` in the saved r31 (Injustice reads it back) and resumes
/// through a stub that pops the frame.
#[hle_export(lib = "coreinit", name = "OSSwitchCoroutine")]
fn os_switch_coroutine(ctx: &mut HleContext<'_>, current: u32, next: u32) -> Result<(), HleError> {
    let resume = ctx.os.registry.function_stub(INTERNAL_LIBRARY, SWITCH_COROUTINE_RESUME.name);
    ctx.os.registry.commit(ctx.memory)?;
    ctx.cpu.gpr[1] = ctx.cpu.gpr[1].wrapping_sub(0x10);
    let sp = ctx.cpu.gpr[1];
    ctx.memory.write_u32(sp + 0xC, ctx.cpu.gpr[31])?;
    ctx.memory.write_u32(sp + 0x14, ctx.cpu.lr)?;
    ctx.cpu.lr = resume;
    ctx.cpu.gpr[31] = next;
    save(ctx, current)?;
    ctx.cpu.gpr[3] = next;
    ctx.cpu.gpr[4] = 1;
    load(ctx, next)?;
    ctx.cpu.pc = ctx.cpu.lr;
    Ok(())
}

/// A coroutine saved by `OSSwitchCoroutine` resumes: pop its frame and return to the caller.
#[hle_export(lib = "shocora", name = "__shocora_OSSwitchCoroutineResume")]
fn switch_coroutine_resume(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    let sp = ctx.cpu.gpr[1];
    ctx.cpu.gpr[31] = ctx.memory.read_u32(sp + 0xC)?;
    ctx.cpu.lr = ctx.memory.read_u32(sp + 0x14)?;
    ctx.cpu.gpr[1] = sp.wrapping_add(0x10);
    ctx.cpu.pc = ctx.cpu.lr;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::field;
    use crate::hle::test_env::{SCRATCH, TestEnv};

    /// Intent: switching saves the caller's registers and enters the other coroutine;
    /// switching back resumes the caller with its r31 and lr intact.
    /// Pass: after A→B the CPU runs B's entry on B's stack; B→A lands on the resume
    /// stub, which restores r31, lr and r1.
    #[test]
    fn switch_and_resume() {
        let mut env = TestEnv::new();
        let (a, b) = (SCRATCH, SCRATCH + 0x200);
        env.call("coreinit", "OSInitCoroutine", &[b, 0x0200_1000, SCRATCH + 0x8000]);
        env.cpu.gpr[1] = SCRATCH + 0x4000;
        env.cpu.gpr[31] = 0xAAAA;
        env.cpu.gpr[20] = 0x2020;
        env.cpu.lr = 0x0200_2000;
        env.call("coreinit", "OSSwitchCoroutine", &[a, b]);
        assert_eq!(env.cpu.pc, 0x0200_1000);
        assert_eq!(env.cpu.gpr[1], SCRATCH + 0x8000);
        assert_eq!(env.cpu.gpr[3], b);
        assert_eq!(env.memory.read_u32(a + field::GPR + 17 * 4).unwrap(), b, "r31 field = next");

        env.cpu.gpr[20] = 0;
        env.cpu.lr = 0x0200_3000;
        env.call("coreinit", "OSSwitchCoroutine", &[b, a]);
        assert_eq!(env.cpu.gpr[20], 0x2020);
        env.call("shocora", "__shocora_OSSwitchCoroutineResume", &[]);
        assert_eq!(env.cpu.pc, 0x0200_2000);
        assert_eq!(env.cpu.gpr[31], 0xAAAA);
        assert_eq!(env.cpu.gpr[1], SCRATCH + 0x4000);
    }
}
