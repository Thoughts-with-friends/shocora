//! HLE functions that touch neither the OS nor other threads.
//!
//! With the cores side by side, every other HLE call runs under the machine lock, one core at
//! a time. These run on the calling core alone, without it, like the guest code they replace:
//! copies and fills, cache maintenance, the time, the core's number. Each does exactly what
//! its locked version does.
#![allow(clippy::cast_possible_truncation)]

use wiiu_cpu::CpuState;
use wiiu_memory::{Memory, MemoryError};
use wiiu_peripherals::TimeBase;

use crate::{coreinit::cache::CACHE_LINE, registry::HleRegistry};

/// One of the functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FastCall {
    /// `memcpy`, `memmove`, `OSBlockMove`: `(dst, src, size)`, returns `dst`.
    Copy,
    /// `memset`, `OSBlockSet`: `(dst, value, size)`, returns `dst`.
    Fill,
    /// `DCFlushRange` and friends: `(address, size)` reaches the GPU.
    Flush,
    /// `DCZeroRange`: zeroes whole cache lines.
    ZeroLines,
    /// Barriers, `DCInvalidateRange`, `DCTouchRange`: nothing to do.
    Nothing,
    CoreId,
    /// `OSGetTime`.
    Time,
    /// `OSGetSystemTime`.
    SystemTime,
    /// `OSGetTick`.
    Tick,
    /// `OSGetSystemTick`.
    SystemTick,
}

impl FastCall {
    /// Whether the GPU must hear of what the call writes.
    #[inline]
    pub const fn writes(self) -> bool {
        matches!(self, Self::Copy | Self::Fill | Self::Flush | Self::ZeroLines)
    }
}

const FUNCTIONS: [(&str, FastCall); 19] = [
    ("memcpy", FastCall::Copy),
    ("memmove", FastCall::Copy),
    ("OSBlockMove", FastCall::Copy),
    ("memset", FastCall::Fill),
    ("OSBlockSet", FastCall::Fill),
    ("DCFlushRange", FastCall::Flush),
    ("DCFlushRangeNoSync", FastCall::Flush),
    ("DCStoreRange", FastCall::Flush),
    ("DCStoreRangeNoSync", FastCall::Flush),
    ("DCZeroRange", FastCall::ZeroLines),
    ("DCInvalidateRange", FastCall::Nothing),
    ("DCTouchRange", FastCall::Nothing),
    ("OSMemoryBarrier", FastCall::Nothing),
    ("OSCoherencyBarrier", FastCall::Nothing),
    ("OSGetCoreId", FastCall::CoreId),
    ("OSGetTime", FastCall::Time),
    ("OSGetSystemTime", FastCall::SystemTime),
    ("OSGetTick", FastCall::Tick),
    ("OSGetSystemTick", FastCall::SystemTick),
];

/// Which HLE indices are such functions.
#[derive(Debug, Default, Clone)]
pub struct FastCalls {
    by_index: Vec<Option<FastCall>>,
}

impl FastCalls {
    pub fn new(registry: &HleRegistry) -> Self {
        let mut by_index = Vec::new();
        for (name, call) in FUNCTIONS {
            let Some(index) = registry.function_index("coreinit", name) else { continue };
            let index = index as usize;
            if by_index.len() <= index {
                by_index.resize(index + 1, None);
            }
            by_index[index] = Some(call);
        }
        Self { by_index }
    }

    #[inline]
    pub fn get(&self, index: u32) -> Option<FastCall> {
        self.by_index.get(index as usize).copied().flatten()
    }
}

/// Runs `call` for the core in `cpu`; `written` hears of what reached memory the GPU reads.
///
/// # Errors
/// Fails like the locked version when the guest passes memory it does not have.
pub fn run(
    call: FastCall,
    cpu: &mut CpuState,
    memory: &mut Memory,
    timebase: &TimeBase,
    written: &mut dyn FnMut(u32, u32),
) -> Result<(), MemoryError> {
    let [a, b, c] = [cpu.gpr[3], cpu.gpr[4], cpu.gpr[5]];
    match call {
        FastCall::Copy => {
            memory.copy_within(a, b, c)?;
            written(a, c);
        }
        FastCall::Fill => {
            memory.fill(a, c, b as u8)?;
            written(a, c);
        }
        FastCall::Flush => written(a, b),
        FastCall::ZeroLines => {
            let start = a & !(CACHE_LINE - 1);
            let end = a.saturating_add(b).next_multiple_of(CACHE_LINE);
            memory.fill(start, end - start, 0)?;
            written(start, end - start);
        }
        FastCall::Nothing => {}
        FastCall::CoreId => cpu.gpr[3] = cpu.core_id(),
        FastCall::Time => set_u64(cpu, timebase.ticks_since_epoch()),
        FastCall::SystemTime => set_u64(cpu, timebase.ticks_since_boot()),
        FastCall::Tick => cpu.gpr[3] = timebase.ticks_since_epoch() as u32,
        FastCall::SystemTick => cpu.gpr[3] = timebase.ticks_since_boot() as u32,
    }
    Ok(())
}

/// A 64-bit return value, as the locked versions return it.
const fn set_u64(cpu: &mut CpuState, value: u64) {
    cpu.gpr[3] = (value >> 32) as u32;
    cpu.gpr[4] = value as u32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::test_env::{SCRATCH, TestEnv};

    /// The same call, locked and not, leaves the same registers and memory.
    fn agree(name: &str, args: [u32; 3]) {
        let setup = || {
            let mut env = TestEnv::new();
            for i in 0..0x80 {
                env.memory.write_u8(SCRATCH + i, i as u8).unwrap();
            }
            env
        };
        let (mut fast, mut locked) = (setup(), setup());
        let index = fast.os.registry.function_index("coreinit", name).unwrap();
        let call = FastCalls::new(&fast.os.registry).get(index).unwrap();
        let mut cpu = fast.cpu.clone();
        cpu.gpr[3..6].copy_from_slice(&args);
        run(call, &mut cpu, &mut fast.memory, &fast.os.timebase, &mut |_, _| {}).unwrap();
        let returned = locked.call("coreinit", name, &args);
        let bytes = |env: &TestEnv| -> Vec<u8> {
            (0..0x80).map(|i| env.memory.read_u8(SCRATCH + 0x100 + i).unwrap()).collect()
        };
        assert_eq!(cpu.gpr[3], returned, "{name}");
        assert_eq!(bytes(&fast), bytes(&locked), "{name}");
    }

    #[test]
    fn they_do_what_the_locked_versions_do() {
        agree("memcpy", [SCRATCH + 0x100, SCRATCH, 0x40]);
        agree("OSBlockMove", [SCRATCH + 0x100, SCRATCH + 3, 0x21]);
        agree("memset", [SCRATCH + 0x100, 0xAB, 0x30]);
        agree("OSBlockSet", [SCRATCH + 0x110, 0x1CD, 0x10]);
        agree("DCZeroRange", [SCRATCH + 0x104, 0x30, 0]);
        agree("OSGetCoreId", [0; 3]);
    }
}
