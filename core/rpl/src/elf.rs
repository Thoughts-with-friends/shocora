//! Big-endian ELF32 headers, as used by RPX/RPL files.
use crate::LoadError;

pub const ET_RPL: u16 = 0xFE01;
pub const EM_PPC: u16 = 20;

pub const SHT_NULL: u32 = 0;
pub const SHT_PROGBITS: u32 = 1;
pub const SHT_SYMTAB: u32 = 2;
pub const SHT_STRTAB: u32 = 3;
pub const SHT_RELA: u32 = 4;
pub const SHT_NOBITS: u32 = 8;
pub const SHT_RPL_EXPORTS: u32 = 0x8000_0001;
pub const SHT_RPL_IMPORTS: u32 = 0x8000_0002;
pub const SHT_RPL_CRCS: u32 = 0x8000_0003;
pub const SHT_RPL_FILEINFO: u32 = 0x8000_0004;

pub const SHF_WRITE: u32 = 0x1;
pub const SHF_ALLOC: u32 = 0x2;
pub const SHF_EXECINSTR: u32 = 0x4;
/// Thread-local storage (`.tdata`, `.thrbss`); the RPL toolchain's own bit.
pub const SHF_RPL_TLS: u32 = 0x0400_0000;
/// Section data is zlib-compressed, prefixed by its inflated size.
pub const SHF_RPL_ZLIB: u32 = 0x0800_0000;

pub const SHN_UNDEF: u16 = 0;
pub const SHN_ABS: u16 = 0xFFF1;

pub const ELF_HEADER_SIZE: usize = 52;
pub const SECTION_HEADER_SIZE: usize = 40;
pub const SYMBOL_SIZE: usize = 16;
pub const RELA_SIZE: usize = 12;

/// Bounds-checked big-endian reads from a byte slice.
pub(crate) trait ReadBe {
    fn be_u16(&self, offset: usize) -> Result<u16, LoadError>;
    fn be_u32(&self, offset: usize) -> Result<u32, LoadError>;
    fn range_checked(&self, offset: usize, len: usize) -> Result<&[u8], LoadError>;
}

impl ReadBe for [u8] {
    fn be_u16(&self, offset: usize) -> Result<u16, LoadError> {
        let bytes = self.range_checked(offset, 2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn be_u32(&self, offset: usize) -> Result<u32, LoadError> {
        let bytes = self.range_checked(offset, 4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn range_checked(&self, offset: usize, len: usize) -> Result<&[u8], LoadError> {
        self.get(offset..offset.saturating_add(len))
            .ok_or(LoadError::Truncated { offset, needed: len })
    }
}

/// The fields of the ELF header the loader uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElfHeader {
    pub kind: u16,
    pub machine: u16,
    pub entry: u32,
    pub section_offset: u32,
    pub section_entry_size: u16,
    pub section_count: u16,
    pub section_names_index: u16,
}

impl ElfHeader {
    /// # Errors
    /// Fails on a short file or a header that is not big-endian ELF32.
    pub fn parse(bytes: &[u8]) -> Result<Self, LoadError> {
        let ident = bytes.range_checked(0, 16)?;
        if ident[..4] != [0x7F, b'E', b'L', b'F'] || ident[4] != 1 || ident[5] != 2 {
            return Err(LoadError::BadMagic);
        }
        Ok(Self {
            kind: bytes.be_u16(16)?,
            machine: bytes.be_u16(18)?,
            entry: bytes.be_u32(24)?,
            section_offset: bytes.be_u32(32)?,
            section_entry_size: bytes.be_u16(46)?,
            section_count: bytes.be_u16(48)?,
            section_names_index: bytes.be_u16(50)?,
        })
    }
}

/// One section header.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SectionHeader {
    pub name: u32,
    pub kind: u32,
    pub flags: u32,
    pub address: u32,
    pub offset: u32,
    pub size: u32,
    pub link: u32,
    pub info: u32,
    pub align: u32,
    pub entry_size: u32,
}

impl SectionHeader {
    /// # Errors
    /// Fails if the header runs past the end of the file.
    pub fn parse(bytes: &[u8], offset: usize) -> Result<Self, LoadError> {
        let field = |n: usize| bytes.be_u32(offset + 4 * n);
        Ok(Self {
            name: field(0)?,
            kind: field(1)?,
            flags: field(2)?,
            address: field(3)?,
            offset: field(4)?,
            size: field(5)?,
            link: field(6)?,
            info: field(7)?,
            align: field(8)?,
            entry_size: field(9)?,
        })
    }

    #[inline]
    pub const fn is_alloc(&self) -> bool {
        self.flags & SHF_ALLOC != 0
    }
}

/// One `Elf32_Sym`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Symbol {
    pub name: u32,
    pub value: u32,
    pub size: u32,
    pub info: u8,
    pub section: u16,
}

impl Symbol {
    /// # Errors
    /// Fails if the entry runs past the end of `bytes`.
    pub fn parse(bytes: &[u8], offset: usize) -> Result<Self, LoadError> {
        Ok(Self {
            name: bytes.be_u32(offset)?,
            value: bytes.be_u32(offset + 4)?,
            size: bytes.be_u32(offset + 8)?,
            info: bytes.range_checked(offset + 12, 1)?[0],
            section: bytes.be_u16(offset + 14)?,
        })
    }
}

/// One `Elf32_Rela`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rela {
    pub offset: u32,
    pub symbol: u32,
    pub kind: u8,
    pub addend: i32,
}

