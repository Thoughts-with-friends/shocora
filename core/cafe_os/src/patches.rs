//! Changes to titles' own code, found by its bytes once the RPX is loaded (as Cemu's
//! `GamePatch.cpp` does).
//!
//! One host thread runs every guest thread here, so a guest thread that polls a flag
//! keeps the core busy until its time slice ends: nothing else runs meanwhile, and a
//! guest that has nothing to do never lets the host sleep. The patches below make such
//! loops nap instead.
//!
//! ```text
//!   loop:  lwz   r7, 0(r6)        <- busy loop: wait for the job queue's lock
//!          ...                      (the instruction before it is replaced by an HLE call
//!          bne   loop               that naps while the queue has nothing to hand out)
//! ```
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ game code in       │   │ patches.rs               │   │ patched            │
//! │ memory             │──►│ ──────────────────────── │──►│ instructions       │
//! │                    │   │ built-in game fixes      │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Small fixes to specific games' own code, found by their bytes after loading, so that they
//! run better under emulation. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.
use wiiu_cpu::encode_hle_call;
use wiiu_hle_macros::hle_export;
use wiiu_memory::{Memory, MemoryError};
use wiiu_peripherals::timebase::FREQUENCY;

use crate::{
    coreinit::thread::{Wait, block},
    hle::{HleContext, HleError, HleExport},
    registry::HleRegistry,
};

/// How long a polling loop with nothing to do naps, in guest time (100 µs).
const NAP_TICKS: u64 = FREQUENCY / 10_000;

/// *Breath of the Wild*'s job workers wait for the queue's lock like this; the
/// instruction before it is `mr r6, r29` (Cemu: `botw_busyLoopSignature`).
const BOTW_BUSY_LOOP: [u8; 40] = [
    0x80, 0xE6, 0x00, 0x00, 0x2C, 0x07, 0x00, 0x01, 0x41, 0x82, 0xFF, 0xF8, 0x7D, 0x00, 0x30, 0x28,
    0x2C, 0x08, 0x00, 0x00, 0x40, 0x82, 0xFF, 0xF8, 0x7C, 0x00, 0x30, 0x6C, 0x39, 0x00, 0x00, 0x01,
    0x7D, 0x00, 0x31, 0x2D, 0x40, 0x82, 0xFF, 0xE8,
];

/// The same loop in later builds, after `mr r12, r29` (Cemu: `botw_busyLoopSignature2`).
const BOTW_BUSY_LOOP_2: [u8; 40] = [
    0x80, 0x0C, 0x00, 0x00, 0x2C, 0x00, 0x00, 0x01, 0x41, 0x82, 0xFF, 0xF8, 0x7C, 0xA0, 0x60, 0x28,
    0x2C, 0x05, 0x00, 0x00, 0x40, 0x82, 0xFF, 0xF8, 0x7C, 0x00, 0x60, 0x6C, 0x38, 0x80, 0x00, 0x01,
    0x7C, 0x80, 0x61, 0x2D, 0x40, 0x82, 0xFF, 0xE8,
];

/// One patch: the bytes that follow the replaced instruction, and its replacement.
struct Patch {
    name: &'static str,
    signature: &'static [u8],
    hle: HleExport,
}

const PATCHES: [Patch; 2] = [
    Patch { name: "BotW busy loop", signature: &BOTW_BUSY_LOOP, hle: BOTW_BUSY_LOOP_NAP },
    Patch { name: "BotW busy loop 2", signature: &BOTW_BUSY_LOOP_2, hle: BOTW_BUSY_LOOP_NAP_2 },
];

pub const EXPORTS: &[HleExport] = &[BOTW_BUSY_LOOP_NAP, BOTW_BUSY_LOOP_NAP_2];

/// Applies every patch whose signature is in the code at `text` (`size` bytes), unless
/// `SHOCORA_NO_PATCHES` is set.
///
/// # Errors
/// Fails if the code cannot be read or written.
pub fn apply(
    memory: &mut Memory,
    registry: &HleRegistry,
    (text, size): (u32, u32),
) -> Result<(), MemoryError> {
    if std::env::var_os("SHOCORA_NO_PATCHES").is_some() {
        return Ok(());
    }
    let mut code = vec![0; size as usize];
    memory.read_bytes(text, &mut code)?;
    for patch in &PATCHES {
        let Some(offset) = find(&code, patch.signature) else { continue };
        let Some(index) = registry.function_index(patch.hle.library, patch.hle.name) else {
            continue;
        };
        #[allow(clippy::cast_possible_truncation)]
        let at = text + offset as u32 - 4;
        memory.write_u32(at, encode_hle_call(index))?;
        log::info!("patched {} at {at:#010x}", patch.name);
    }
    Ok(())
}

/// Offset of `signature` in `code`, on an instruction boundary past the first one.
fn find(code: &[u8], signature: &[u8]) -> Option<usize> {
    code.windows(signature.len())
        .enumerate()
        .skip(4)
        .step_by(4)
        .find(|(_, window)| *window == signature)
        .map(|(offset, _)| offset)
}

/// Does what the replaced `mr rD, r29` did, then naps while the job queue in `r24`
/// has nothing to hand out (Cemu's condition for giving up the time slice).
fn busy_loop_nap(ctx: &mut HleContext<'_>, register: usize) -> Result<(), HleError> {
    ctx.cpu.gpr[register] = ctx.cpu.gpr[29];
    if let Some(Wait::Sleep(_)) = ctx.os.coreinit.threads.take_woke_from() {
        return Ok(());
    }
    let queue = ctx.cpu.gpr[24];
    let pending = ctx.memory.read_u32(queue + 0x7C)?;
    let open = ctx.memory.read_u8(queue + 0x80)?;
    if open != 0 && ctx.cpu.gpr[22] == 0 && pending == 0 {
        let deadline = ctx.os.timebase.ticks_since_boot().saturating_add(NAP_TICKS);
        block(ctx, Wait::Sleep(deadline));
    }
    Ok(())
}

#[hle_export(lib = "shocora", name = "__shocora_BotwBusyLoopNap")]
fn botw_busy_loop_nap(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    busy_loop_nap(ctx, 6)
}

#[hle_export(lib = "shocora", name = "__shocora_BotwBusyLoopNap2")]
fn botw_busy_loop_nap_2(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    busy_loop_nap(ctx, 12)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_are_found_on_instruction_boundaries_only() {
        let mut code = vec![0; 64];
        code[9..9 + 4].copy_from_slice(&BOTW_BUSY_LOOP[..4]);
        assert_eq!(find(&code, &BOTW_BUSY_LOOP[..4]), None);
        code[12..16].copy_from_slice(&BOTW_BUSY_LOOP[..4]);
        assert_eq!(find(&code, &BOTW_BUSY_LOOP[..4]), Some(12));
        // Nothing comes before offset 0, so it cannot be patched.
        assert_eq!(find(&BOTW_BUSY_LOOP, &BOTW_BUSY_LOOP), None);
    }
}
