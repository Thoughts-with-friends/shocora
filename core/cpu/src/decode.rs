//! `u32 → Opcode`. Pure, table-driven, no side effects.
//!
//! The record (`.`) and overflow (`o`) variants share one [`Opcode`]; the
//! executor reads those bits from the [`Instr`](crate::Instr) itself.

/// Which interpreter module executes an opcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Integer,
    Branch,
    LoadStore,
    Float,
    Paired,
    System,
    Invalid,
}

macro_rules! opcodes {
    ($($unit:ident { $($name:ident => $mnemonic:literal,)* })*) => {
        /// Every instruction the decoder recognises.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Opcode {
            $($($name,)*)*
        }

        impl Opcode {
            /// Base mnemonic (without `.`/`o` suffixes).
            #[inline]
            pub const fn mnemonic(self) -> &'static str {
                match self { $($(Self::$name => $mnemonic,)*)* }
            }

            /// Interpreter module responsible for this opcode.
            #[inline]
            pub const fn unit(self) -> Unit {
                match self { $($(Self::$name => Unit::$unit,)*)* }
            }
        }
    };
}

opcodes! {
    Integer {
        Addi => "addi", Addis => "addis", Addic => "addic", AddicRc => "addic.",
        Subfic => "subfic", Mulli => "mulli", Cmpi => "cmpwi", Cmpli => "cmplwi",
        Ori => "ori", Oris => "oris", Xori => "xori", Xoris => "xoris",
        AndiRc => "andi.", AndisRc => "andis.",
        Rlwimi => "rlwimi", Rlwinm => "rlwinm", Rlwnm => "rlwnm",
        Add => "add", Addc => "addc", Adde => "adde", Addze => "addze", Addme => "addme",
        Subf => "subf", Subfc => "subfc", Subfe => "subfe", Subfze => "subfze", Subfme => "subfme",
        Neg => "neg", Mullw => "mullw", Mulhw => "mulhw", Mulhwu => "mulhwu",
        Divw => "divw", Divwu => "divwu",
        Cmp => "cmpw", Cmpl => "cmplw",
        And => "and", Andc => "andc", Or => "or", Orc => "orc", Xor => "xor",
        Nor => "nor", Nand => "nand", Eqv => "eqv",
        Slw => "slw", Srw => "srw", Sraw => "sraw", Srawi => "srawi",
        Cntlzw => "cntlzw", Extsb => "extsb", Extsh => "extsh",
    }
    Branch {
        B => "b", Bc => "bc", Bclr => "bclr", Bcctr => "bcctr",
    }
    LoadStore {
        Lwz => "lwz", Lwzu => "lwzu", Lwzx => "lwzx", Lwzux => "lwzux",
        Lbz => "lbz", Lbzu => "lbzu", Lbzx => "lbzx", Lbzux => "lbzux",
        Lhz => "lhz", Lhzu => "lhzu", Lhzx => "lhzx", Lhzux => "lhzux",
        Lha => "lha", Lhau => "lhau", Lhax => "lhax", Lhaux => "lhaux",
        Stw => "stw", Stwu => "stwu", Stwx => "stwx", Stwux => "stwux",
        Stb => "stb", Stbu => "stbu", Stbx => "stbx", Stbux => "stbux",
        Sth => "sth", Sthu => "sthu", Sthx => "sthx", Sthux => "sthux",
        Lmw => "lmw", Stmw => "stmw",
        Lwbrx => "lwbrx", Lhbrx => "lhbrx", Stwbrx => "stwbrx", Sthbrx => "sthbrx",
        Lwarx => "lwarx", Stwcx => "stwcx.",
        Lswi => "lswi", Lswx => "lswx", Stswi => "stswi", Stswx => "stswx",
        Lfs => "lfs", Lfsu => "lfsu", Lfsx => "lfsx", Lfsux => "lfsux",
        Lfd => "lfd", Lfdu => "lfdu", Lfdx => "lfdx", Lfdux => "lfdux",
        Stfs => "stfs", Stfsu => "stfsu", Stfsx => "stfsx", Stfsux => "stfsux",
        Stfd => "stfd", Stfdu => "stfdu", Stfdx => "stfdx", Stfdux => "stfdux",
        Stfiwx => "stfiwx", Dcbz => "dcbz",
    }
    Float {
        Fdivs => "fdivs", Fsubs => "fsubs", Fadds => "fadds", Fres => "fres", Fmuls => "fmuls",
        Fmsubs => "fmsubs", Fmadds => "fmadds", Fnmsubs => "fnmsubs", Fnmadds => "fnmadds",
        Fdiv => "fdiv", Fsub => "fsub", Fadd => "fadd", Fsel => "fsel", Fmul => "fmul",
        Frsqrte => "frsqrte", Fmsub => "fmsub", Fmadd => "fmadd", Fnmsub => "fnmsub", Fnmadd => "fnmadd",
        Fcmpu => "fcmpu", Fcmpo => "fcmpo", Frsp => "frsp", Fctiw => "fctiw", Fctiwz => "fctiwz",
        Fneg => "fneg", Fmr => "fmr", Fnabs => "fnabs", Fabs => "fabs",
        Mffs => "mffs", Mtfsf => "mtfsf", Mtfsfi => "mtfsfi", Mtfsb0 => "mtfsb0", Mtfsb1 => "mtfsb1",
        Mcrfs => "mcrfs",
    }
    Paired {
        PsSum0 => "ps_sum0", PsSum1 => "ps_sum1", PsMuls0 => "ps_muls0", PsMuls1 => "ps_muls1",
        PsMadds0 => "ps_madds0", PsMadds1 => "ps_madds1", PsDiv => "ps_div", PsSub => "ps_sub",
        PsAdd => "ps_add", PsSel => "ps_sel", PsRes => "ps_res", PsMul => "ps_mul",
        PsRsqrte => "ps_rsqrte", PsMsub => "ps_msub", PsMadd => "ps_madd", PsNmsub => "ps_nmsub",
        PsNmadd => "ps_nmadd", PsCmpu0 => "ps_cmpu0", PsCmpo0 => "ps_cmpo0", PsCmpu1 => "ps_cmpu1",
        PsCmpo1 => "ps_cmpo1", PsNeg => "ps_neg", PsMr => "ps_mr", PsNabs => "ps_nabs", PsAbs => "ps_abs",
        PsMerge00 => "ps_merge00", PsMerge01 => "ps_merge01", PsMerge10 => "ps_merge10",
        PsMerge11 => "ps_merge11",
        PsqL => "psq_l", PsqLu => "psq_lu", PsqLx => "psq_lx", PsqLux => "psq_lux",
        PsqSt => "psq_st", PsqStu => "psq_stu", PsqStx => "psq_stx", PsqStux => "psq_stux",
        DcbzL => "dcbz_l",
    }
    System {
        HleCall => "hle_call", Sc => "sc", Rfi => "rfi", Tw => "tw", Twi => "twi",
        Mcrf => "mcrf", Mcrxr => "mcrxr", Mfcr => "mfcr", Mtcrf => "mtcrf",
        Crand => "crand", Crandc => "crandc", Creqv => "creqv", Crnand => "crnand",
        Crnor => "crnor", Cror => "cror", Crorc => "crorc", Crxor => "crxor",
        Mfspr => "mfspr", Mtspr => "mtspr", Mftb => "mftb", Mfmsr => "mfmsr", Mtmsr => "mtmsr",
        Mfsr => "mfsr", Mtsr => "mtsr", Mfsrin => "mfsrin", Mtsrin => "mtsrin",
        Sync => "sync", Isync => "isync", Eieio => "eieio",
        Dcbf => "dcbf", Dcbst => "dcbst", Dcbt => "dcbt", Dcbtst => "dcbtst", Dcbi => "dcbi",
        Icbi => "icbi", Tlbie => "tlbie", Tlbsync => "tlbsync",
    }
    Invalid {
        Invalid => "<invalid>",
    }
}

