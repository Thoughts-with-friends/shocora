//! Integer and floating-point loads/stores, reservations and `dcbz`.
#![allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]

use super::{ExecResult, Fault, unimplemented};
use crate::{Bus, CpuState, Fpr, Instr, Opcode, StepEvent};

/// Cache line size zeroed by `dcbz`.
const CACHE_LINE: u32 = 32;

/// Effective address of D-form (`d(rA|0)`).
const fn ea_d(s: &CpuState, i: Instr) -> u32 {
    s.ra_or_zero(i.ra()).wrapping_add(i.simm() as u32)
}

/// Effective address of X-form (`(rA|0) + rB`).
const fn ea_x(s: &CpuState, i: Instr) -> u32 {
    s.ra_or_zero(i.ra()).wrapping_add(s.gpr[i.rb()])
}

/// Addressing mode of a load/store opcode.
#[derive(Clone, Copy)]
enum Mode {
    D,
    DUpdate,
    X,
    XUpdate,
}

impl Mode {
    const fn ea(self, s: &CpuState, i: Instr) -> u32 {
        match self {
            Self::D => ea_d(s, i),
            Self::DUpdate => s.gpr[i.ra()].wrapping_add(i.simm() as u32),
            Self::X => ea_x(s, i),
            Self::XUpdate => s.gpr[i.ra()].wrapping_add(s.gpr[i.rb()]),
        }
    }

    const fn updates(self) -> bool {
        matches!(self, Self::DUpdate | Self::XUpdate)
    }
}

/// What a load/store moves.
#[derive(Clone, Copy)]
enum Access {
    LoadWord,
    LoadByte,
    LoadHalf,
    LoadHalfSigned,
    StoreWord,
    StoreByte,
    StoreHalf,
    LoadSingle,
    LoadDouble,
    StoreSingle,
    StoreDouble,
}

const fn classify(op: Opcode) -> Option<(Access, Mode)> {
    use Access as A;
    use Mode as M;
    use Opcode as O;
    Some(match op {
        O::Lwz => (A::LoadWord, M::D),
        O::Lwzu => (A::LoadWord, M::DUpdate),
        O::Lwzx => (A::LoadWord, M::X),
        O::Lwzux => (A::LoadWord, M::XUpdate),
        O::Lbz => (A::LoadByte, M::D),
        O::Lbzu => (A::LoadByte, M::DUpdate),
        O::Lbzx => (A::LoadByte, M::X),
        O::Lbzux => (A::LoadByte, M::XUpdate),
        O::Lhz => (A::LoadHalf, M::D),
        O::Lhzu => (A::LoadHalf, M::DUpdate),
        O::Lhzx => (A::LoadHalf, M::X),
        O::Lhzux => (A::LoadHalf, M::XUpdate),
        O::Lha => (A::LoadHalfSigned, M::D),
        O::Lhau => (A::LoadHalfSigned, M::DUpdate),
        O::Lhax => (A::LoadHalfSigned, M::X),
        O::Lhaux => (A::LoadHalfSigned, M::XUpdate),
        O::Stw => (A::StoreWord, M::D),
        O::Stwu => (A::StoreWord, M::DUpdate),
        O::Stwx => (A::StoreWord, M::X),
        O::Stwux => (A::StoreWord, M::XUpdate),
        O::Stb => (A::StoreByte, M::D),
        O::Stbu => (A::StoreByte, M::DUpdate),
        O::Stbx => (A::StoreByte, M::X),
        O::Stbux => (A::StoreByte, M::XUpdate),
        O::Sth => (A::StoreHalf, M::D),
        O::Sthu => (A::StoreHalf, M::DUpdate),
        O::Sthx => (A::StoreHalf, M::X),
        O::Sthux => (A::StoreHalf, M::XUpdate),
        O::Lfs => (A::LoadSingle, M::D),
        O::Lfsu => (A::LoadSingle, M::DUpdate),
        O::Lfsx => (A::LoadSingle, M::X),
        O::Lfsux => (A::LoadSingle, M::XUpdate),
        O::Lfd => (A::LoadDouble, M::D),
        O::Lfdu => (A::LoadDouble, M::DUpdate),
        O::Lfdx => (A::LoadDouble, M::X),
        O::Lfdux => (A::LoadDouble, M::XUpdate),
        O::Stfs => (A::StoreSingle, M::D),
        O::Stfsu => (A::StoreSingle, M::DUpdate),
        O::Stfsx => (A::StoreSingle, M::X),
        O::Stfsux => (A::StoreSingle, M::XUpdate),
        O::Stfd => (A::StoreDouble, M::D),
        O::Stfdu => (A::StoreDouble, M::DUpdate),
        O::Stfdx => (A::StoreDouble, M::X),
        O::Stfdux => (A::StoreDouble, M::XUpdate),
        _ => return None,
    })
}

