//! `dmae`: the DMA engine. Copies and fills run at once, so every submission has
//! retired by the time it returns, and the time stamps are the system clock's.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ game queues a copy │   │ dmae.rs                  │   │ memory             │
//! │                    │──►│ ──────────────────────── │──►│ copied/filled      │
//! │                    │   │ DMA engine               │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! A helper chip that copies or fills memory so the CPU does not have to. Here the copy simply
//! happens at once. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.
use wiiu_hle_macros::hle_export;

use crate::hle::{HleContext, HleError, HleExport};

/// `DMAEEndianSwapMode`.
mod swap {
    pub const NONE: u32 = 0;
    /// Bytes swapped inside each 16-bit unit.
    pub const IN_16: u32 = 1;
    /// Bytes swapped inside each 32-bit unit.
    pub const IN_32: u32 = 2;
}

/// The time stamp of a submission, which has already retired.
#[allow(clippy::cast_possible_wrap)]
fn now(ctx: &HleContext<'_>) -> i64 {
    ctx.os.timebase.ticks_since_boot() as i64
}

/// `DMAECopyMem(dst, src, words, swap)`: copies `words` 32-bit words.
#[hle_export(lib = "dmae", name = "DMAECopyMem")]
fn dmae_copy_mem(
    ctx: &mut HleContext<'_>,
    dst: u32,
    src: u32,
    words: u32,
    mode: u32,
) -> Result<i64, HleError> {
    let size = words.saturating_mul(4);
    if size != 0 {
        let mut data = ctx.memory.slice(src, size)?.to_vec();
        match mode {
            swap::IN_16 => data.as_chunks_mut::<2>().0.iter_mut().for_each(|c| c.reverse()),
            swap::IN_32 => data.as_chunks_mut::<4>().0.iter_mut().for_each(|c| c.reverse()),
            swap::NONE => {}
            other => log::warn!("DMAECopyMem: unknown swap mode {other}"),
        }
        ctx.memory.write_bytes(dst, &data)?;
        ctx.gpu.note_cpu_write(dst, size);
    }
    Ok(now(ctx))
}

/// `DMAEFillMem(dst, value, words)`: writes `value` to `words` 32-bit words.
#[hle_export(lib = "dmae", name = "DMAEFillMem")]
fn dmae_fill_mem(
    ctx: &mut HleContext<'_>,
    dst: u32,
    value: u32,
    words: u32,
) -> Result<i64, HleError> {
    let size = words.saturating_mul(4);
    if size != 0 {
        let data: Vec<u8> = (0..words).flat_map(|_| value.to_be_bytes()).collect();
        ctx.memory.write_bytes(dst, &data)?;
        ctx.gpu.note_cpu_write(dst, size);
    }
    Ok(now(ctx))
}

/// `DMAEWaitDone(timestamp)`: every submission is done.
#[hle_export(lib = "dmae", name = "DMAEWaitDone")]
fn dmae_wait_done(_ctx: &mut HleContext<'_>, _timestamp: u64) -> u32 {
    1
}

#[hle_export(lib = "dmae", name = "DMAEGetRetiredTimeStamp")]
fn dmae_get_retired_time_stamp(ctx: &mut HleContext<'_>) -> i64 {
    now(ctx)
}

#[hle_export(lib = "dmae", name = "DMAEGetLastSubmittedTimeStamp")]
fn dmae_get_last_submitted_time_stamp(ctx: &mut HleContext<'_>) -> i64 {
    now(ctx)
}

pub const EXPORTS: &[HleExport] = &[
    DMAE_COPY_MEM,
    DMAE_FILL_MEM,
    DMAE_WAIT_DONE,
    DMAE_GET_RETIRED_TIME_STAMP,
    DMAE_GET_LAST_SUBMITTED_TIME_STAMP,
];

#[cfg(test)]
mod tests {
    use crate::hle::test_env::{SCRATCH, TestEnv};

    /// Intent: a DMA copy lands at once, swapping bytes as asked, and waiting on it
    /// returns done, so titles polling `DMAEWaitDone` move on.
    /// Pass: a plain copy matches the source, a 32-bit swap reverses each word, a fill
    /// writes the word, and `DMAEWaitDone` returns 1.
    #[test]
    fn copies_swaps_fills_and_is_done() {
        let mut env = TestEnv::new();
        env.memory.write_bytes(SCRATCH, &[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        env.call("dmae", "DMAECopyMem", &[SCRATCH + 0x100, SCRATCH, 2, 0]);
        assert_eq!(env.memory.slice(SCRATCH + 0x100, 8).unwrap(), &[1, 2, 3, 4, 5, 6, 7, 8]);
        env.call("dmae", "DMAECopyMem", &[SCRATCH + 0x200, SCRATCH, 2, 2]);
        assert_eq!(env.memory.slice(SCRATCH + 0x200, 8).unwrap(), &[4, 3, 2, 1, 8, 7, 6, 5]);
        env.call("dmae", "DMAEFillMem", &[SCRATCH + 0x300, 0xAABB_CCDD, 2]);
        assert_eq!(env.memory.read_u32(SCRATCH + 0x304).unwrap(), 0xAABB_CCDD);
        assert_eq!(env.call("dmae", "DMAEWaitDone", &[0, 0]), 1);
    }
}