/// Decodes one instruction word.
pub const fn decode(raw: u32) -> Opcode {
    use Opcode as O;
    let xo10 = (raw >> 1) & 0x3FF;
    match raw >> 26 {
        1 => O::HleCall,
        3 => O::Twi,
        4 => decode_paired(raw),
        7 => O::Mulli,
        8 => O::Subfic,
        10 => O::Cmpli,
        11 => O::Cmpi,
        12 => O::Addic,
        13 => O::AddicRc,
        14 => O::Addi,
        15 => O::Addis,
        16 => O::Bc,
        17 => O::Sc,
        18 => O::B,
        19 => match xo10 {
            0 => O::Mcrf,
            16 => O::Bclr,
            33 => O::Crnor,
            50 => O::Rfi,
            129 => O::Crandc,
            150 => O::Isync,
            193 => O::Crxor,
            225 => O::Crnand,
            257 => O::Crand,
            289 => O::Creqv,
            417 => O::Crorc,
            449 => O::Cror,
            528 => O::Bcctr,
            _ => O::Invalid,
        },
        20 => O::Rlwimi,
        21 => O::Rlwinm,
        23 => O::Rlwnm,
        24 => O::Ori,
        25 => O::Oris,
        26 => O::Xori,
        27 => O::Xoris,
        28 => O::AndiRc,
        29 => O::AndisRc,
        31 => decode_31(raw),
        32 => O::Lwz,
        33 => O::Lwzu,
        34 => O::Lbz,
        35 => O::Lbzu,
        36 => O::Stw,
        37 => O::Stwu,
        38 => O::Stb,
        39 => O::Stbu,
        40 => O::Lhz,
        41 => O::Lhzu,
        42 => O::Lha,
        43 => O::Lhau,
        44 => O::Sth,
        45 => O::Sthu,
        46 => O::Lmw,
        47 => O::Stmw,
        48 => O::Lfs,
        49 => O::Lfsu,
        50 => O::Lfd,
        51 => O::Lfdu,
        52 => O::Stfs,
        53 => O::Stfsu,
        54 => O::Stfd,
        55 => O::Stfdu,
        56 => O::PsqL,
        57 => O::PsqLu,
        59 => match (raw >> 1) & 0x1F {
            18 => O::Fdivs,
            20 => O::Fsubs,
            21 => O::Fadds,
            24 => O::Fres,
            25 => O::Fmuls,
            28 => O::Fmsubs,
            29 => O::Fmadds,
            30 => O::Fnmsubs,
            31 => O::Fnmadds,
            _ => O::Invalid,
        },
        60 => O::PsqSt,
        61 => O::PsqStu,
        63 => decode_63(raw),
        _ => O::Invalid,
    }
}

