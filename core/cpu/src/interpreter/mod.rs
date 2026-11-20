//! Reference interpreter.
//!
//! [`step`] fetches, decodes and dispatches one instruction to the module of
//! its [`Unit`]. Each module only implements semantics; fetching, PC
//! bookkeeping and error reporting live here.
mod branch;
mod float;
pub(crate) mod integer;
mod load_store;
mod paired_single;
mod system;

use crate::{
    Bus, BusError, CpuState, Instr, Opcode,
    decode::{Unit, decode},
};

/// What the caller must do after a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepEvent {
    /// Keep executing.
    Continue,
    /// The guest executed an HLE call stub; dispatch function `index`.
    HleCall(u32),
    /// `sc` was executed.
    SystemCall,
    /// A trap condition (`tw`/`twi`) fired.
    Trap,
}

/// An instruction that could not be executed. `pc` is left pointing at it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CpuError {
    #[error("bus error at {address:#010x} (pc {pc:#010x})")]
    Bus { pc: u32, address: u32 },
    #[error("invalid instruction {raw:#010x} at {pc:#010x}")]
    Invalid { pc: u32, raw: u32 },
    #[error("unimplemented instruction {mnemonic} ({raw:#010x}) at {pc:#010x}")]
    Unimplemented { pc: u32, raw: u32, mnemonic: &'static str },
}

/// Failure inside a unit, before the PC is attached.
#[derive(Debug, Clone, Copy)]
enum Fault {
    Bus(u32),
    Unimplemented,
}

impl From<BusError> for Fault {
    fn from(error: BusError) -> Self {
        Self::Bus(error.address)
    }
}

type ExecResult = Result<StepEvent, Fault>;

/// Executes exactly one instruction.
///
/// # Errors
/// On a bus fault or an unknown/unimplemented instruction. The state is left
/// with `pc` at the faulting instruction.
pub fn step<B: Bus>(state: &mut CpuState, bus: &mut B) -> Result<StepEvent, CpuError> {
    let pc = state.pc;
    // TEMP debug: SHOCORA_TRACE_PC="addr,addr" prints registers at those addresses.
    {
        static TRACED: std::sync::OnceLock<Vec<u32>> = std::sync::OnceLock::new();
        let traced = TRACED.get_or_init(|| {
            std::env::var("SHOCORA_TRACE_PC")
                .map(|list| {
                    list.split(',')
                        .filter_map(|a| {
                            u32::from_str_radix(a.trim().trim_start_matches("0x"), 16).ok()
                        })
                        .collect()
                })
                .unwrap_or_default()
        });
        if traced.contains(&pc) {
            let r = &state.gpr;
            let mut chain = String::new();
            let mut frame = bus.read_u32(r[1]).unwrap_or(0);
            for _ in 0..12 {
                if frame == 0 {
                    break;
                }
                let caller = bus.read_u32(frame.wrapping_add(4)).unwrap_or(0);
                chain.push_str(&format!(" < {caller:#010x}"));
                frame = bus.read_u32(frame).unwrap_or(0);
            }
            eprintln!("TRACE chain{chain}");
            eprintln!(
                "TRACE pc {pc:#010x} lr {:#010x} r3 {:#x} r4 {:#x} r5 {:#x} r6 {:#x} r29 {:#x} r30 {:#x} r31 {:#x}",
                state.lr, r[3], r[4], r[5], r[6], r[29], r[30], r[31]
            );
        }
    }
    // TEMP debug: SHOCORA_WATCH="addr" prints every change of the word at addr.
    static WATCH: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    static WATCH_LAST: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let watch = *WATCH.get_or_init(|| {
        std::env::var("SHOCORA_WATCH")
            .ok()
            .and_then(|a| u32::from_str_radix(a.trim().trim_start_matches("0x"), 16).ok())
    });
    if let Some(address) = watch
        && let Ok(now) = bus.read_u32(address)
    {
        let last = WATCH_LAST.swap(now, std::sync::atomic::Ordering::Relaxed);
        if last != now {
            eprintln!(
                "WATCH {address:#010x}: {last:#010x} -> {now:#010x} before pc {pc:#010x} lr {:#010x} (outside or previous instr)",
                state.lr
            );
        }
    }
    let raw = bus.fetch(pc).map_err(|e| CpuError::Bus { pc, address: e.address })?;
    let instr = Instr(raw);
    let op = decode(raw);
    state.pc = pc.wrapping_add(4);

    let result = match op.unit() {
        Unit::Integer => integer::execute(op, instr, state),
        Unit::Branch => branch::execute(op, instr, state, pc),
        Unit::LoadStore => load_store::execute(op, instr, state, bus),
        Unit::Float => float::execute(op, instr, state),
        Unit::Paired => paired_single::execute(op, instr, state, bus),
        Unit::System => system::execute(op, instr, state, bus),
        Unit::Invalid => {
            state.pc = pc;
            return Err(CpuError::Invalid { pc, raw });
        }
    };
    result.map_err(|fault| {
        state.pc = pc;
        match fault {
            Fault::Bus(address) => CpuError::Bus { pc, address },
            Fault::Unimplemented => CpuError::Unimplemented { pc, raw, mnemonic: op.mnemonic() },
        }
    })
}

