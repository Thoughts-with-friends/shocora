//! PowerPC ELF relocation semantics.
#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap, clippy::cast_sign_loss)]

use std::collections::HashMap;

use wiiu_memory::Memory;

use crate::LoadError;

pub const R_PPC_NONE: u8 = 0;
pub const R_PPC_ADDR32: u8 = 1;
pub const R_PPC_ADDR24: u8 = 2;
pub const R_PPC_ADDR16: u8 = 3;
pub const R_PPC_ADDR16_LO: u8 = 4;
pub const R_PPC_ADDR16_HI: u8 = 5;
pub const R_PPC_ADDR16_HA: u8 = 6;
pub const R_PPC_REL24: u8 = 10;
pub const R_PPC_REL14: u8 = 11;
pub const R_PPC_REL32: u8 = 26;
/// `tls_index::module`: which module's TLS block the symbol lives in.
pub const R_PPC_DTPMOD32: u8 = 68;
/// `tls_index::offset`: the symbol's offset inside that block.
pub const R_PPC_DTPREL32: u8 = 78;
/// A load or store through `r13` (small data) or `r2` (small data 2): the immediate becomes
/// the offset from that register's base (Cemu: "SDATA reloc"; Paper Mario: Color Splash).
pub const R_PPC_EMB_SDA21: u8 = 109;
pub const R_PPC_GHS_REL16_HA: u8 = 251;
pub const R_PPC_GHS_REL16_HI: u8 = 252;
pub const R_PPC_GHS_REL16_LO: u8 = 253;

/// Size of one long-branch trampoline.
pub const TRAMPOLINE_SIZE: u32 = 16;

const fn lo(value: u32) -> u16 {
    value as u16
}

const fn hi(value: u32) -> u16 {
    (value >> 16) as u16
}

const fn ha(value: u32) -> u16 {
    (value.wrapping_add(0x8000) >> 16) as u16
}

/// Long-branch thunks for `REL24` targets beyond ±32 MiB (e.g. HLE stubs).
#[derive(Debug)]
pub struct Trampolines {
    next: u32,
    end: u32,
    by_target: HashMap<u32, u32>,
}

impl Trampolines {
    pub fn new(start: u32, size: u32) -> Self {
        Self { next: start, end: start + size, by_target: HashMap::new() }
    }

    /// Address of a thunk jumping to `target`, emitting it on first use.
    fn get(&mut self, memory: &mut Memory, target: u32, place: u32) -> Result<u32, LoadError> {
        if let Some(&thunk) = self.by_target.get(&target) {
            return Ok(thunk);
        }
        if self.next + TRAMPOLINE_SIZE > self.end {
            return Err(LoadError::RelocationOutOfRange { address: place, target });
        }
        let thunk = self.next;
        let code = [
            0x3D80_0000 | u32::from(hi(target)), // lis r12, target@h
            0x618C_0000 | u32::from(lo(target)), // ori r12, r12, target@l
            0x7D89_03A6,                         // mtctr r12
            0x4E80_0420,                         // bctr
        ];
        for (n, word) in code.into_iter().enumerate() {
            memory.write_u32(thunk + 4 * n as u32, word)?;
        }
        self.next += TRAMPOLINE_SIZE;
        self.by_target.insert(target, thunk);
        Ok(thunk)
    }
}

const fn fits_rel24(delta: u32) -> bool {
    let delta = delta as i32;
    delta >= -0x0200_0000 && delta < 0x0200_0000
}

/// Applies [`R_PPC_EMB_SDA21`] at `place` with `value = S + A`: the instruction's base
/// register picks the small data area the offset is taken from (`r0` means address zero).
pub fn apply_sda21(
    memory: &mut Memory,
    place: u32,
    value: u32,
    sda_base: u32,
    sda2_base: u32,
) -> Result<(), LoadError> {
    let insn = memory.read_u32(place)?;
    let base = match (insn >> 16) & 0x1F {
        13 => sda_base,
        2 => sda2_base,
        0 => 0,
        _ => return Err(LoadError::UnsupportedRelocation(R_PPC_EMB_SDA21)),
    };
    let offset = value.wrapping_sub(base);
    memory.write_u32(place, (insn & 0xFFFF_0000) | (offset & 0xFFFF))?;
    Ok(())
}