const fn decode_31(raw: u32) -> Opcode {
    use Opcode as O;
    // XO-form arithmetic: the OE bit sits inside the 10-bit field, so match on 9 bits first.
    let arithmetic = match (raw >> 1) & 0x1FF {
        8 => Some(O::Subfc),
        10 => Some(O::Addc),
        11 => Some(O::Mulhwu),
        40 => Some(O::Subf),
        75 => Some(O::Mulhw),
        104 => Some(O::Neg),
        136 => Some(O::Subfe),
        138 => Some(O::Adde),
        200 => Some(O::Subfze),
        202 => Some(O::Addze),
        232 => Some(O::Subfme),
        234 => Some(O::Addme),
        235 => Some(O::Mullw),
        266 => Some(O::Add),
        459 => Some(O::Divwu),
        491 => Some(O::Divw),
        _ => None,
    };
    if let Some(op) = arithmetic {
        return op;
    }
    match (raw >> 1) & 0x3FF {
        0 => O::Cmp,
        4 => O::Tw,
        19 => O::Mfcr,
        20 => O::Lwarx,
        23 => O::Lwzx,
        24 => O::Slw,
        26 => O::Cntlzw,
        28 => O::And,
        32 => O::Cmpl,
        54 => O::Dcbst,
        55 => O::Lwzux,
        60 => O::Andc,
        83 => O::Mfmsr,
        86 => O::Dcbf,
        87 => O::Lbzx,
        119 => O::Lbzux,
        124 => O::Nor,
        144 => O::Mtcrf,
        146 => O::Mtmsr,
        150 => O::Stwcx,
        151 => O::Stwx,
        183 => O::Stwux,
        210 => O::Mtsr,
        215 => O::Stbx,
        242 => O::Mtsrin,
        246 => O::Dcbtst,
        247 => O::Stbux,
        278 => O::Dcbt,
        279 => O::Lhzx,
        284 => O::Eqv,
        306 => O::Tlbie,
        311 => O::Lhzux,
        316 => O::Xor,
        339 => O::Mfspr,
        343 => O::Lhax,
        371 => O::Mftb,
        375 => O::Lhaux,
        407 => O::Sthx,
        412 => O::Orc,
        439 => O::Sthux,
        444 => O::Or,
        467 => O::Mtspr,
        470 => O::Dcbi,
        476 => O::Nand,
        512 => O::Mcrxr,
        533 => O::Lswx,
        534 => O::Lwbrx,
        535 => O::Lfsx,
        536 => O::Srw,
        566 => O::Tlbsync,
        567 => O::Lfsux,
        595 => O::Mfsr,
        597 => O::Lswi,
        598 => O::Sync,
        599 => O::Lfdx,
        631 => O::Lfdux,
        659 => O::Mfsrin,
        661 => O::Stswx,
        662 => O::Stwbrx,
        663 => O::Stfsx,
        695 => O::Stfsux,
        725 => O::Stswi,
        727 => O::Stfdx,
        759 => O::Stfdux,
        790 => O::Lhbrx,
        792 => O::Sraw,
        824 => O::Srawi,
        854 => O::Eieio,
        918 => O::Sthbrx,
        922 => O::Extsh,
        954 => O::Extsb,
        982 => O::Icbi,
        983 => O::Stfiwx,
        1014 => O::Dcbz,
        _ => O::Invalid,
    }
}