/// Runs until an event other than [`StepEvent::Continue`] occurs or `budget`
/// instructions have executed. Returns the event and the instruction count.
///
/// # Errors
/// See [`step`].
pub fn run<B: Bus>(
    state: &mut CpuState,
    bus: &mut B,
    budget: u64,
) -> Result<(StepEvent, u64), CpuError> {
    for executed in 1..=budget {
        let event = step(state, bus)?;
        if event != StepEvent::Continue {
            return Ok((event, executed));
        }
    }
    Ok((StepEvent::Continue, budget))
}

/// Convenience for modules: an opcode routed to the wrong unit.
const fn unimplemented(_op: Opcode) -> ExecResult {
    Err(Fault::Unimplemented)
}

#[cfg(test)]
pub(crate) mod test_bus {
    use crate::{Bus, BusError};

    /// 64 KiB of flat big-endian memory at address 0.
    pub struct TestBus {
        pub mem: Vec<u8>,
        pub timebase: u64,
    }

    impl TestBus {
        pub fn new() -> Self {
            Self { mem: vec![0; 0x1_0000], timebase: 0 }
        }

        pub fn load_program(&mut self, address: u32, words: &[u32]) {
            for (i, word) in words.iter().enumerate() {
                let at = address as usize + i * 4;
                self.mem[at..at + 4].copy_from_slice(&word.to_be_bytes());
            }
        }

        fn range(&self, address: u32, len: usize) -> Result<std::ops::Range<usize>, BusError> {
            let start = address as usize;
            (start + len <= self.mem.len())
                .then_some(start..start + len)
                .ok_or(BusError { address })
        }
    }

    macro_rules! access {
        ($($read:ident, $write:ident: $ty:ty;)*) => {$(
            fn $read(&mut self, address: u32) -> Result<$ty, BusError> {
                let range = self.range(address, size_of::<$ty>())?;
                Ok(<$ty>::from_be_bytes(self.mem[range].try_into().map_err(|_| BusError { address })?))
            }
            fn $write(&mut self, address: u32, value: $ty) -> Result<(), BusError> {
                let range = self.range(address, size_of::<$ty>())?;
                self.mem[range].copy_from_slice(&value.to_be_bytes());
                Ok(())
            }
        )*};
    }

    impl Bus for TestBus {
        access! {
            read_u8, write_u8: u8;
            read_u16, write_u16: u16;
            read_u32, write_u32: u32;
            read_u64, write_u64: u64;
        }

        fn timebase(&mut self) -> u64 {
            self.timebase
        }
    }
}

#[cfg(test)]
pub(crate) mod asm {
    //! Tiny encoders for tests.
    #[inline]
    pub const fn d(op: u32, rt: u32, ra: u32, imm: i32) -> u32 {
        #[allow(clippy::cast_sign_loss)]
        let imm = imm as u32 & 0xFFFF;
        (op << 26) | (rt << 21) | (ra << 16) | imm
    }

    #[inline]
    pub const fn x(op: u32, rt: u32, ra: u32, rb: u32, xo: u32, rc: bool) -> u32 {
        (op << 26) | (rt << 21) | (ra << 16) | (rb << 11) | (xo << 1) | rc as u32
    }

    #[inline]
    pub const fn a(op: u32, frt: u32, fra: u32, frb: u32, frc: u32, xo: u32) -> u32 {
        (op << 26) | (frt << 21) | (fra << 16) | (frb << 11) | (frc << 6) | (xo << 1)
    }

    #[inline]
    pub const fn li(rt: u32, imm: i32) -> u32 {
        d(14, rt, 0, imm)
    }
}

#[cfg(test)]
mod tests {
    use super::{asm::*, test_bus::TestBus, *};
    use crate::encode_hle_call;

    #[test]
    fn runs_until_hle_call() {
        let mut bus = TestBus::new();
        bus.load_program(0x100, &[li(3, 40), d(14, 3, 3, 2), encode_hle_call(9), crate::INSTR_BLR]);
        let mut s = CpuState::new(0);
        s.pc = 0x100;
        let (event, executed) = run(&mut s, &mut bus, 100).unwrap();
        assert_eq!(event, StepEvent::HleCall(9));
        assert_eq!(executed, 3);
        assert_eq!(s.gpr[3], 42);
        assert_eq!(s.pc, 0x10C);
    }

    #[test]
    fn invalid_instruction_keeps_pc() {
        let mut bus = TestBus::new();
        let mut s = CpuState::new(0);
        s.pc = 0x200;
        assert_eq!(step(&mut s, &mut bus), Err(CpuError::Invalid { pc: 0x200, raw: 0 }));
        assert_eq!(s.pc, 0x200);
    }
}