fn transfer<B: Bus>(
    access: Access,
    rd: usize,
    ea: u32,
    s: &mut CpuState,
    bus: &mut B,
) -> Result<(), Fault> {
    use Access as A;
    match access {
        A::LoadWord => s.gpr[rd] = bus.read_u32(ea)?,
        A::LoadByte => s.gpr[rd] = u32::from(bus.read_u8(ea)?),
        A::LoadHalf => s.gpr[rd] = u32::from(bus.read_u16(ea)?),
        A::LoadHalfSigned => s.gpr[rd] = bus.read_u16(ea)? as i16 as i32 as u32,
        A::StoreWord => bus.write_u32(ea, s.gpr[rd])?,
        A::StoreByte => bus.write_u8(ea, s.gpr[rd] as u8)?,
        A::StoreHalf => bus.write_u16(ea, s.gpr[rd] as u16)?,
        // In paired-single mode a single load fills both slots.
        A::LoadSingle => s.fpr[rd] = Fpr::splat(f64::from(f32::from_bits(bus.read_u32(ea)?))),
        A::LoadDouble => s.fpr[rd].ps0 = f64::from_bits(bus.read_u64(ea)?),
        A::StoreSingle => bus.write_u32(ea, (s.fpr[rd].ps0 as f32).to_bits())?,
        A::StoreDouble => bus.write_u64(ea, s.fpr[rd].ps0.to_bits())?,
    }
    Ok(())
}

/// `lswi`/`lswx`/`stswi`/`stswx`: `count` bytes packed big-endian into
/// consecutive registers starting at `first`, wrapping from r31 to r0.
fn string_transfer<B: Bus>(
    s: &mut CpuState,
    bus: &mut B,
    first: usize,
    ea: u32,
    count: u32,
    load: bool,
) -> Result<(), Fault> {
    for n in 0..count {
        let reg = (first + (n / 4) as usize) % 32;
        let shift = 24 - 8 * (n % 4);
        let address = ea.wrapping_add(n);
        if load {
            if n % 4 == 0 {
                s.gpr[reg] = 0;
            }
            s.gpr[reg] |= u32::from(bus.read_u8(address)?) << shift;
        } else {
            bus.write_u8(address, (s.gpr[reg] >> shift) as u8)?;
        }
    }
    Ok(())
}

pub(super) fn execute<B: Bus>(op: Opcode, i: Instr, s: &mut CpuState, bus: &mut B) -> ExecResult {
    use Opcode as O;
    let (rd, ra) = (i.rd(), i.ra());

    if let Some((access, mode)) = classify(op) {
        let ea = mode.ea(s, i);
        transfer(access, rd, ea, s, bus)?;
        if mode.updates() {
            s.gpr[ra] = ea;
        }
        return Ok(StepEvent::Continue);
    }

    match op {
        O::Lmw => {
            let base = ea_d(s, i);
            for (n, reg) in (rd..32).enumerate() {
                s.gpr[reg] = bus.read_u32(base.wrapping_add(4 * n as u32))?;
            }
        }
        O::Stmw => {
            let base = ea_d(s, i);
            for (n, reg) in (rd..32).enumerate() {
                bus.write_u32(base.wrapping_add(4 * n as u32), s.gpr[reg])?;
            }
        }
        O::Lwbrx => s.gpr[rd] = bus.read_u32(ea_x(s, i))?.swap_bytes(),
        O::Lhbrx => s.gpr[rd] = u32::from(bus.read_u16(ea_x(s, i))?.swap_bytes()),
        O::Stwbrx => bus.write_u32(ea_x(s, i), s.gpr[rd].swap_bytes())?,
        O::Sthbrx => bus.write_u16(ea_x(s, i), (s.gpr[rd] as u16).swap_bytes())?,
        O::Lwarx => {
            let ea = ea_x(s, i);
            let value = bus.read_u32(ea)?;
            s.gpr[rd] = value;
            s.reservation = Some((ea, value));
        }
        O::Stwcx => {
            let ea = ea_x(s, i);
            // Like Cemu, a reservation is the value `lwarx` read: a word changed meanwhile, by
            // this core or another, fails the store, or that update would be lost. The compare
            // and the store are one atomic step, as other cores may run at the same time.
            let reserved = s.reservation.take();
            let success = match reserved {
                Some((address, value)) if address == ea => {
                    bus.compare_exchange_u32(ea, value, s.gpr[rd])?
                }
                _ => false,
            };
            s.set_cr_field(0, (u32::from(success) << 1) | u32::from(s.xer_so));
        }
        O::Lswi | O::Lswx | O::Stswi | O::Stswx => {
            let (ea, count) = if matches!(op, O::Lswi | O::Stswi) {
                let nb = i.rb() as u32;
                (s.ra_or_zero(ra), if nb == 0 { 32 } else { nb })
            } else {
                (ea_x(s, i), u32::from(s.xer_byte_count))
            };
            string_transfer(s, bus, rd, ea, count, matches!(op, O::Lswi | O::Lswx))?;
        }
        O::Stfiwx => bus.write_u32(ea_x(s, i), s.fpr[rd].ps0.to_bits() as u32)?,
        O::Dcbz => {
            let line = ea_x(s, i) & !(CACHE_LINE - 1);
            for offset in (0..CACHE_LINE).step_by(8) {
                bus.write_u64(line + offset, 0)?;
            }
        }
        _ => return unimplemented(op),
    }
    Ok(StepEvent::Continue)
}