const fn decode_63(raw: u32) -> Opcode {
    use Opcode as O;
    // A-form ops all have xo5 >= 16; X-form ops all have low five bits < 16.
    match (raw >> 1) & 0x1F {
        18 => return O::Fdiv,
        20 => return O::Fsub,
        21 => return O::Fadd,
        23 => return O::Fsel,
        25 => return O::Fmul,
        26 => return O::Frsqrte,
        28 => return O::Fmsub,
        29 => return O::Fmadd,
        30 => return O::Fnmsub,
        31 => return O::Fnmadd,
        _ => {}
    }
    match (raw >> 1) & 0x3FF {
        0 => O::Fcmpu,
        12 => O::Frsp,
        14 => O::Fctiw,
        15 => O::Fctiwz,
        32 => O::Fcmpo,
        38 => O::Mtfsb1,
        40 => O::Fneg,
        64 => O::Mcrfs,
        70 => O::Mtfsb0,
        72 => O::Fmr,
        134 => O::Mtfsfi,
        136 => O::Fnabs,
        264 => O::Fabs,
        583 => O::Mffs,
        711 => O::Mtfsf,
        _ => O::Invalid,
    }
}

const fn decode_paired(raw: u32) -> Opcode {
    use Opcode as O;
    // The low five bits of the A-form, 6-bit and 10-bit groups never collide.
    match (raw >> 1) & 0x1F {
        10 => return O::PsSum0,
        11 => return O::PsSum1,
        12 => return O::PsMuls0,
        13 => return O::PsMuls1,
        14 => return O::PsMadds0,
        15 => return O::PsMadds1,
        18 => return O::PsDiv,
        20 => return O::PsSub,
        21 => return O::PsAdd,
        23 => return O::PsSel,
        24 => return O::PsRes,
        25 => return O::PsMul,
        26 => return O::PsRsqrte,
        28 => return O::PsMsub,
        29 => return O::PsMadd,
        30 => return O::PsNmsub,
        31 => return O::PsNmadd,
        _ => {}
    }
    match (raw >> 1) & 0x3F {
        6 => return O::PsqLx,
        7 => return O::PsqStx,
        38 => return O::PsqLux,
        39 => return O::PsqStux,
        _ => {}
    }
    match (raw >> 1) & 0x3FF {
        0 => O::PsCmpu0,
        32 => O::PsCmpo0,
        40 => O::PsNeg,
        64 => O::PsCmpu1,
        72 => O::PsMr,
        96 => O::PsCmpo1,
        136 => O::PsNabs,
        264 => O::PsAbs,
        528 => O::PsMerge00,
        560 => O::PsMerge01,
        592 => O::PsMerge10,
        624 => O::PsMerge11,
        1014 => O::DcbzL,
        _ => O::Invalid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_common_instructions() {
        let cases = [
            (0x3860_0005, Opcode::Addi),   // li r3, 5
            (0x7C63_2214, Opcode::Add),    // add r3, r3, r4
            (0x7C63_2615, Opcode::Add),    // addo. r3, r3, r4
            (0x7C08_02A6, Opcode::Mfspr),  // mflr r0
            (0x4E80_0020, Opcode::Bclr),   // blr
            (0x4E80_0420, Opcode::Bcctr),  // bctr
            (0x9421_FFF0, Opcode::Stwu),   // stwu r1, -16(r1)
            (0x5463_103A, Opcode::Rlwinm), // slwi r3, r3, 2
            (0x7C00_04AC, Opcode::Sync),
            (0xFC20_0890, Opcode::Fmr),   // fmr f1, f1
            (0xEC21_102A, Opcode::Fadds), // fadds f1, f1, f2
            (0x1021_102A, Opcode::PsAdd), // ps_add f1, f1, f2
            (0x1021_14A0, Opcode::PsMerge10),
            (0xE021_0000, Opcode::PsqL),
            (0x0400_0007, Opcode::HleCall),
            (0x0000_0000, Opcode::Invalid),
        ];
        for (raw, expected) in cases {
            assert_eq!(decode(raw), expected, "{raw:#010x}");
        }
    }
}

/// Tests against the developer's own title (`shocora.toml`); they pass without one.
#[cfg(test)]
mod rom_tests {
    use std::collections::HashMap;

    use wiiu_rpl::{RplFile, elf::SHF_EXECINSTR};
    use wiiu_test_roms::{report, require};

    use super::{Opcode, decode};

    /// Intent: the decoder knows every instruction the title's code uses, so neither the
    /// interpreter nor the JIT meets an opcode it cannot run.
    /// Pass: under 0.1 % of the words in executable sections decode as `<invalid>` (the
    /// rest are data islands such as jump tables and padding).
    #[test]
    fn decodes_the_title_code() {
        let title = require!(wiiu_test_roms::title(), "no title");
        let file = RplFile::parse(&std::fs::read(wiiu_test_roms::rpx_of(&title).unwrap()).unwrap())
            .unwrap();
        let mut counts: HashMap<Opcode, usize> = HashMap::new();
        let mut words = 0;
        for section in file.sections.iter().filter(|s| s.header.flags & SHF_EXECINSTR != 0) {
            for word in section.data.as_chunks::<4>().0 {
                let raw = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
                *counts.entry(decode(raw)).or_default() += 1;
                words += 1;
            }
        }
        let invalid = counts.get(&Opcode::Invalid).copied().unwrap_or(0);
        let mut common: Vec<_> = counts.iter().collect();
        common.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
        let common: Vec<String> =
            common.iter().take(8).map(|(op, n)| format!("{} {n}", op.mnemonic())).collect();
        report("instruction words", words);
        report("distinct opcodes", counts.len());
        report("invalid words", invalid);
        report("most common", common.join(", "));
        assert!(invalid * 1000 < words, "{invalid} of {words} words are invalid");
    }
}