impl Rela {
    /// # Errors
    /// Fails if the entry runs past the end of `bytes`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    pub fn parse(bytes: &[u8], offset: usize) -> Result<Self, LoadError> {
        let info = bytes.be_u32(offset + 4)?;
        Ok(Self {
            offset: bytes.be_u32(offset)?,
            symbol: info >> 8,
            kind: info as u8,
            addend: bytes.be_u32(offset + 8)? as i32,
        })
    }
}

/// Reads a NUL-terminated name out of a string table.
pub fn name_at(table: &[u8], offset: u32) -> String {
    let tail = table.get(offset as usize..).unwrap_or_default();
    let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
    String::from_utf8_lossy(&tail[..end]).into_owned()
}

/// Tests against the developer's own title (`shocora.toml`); they pass without one.
#[cfg(test)]
mod rom_tests {
    use wiiu_test_roms::{report, require};

    use super::*;

    /// Intent: the ELF headers of the title's `.rpx` are what the Cafe OS loader expects:
    /// a PowerPC RPL whose section table lies inside the file.
    /// Pass: type `ET_RPL`, machine `EM_PPC`, 40-byte section entries, and every section
    /// header parses with its file range inside the file (except `NOBITS`).
    #[test]
    fn reads_the_rpx_headers() {
        let title = require!(wiiu_test_roms::title(), "no title");
        let bytes = std::fs::read(wiiu_test_roms::rpx_of(&title).unwrap()).unwrap();
        let header = ElfHeader::parse(&bytes).unwrap();
        assert_eq!((header.kind, header.machine), (ET_RPL, EM_PPC));
        assert_eq!(usize::from(header.section_entry_size), SECTION_HEADER_SIZE);
        let mut compressed = 0;
        for index in 0..usize::from(header.section_count) {
            let at = header.section_offset as usize + index * SECTION_HEADER_SIZE;
            let section = SectionHeader::parse(&bytes, at).unwrap();
            if section.kind != SHT_NOBITS && section.kind != SHT_NULL {
                assert!((section.offset + section.size) as usize <= bytes.len());
            }
            compressed += usize::from(section.flags & SHF_RPL_ZLIB != 0);
        }
        report("rpx size", bytes.len());
        report("entry point", format!("{:#010x}", header.entry));
        report("sections", header.section_count);
        report("zlib-compressed sections", compressed);
    }
}