#[cfg(test)]
mod tests {
    use crate::{
        CpuState,
        interpreter::{asm::*, step, test_bus::TestBus},
    };

    fn run_one(s: &mut CpuState, bus: &mut TestBus, raw: u32) {
        bus.load_program(0x100, &[raw]);
        s.pc = 0x100;
        step(s, bus).unwrap();
    }

    #[test]
    fn store_with_update_and_load_back() {
        let mut bus = TestBus::new();
        let mut s = CpuState::default();
        s.gpr[1] = 0x8000;
        s.gpr[3] = 0xDEAD_BEEF;
        run_one(&mut s, &mut bus, d(37, 3, 1, -16)); // stwu r3, -16(r1)
        assert_eq!(s.gpr[1], 0x7FF0);
        run_one(&mut s, &mut bus, d(40, 4, 1, 2)); // lhz r4, 2(r1)
        assert_eq!(s.gpr[4], 0xBEEF);
        run_one(&mut s, &mut bus, d(42, 4, 1, 0)); // lha r4, 0(r1)
        assert_eq!(s.gpr[4], 0xFFFF_DEAD);
        s.gpr[5] = 0;
        run_one(&mut s, &mut bus, x(31, 4, 1, 5, 534, false)); // lwbrx r4, r1, r5
        assert_eq!(s.gpr[4], 0xEFBE_ADDE);
    }

    #[test]
    fn multiple_word_and_reservation() {
        let mut bus = TestBus::new();
        let mut s = CpuState::default();
        s.gpr[1] = 0x4000;
        s.gpr[30] = 1;
        s.gpr[31] = 2;
        run_one(&mut s, &mut bus, d(47, 30, 1, 8)); // stmw r30, 8(r1)
        s.gpr[30] = 0;
        s.gpr[31] = 0;
        run_one(&mut s, &mut bus, d(46, 30, 1, 8)); // lmw r30, 8(r1)
        assert_eq!((s.gpr[30], s.gpr[31]), (1, 2));

        s.gpr[2] = 0;
        s.gpr[3] = 0x4000;
        run_one(&mut s, &mut bus, x(31, 4, 2, 3, 20, false)); // lwarx r4, 0, r3
        s.gpr[4] = 99;
        run_one(&mut s, &mut bus, x(31, 4, 2, 3, 150, true)); // stwcx. r4, 0, r3
        assert_eq!(s.cr_field(0), 0b0010);
        assert_eq!(bus.mem[0x4003], 99);
        run_one(&mut s, &mut bus, x(31, 4, 2, 3, 150, true)); // reservation gone
        assert_eq!(s.cr_field(0), 0);

        run_one(&mut s, &mut bus, x(31, 4, 2, 3, 20, false)); // lwarx r4, 0, r3
        bus.mem[0x4003] = 7; // another thread writes while this one is switched out
        run_one(&mut s, &mut bus, x(31, 4, 2, 3, 150, true)); // stwcx. r4, 0, r3
        assert_eq!(s.cr_field(0), 0, "the word changed");
        assert_eq!(bus.mem[0x4003], 7);
    }

    #[test]
    fn string_loads_and_stores() {
        let mut bus = TestBus::new();
        let mut s = CpuState::default();
        bus.mem[0x2000..0x2006].copy_from_slice(b"abcdef");
        s.gpr[9] = 0x2000;
        run_one(&mut s, &mut bus, x(31, 31, 9, 6, 597, false)); // lswi r31, r9, 6 (wraps to r0)
        assert_eq!(
            (s.gpr[31], s.gpr[0]),
            (u32::from_be_bytes(*b"abcd"), u32::from_be_bytes([b'e', b'f', 0, 0]))
        );
        s.gpr[9] = 0x3000;
        run_one(&mut s, &mut bus, x(31, 31, 9, 6, 725, false)); // stswi r31, r9, 6
        assert_eq!(&bus.mem[0x3000..0x3006], b"abcdef");
    }

    #[test]
    fn float_loads_and_stores() {
        let mut bus = TestBus::new();
        let mut s = CpuState::default();
        bus.mem[0x3000..0x3004].copy_from_slice(&1.5_f32.to_be_bytes());
        s.gpr[3] = 0x3000;
        run_one(&mut s, &mut bus, d(48, 1, 3, 0)); // lfs f1, 0(r3)
        assert!((s.fpr[1].ps0 - 1.5).abs() < 1e-9 && (s.fpr[1].ps1 - 1.5).abs() < 1e-9);
        run_one(&mut s, &mut bus, d(54, 1, 3, 8)); // stfd f1, 8(r3)
        assert_eq!(&bus.mem[0x3008..0x3010], &1.5_f64.to_be_bytes());
    }
}