/// Applies relocation `kind` at `place` with `value = S + A`.
pub fn apply(
    memory: &mut Memory,
    kind: u8,
    place: u32,
    value: u32,
    trampolines: &mut Trampolines,
) -> Result<(), LoadError> {
    let relative = value.wrapping_sub(place);
    match kind {
        R_PPC_NONE => {}
        R_PPC_ADDR32 => memory.write_u32(place, value)?,
        R_PPC_ADDR24 => {
            let insn = memory.read_u32(place)?;
            memory.write_u32(place, (insn & !0x03FF_FFFC) | (value & 0x03FF_FFFC))?;
        }
        R_PPC_ADDR16 | R_PPC_ADDR16_LO => memory.write_u16(place, lo(value))?,
        R_PPC_ADDR16_HI => memory.write_u16(place, hi(value))?,
        R_PPC_ADDR16_HA => memory.write_u16(place, ha(value))?,
        R_PPC_REL24 => {
            let delta = if fits_rel24(relative) {
                relative
            } else {
                trampolines.get(memory, value, place)?.wrapping_sub(place)
            };
            let insn = memory.read_u32(place)?;
            memory.write_u32(place, (insn & !0x03FF_FFFC) | (delta & 0x03FF_FFFC))?;
        }
        R_PPC_REL14 => {
            let delta = relative as i32;
            if !(-0x8000..0x8000).contains(&delta) {
                return Err(LoadError::RelocationOutOfRange { address: place, target: value });
            }
            let insn = memory.read_u32(place)?;
            memory.write_u32(place, (insn & !0xFFFC) | (relative & 0xFFFC))?;
        }
        R_PPC_REL32 => memory.write_u32(place, relative)?,
        R_PPC_GHS_REL16_HA => memory.write_u16(place, ha(relative))?,
        R_PPC_GHS_REL16_HI => memory.write_u16(place, hi(relative))?,
        R_PPC_GHS_REL16_LO => memory.write_u16(place, lo(relative))?,
        other => return Err(LoadError::UnsupportedRelocation(other)),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use wiiu_mem_addr::Region;

    use super::*;

    fn memory() -> Memory {
        let mut memory = Memory::new();
        memory.map(Region::new("lo", 0x0100_0000, 0x1000)).unwrap();
        memory
    }

    #[test]
    fn half_relocations() {
        let mut m = memory();
        let mut t = Trampolines::new(0x0100_0800, 0x100);
        apply(&mut m, R_PPC_ADDR16_HA, 0x0100_0002, 0x1000_8000, &mut t).unwrap();
        apply(&mut m, R_PPC_ADDR16_LO, 0x0100_0006, 0x1000_8000, &mut t).unwrap();
        assert_eq!(m.read_u16(0x0100_0002).unwrap(), 0x1001);
        assert_eq!(m.read_u16(0x0100_0006).unwrap(), 0x8000);
    }

    #[test]
    fn far_branch_goes_through_trampoline() {
        let mut m = memory();
        let mut t = Trampolines::new(0x0100_0800, 0x100);
        m.write_u32(0x0100_0000, 0x4800_0001).unwrap(); // bl 0
        apply(&mut m, R_PPC_REL24, 0x0100_0000, 0x0E00_0010, &mut t).unwrap();
        assert_eq!(m.read_u32(0x0100_0000).unwrap(), 0x4800_0801);
        assert_eq!(m.read_u32(0x0100_0800).unwrap(), 0x3D80_0E00);
        assert_eq!(m.read_u32(0x0100_0804).unwrap(), 0x618C_0010);
        // Second call to the same target reuses the thunk.
        m.write_u32(0x0100_0004, 0x4800_0001).unwrap();
        apply(&mut m, R_PPC_REL24, 0x0100_0004, 0x0E00_0010, &mut t).unwrap();
        assert_eq!(m.read_u32(0x0100_0004).unwrap(), 0x4800_07FD);
    }
}
