//! Human-readable disassembly for logs and the debugger.
#![allow(clippy::cast_sign_loss)]

use crate::{Instr, Opcode, decode};

/// Disassembles `raw`, located at `pc` (used to resolve relative branches).
pub fn disassemble(pc: u32, raw: u32) -> String {
    use Opcode as O;
    let op = decode(raw);
    let i = Instr(raw);
    let (d, a, b) = (i.rd(), i.ra(), i.rb());
    let dot = if i.rc() { "." } else { "" };
    let m = op.mnemonic();

    match op {
        O::Invalid => format!(".word {raw:#010x}"),
        O::HleCall => format!("hle_call {}", raw & 0x03FF_FFFF),
        O::Addi | O::Addic | O::AddicRc | O::Subfic | O::Mulli => {
            format!("{m} r{d}, r{a}, {}", i.simm())
        }
        O::Addis => format!("{m} r{d}, r{a}, {:#x}", i.uimm()),
        O::Cmpi => format!("{m} cr{}, r{a}, {}", i.crfd(), i.simm()),
        O::Cmpli => format!("{m} cr{}, r{a}, {:#x}", i.crfd(), i.uimm()),
        O::Cmp | O::Cmpl => format!("{m} cr{}, r{a}, r{b}", i.crfd()),
        O::Ori | O::Oris | O::Xori | O::Xoris | O::AndiRc | O::AndisRc => {
            format!("{m} r{a}, r{d}, {:#x}", i.uimm())
        }
        O::Rlwinm | O::Rlwimi => format!("{m}{dot} r{a}, r{d}, {}, {}, {}", i.sh(), i.mb(), i.me()),
        O::Rlwnm => format!("{m}{dot} r{a}, r{d}, r{b}, {}, {}", i.mb(), i.me()),
        O::Srawi => format!("{m}{dot} r{a}, r{d}, {}", i.sh()),
        O::Add
        | O::Addc
        | O::Adde
        | O::Subf
        | O::Subfc
        | O::Subfe
        | O::Mullw
        | O::Mulhw
        | O::Mulhwu
        | O::Divw
        | O::Divwu => {
            let o = if i.oe() { "o" } else { "" };
            format!("{m}{o}{dot} r{d}, r{a}, r{b}")
        }
        O::Addze | O::Addme | O::Subfze | O::Subfme | O::Neg => format!("{m}{dot} r{d}, r{a}"),
        O::And
        | O::Andc
        | O::Or
        | O::Orc
        | O::Xor
        | O::Nor
        | O::Nand
        | O::Eqv
        | O::Slw
        | O::Srw
        | O::Sraw => {
            format!("{m}{dot} r{a}, r{d}, r{b}")
        }
        O::Cntlzw | O::Extsb | O::Extsh => format!("{m}{dot} r{a}, r{d}"),
        O::B => {
            let target = if i.aa() { i.li() as u32 } else { pc.wrapping_add(i.li() as u32) };
            format!(
                "b{}{} {target:#010x}",
                if i.lk() { "l" } else { "" },
                if i.aa() { "a" } else { "" }
            )
        }
        O::Bc => {
            let target = if i.aa() { i.bd() as u32 } else { pc.wrapping_add(i.bd() as u32) };
            format!("bc{} {d}, {a}, {target:#010x}", if i.lk() { "l" } else { "" })
        }
        O::Bclr | O::Bcctr => {
            let base = if op == O::Bclr { "blr" } else { "bctr" };
            if d & 0b10100 == 0b10100 {
                format!("{base}{}", if i.lk() { "l" } else { "" })
            } else {
                format!("{m}{} {d}, {a}", if i.lk() { "l" } else { "" })
            }
        }
        O::Lwz
        | O::Lwzu
        | O::Lbz
        | O::Lbzu
        | O::Lhz
        | O::Lhzu
        | O::Lha
        | O::Lhau
        | O::Stw
        | O::Stwu
        | O::Stb
        | O::Stbu
        | O::Sth
        | O::Sthu
        | O::Lmw
        | O::Stmw => format!("{m} r{d}, {}(r{a})", i.simm()),
        O::Lfs | O::Lfsu | O::Lfd | O::Lfdu | O::Stfs | O::Stfsu | O::Stfd | O::Stfdu => {
            format!("{m} f{d}, {}(r{a})", i.simm())
        }
        O::PsqL | O::PsqLu | O::PsqSt | O::PsqStu => {
            format!("{m} f{d}, {}(r{a}), {}, qr{}", i.psq_d(), u8::from(i.psq_w()), i.psq_i())
        }
        O::PsqLx | O::PsqLux | O::PsqStx | O::PsqStux => {
            format!("{m} f{d}, r{a}, r{b}, {}, qr{}", u8::from(i.psqx_w()), i.psqx_i())
        }
        O::Lfsx
        | O::Lfsux
        | O::Lfdx
        | O::Lfdux
        | O::Stfsx
        | O::Stfsux
        | O::Stfdx
        | O::Stfdux
        | O::Stfiwx => format!("{m} f{d}, r{a}, r{b}"),
        O::Dcbz | O::DcbzL | O::Dcbf | O::Dcbst | O::Dcbt | O::Dcbtst | O::Dcbi | O::Icbi => {
            format!("{m} r{a}, r{b}")
        }
        O::Mfspr => match i.spr() {
            8 => format!("mflr r{d}"),
            9 => format!("mfctr r{d}"),
            spr => format!("mfspr r{d}, {spr}"),
        },
        O::Mtspr => match i.spr() {
            8 => format!("mtlr r{d}"),
            9 => format!("mtctr r{d}"),
            spr => format!("mtspr {spr}, r{d}"),
        },
        O::Mftb => format!("mftb r{d}, {}", i.spr()),
        O::Mfcr | O::Mfmsr | O::Mtmsr => format!("{m} r{d}"),
        O::Mtcrf => format!("{m} {:#x}, r{d}", i.crm()),
        O::Mcrf | O::Mcrfs => format!("{m} cr{}, cr{}", i.crfd(), i.crfs()),
        O::Crand | O::Crandc | O::Creqv | O::Crnand | O::Crnor | O::Cror | O::Crorc | O::Crxor => {
            format!("{m} {d}, {a}, {b}")
        }
        O::Fcmpu | O::Fcmpo | O::PsCmpu0 | O::PsCmpo0 | O::PsCmpu1 | O::PsCmpo1 => {
            format!("{m} cr{}, f{a}, f{b}", i.crfd())
        }
        O::Fmr
        | O::Fneg
        | O::Fabs
        | O::Fnabs
        | O::Frsp
        | O::Fctiw
        | O::Fctiwz
        | O::Fres
        | O::Frsqrte => {
            format!("{m}{dot} f{d}, f{b}")
        }
        O::PsMr | O::PsNeg | O::PsAbs | O::PsNabs | O::PsRes | O::PsRsqrte => {
            format!("{m}{dot} f{d}, f{b}")
        }
        O::Fadd
        | O::Fadds
        | O::Fsub
        | O::Fsubs
        | O::Fdiv
        | O::Fdivs
        | O::PsAdd
        | O::PsSub
        | O::PsDiv => {
            format!("{m}{dot} f{d}, f{a}, f{b}")
        }
        O::PsMerge00 | O::PsMerge01 | O::PsMerge10 | O::PsMerge11 => {
            format!("{m}{dot} f{d}, f{a}, f{b}")
        }
        O::Fmul | O::Fmuls | O::PsMul | O::PsMuls0 | O::PsMuls1 => {
            format!("{m}{dot} f{d}, f{a}, f{}", i.rc_reg())
        }
        O::Fmadd
        | O::Fmadds
        | O::Fmsub
        | O::Fmsubs
        | O::Fnmadd
        | O::Fnmadds
        | O::Fnmsub
        | O::Fnmsubs
        | O::Fsel
        | O::PsMadd
        | O::PsMsub
        | O::PsNmadd
        | O::PsNmsub
        | O::PsSel
        | O::PsSum0
        | O::PsSum1
        | O::PsMadds0
        | O::PsMadds1 => format!("{m}{dot} f{d}, f{a}, f{}, f{b}", i.rc_reg()),
        O::Mffs => format!("{m}{dot} f{d}"),
        O::Mtfsf => format!("{m}{dot} {:#x}, f{b}", i.fm()),
        O::Mtfsfi => format!("{m}{dot} cr{}, {}", i.crfd(), i.fpscr_imm()),
        O::Mtfsb0 | O::Mtfsb1 => format!("{m}{dot} {d}"),
        O::Tw => format!("{m} {d}, r{a}, r{b}"),
        O::Twi => format!("{m} {d}, r{a}, {}", i.simm()),
        O::Mfsr | O::Mtsr => format!("{m} r{d}, {}", i.sr()),
        _ if matches!(op.unit(), crate::decode::Unit::LoadStore) => format!("{m} r{d}, r{a}, r{b}"),
        _ => m.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_common_forms() {
        assert_eq!(disassemble(0, 0x3861_FFF8), "addi r3, r1, -8");
        assert_eq!(disassemble(0, 0x9421_FFF0), "stwu r1, -16(r1)");
        assert_eq!(disassemble(0, 0x4E80_0020), "blr");
        assert_eq!(disassemble(0x100, 0x4800_0011), "bl 0x00000110");
        assert_eq!(disassemble(0, 0x7C08_02A6), "mflr r0");
        assert_eq!(disassemble(0, 0x7C63_2615), "addo. r3, r3, r4");
    }
}

/// Tests against the developer's own title (`shocora.toml`); they pass without one.
#[cfg(test)]
mod rom_tests {
    use wiiu_rpl::RplFile;
    use wiiu_test_roms::{report, require};

    use super::disassemble;

    /// Intent: the disassembler, which the debugger views and crash reports use, reads
    /// the title's entry point (`_start` of the C runtime).
    /// Pass: the first eight instructions at the entry disassemble to text that is not
    /// `<invalid>`.
    #[test]
    fn disassembles_the_entry_point() {
        let title = require!(wiiu_test_roms::title(), "no title");
        let file = RplFile::parse(&std::fs::read(wiiu_test_roms::rpx_of(&title).unwrap()).unwrap())
            .unwrap();
        let entry = file.header.entry;
        let section = file
            .sections
            .iter()
            .find(|s| (s.header.address..s.header.address + s.data.len() as u32).contains(&entry))
            .unwrap();
        let at = (entry - section.header.address) as usize;
        for (i, word) in section.data[at..at + 32].as_chunks::<4>().0.iter().enumerate() {
            let pc = entry + 4 * i as u32;
            let text = disassemble(pc, u32::from_be_bytes([word[0], word[1], word[2], word[3]]));
            assert!(!text.contains("<invalid>"), "{pc:#010x}: {text}");
            report(&format!("{pc:#010x}"), text);
        }
    }
}
