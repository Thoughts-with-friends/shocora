//! Builds small, valid RPX files in memory.
//!
//! Real RPX files need the devkitPPC/wut toolchain; this builder lets tests
//! (here and in `wiiu_emu`) exercise the whole loader → CPU → HLE path with
//! hand-written PowerPC code.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!               ▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ test code          │   │ builder.rs               │   │ small RPX files    │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ RPX builder (tests)      │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Builds tiny valid executables in memory so tests do not need real games. Terms such as HLE,
//! JIT or shader are explained in `docs/guide/wiiu_term.md`.
#![allow(clippy::cast_possible_truncation)]

use std::{collections::BTreeMap, io::Write};

use crate::{
    elf::{
        EM_PPC, ET_RPL, SHF_ALLOC, SHF_EXECINSTR, SHF_RPL_ZLIB, SHF_WRITE, SHT_PROGBITS, SHT_RELA,
        SHT_RPL_FILEINFO, SHT_RPL_IMPORTS, SHT_STRTAB, SHT_SYMTAB,
    },
    relocation::{R_PPC_ADDR16_HA, R_PPC_ADDR16_LO, R_PPC_REL24},
    rpl::FILE_INFO_FLAG_RPX,
};

/// Link-time address of `.text` in generated files.
pub const TEXT_ADDRESS: u32 = 0x0200_0000;
/// Link-time address of `.data` in generated files.
pub const DATA_ADDRESS: u32 = 0x1000_0000;
const IMPORT_ADDRESS: u32 = 0xC000_0000;

enum Target {
    Import { library: String, name: String, data: bool },
    Data,
}

struct Reloc {
    text_offset: u32,
    kind: u8,
    target: Target,
    addend: i32,
}

/// Incrementally describes an RPX.
#[derive(Default)]
pub struct RplBuilder {
    text: Vec<u8>,
    data: Vec<u8>,
    entry_offset: u32,
    relocs: Vec<Reloc>,
    compress_text: bool,
    stack_size: u32,
}

impl RplBuilder {
    #[inline]
    pub fn new() -> Self {
        Self { stack_size: 0x1_0000, ..Self::default() }
    }

    /// Sets `.text` from instruction words.
    #[must_use]
    pub fn text(mut self, words: &[u32]) -> Self {
        self.text = words.iter().flat_map(|w| w.to_be_bytes()).collect();
        self
    }

    #[must_use]
    #[inline]
    pub fn data(mut self, bytes: &[u8]) -> Self {
        self.data = bytes.to_vec();
        self
    }

    /// Entry point as a byte offset into `.text`.
    #[must_use]
    #[inline]
    pub const fn entry(mut self, text_offset: u32) -> Self {
        self.entry_offset = text_offset;
        self
    }

    /// Stores `.text` zlib-compressed, like retail RPX files.
    #[must_use]
    #[inline]
    pub const fn compress_text(mut self) -> Self {
        self.compress_text = true;
        self
    }

    /// Patches the `bl` at `text_offset` to call `library::name`.
    #[must_use]
    pub fn call_import(mut self, text_offset: u32, library: &str, name: &str) -> Self {
        self.relocs.push(Reloc {
            text_offset,
            kind: R_PPC_REL24,
            target: Target::Import { library: library.into(), name: name.into(), data: false },
            addend: 0,
        });
        self
    }

    /// Patches a `lis`/`addi` pair to load the address of data import `library::name`.
    #[must_use]
    pub fn load_data_import(
        mut self,
        lis_offset: u32,
        addi_offset: u32,
        library: &str,
        name: &str,
    ) -> Self {
        for (offset, kind) in [(lis_offset, R_PPC_ADDR16_HA), (addi_offset, R_PPC_ADDR16_LO)] {
            self.relocs.push(Reloc {
                text_offset: offset + 2,
                kind,
                target: Target::Import { library: library.into(), name: name.into(), data: true },
                addend: 0,
            });
        }
        self
    }

    /// Patches a `lis`/`addi` pair to load the address of `.data + data_offset`.
    #[must_use]
    pub fn load_data_address(
        mut self,
        lis_offset: u32,
        addi_offset: u32,
        data_offset: i32,
    ) -> Self {
        for (offset, kind) in [(lis_offset, R_PPC_ADDR16_HA), (addi_offset, R_PPC_ADDR16_LO)] {
            self.relocs.push(Reloc {
                text_offset: offset + 2,
                kind,
                target: Target::Data,
                addend: data_offset,
            });
        }
        self
    }

    /// Serialises the RPX.
    pub fn build(self) -> Vec<u8> {
        // Group imports by library (one import section each) and assign symbols.
        let mut libraries: BTreeMap<(bool, String), Vec<String>> = BTreeMap::new();
        for reloc in &self.relocs {
            if let Target::Import { library, name, data } = &reloc.target {
                let names = libraries.entry((*data, library.clone())).or_default();
                if !names.contains(name) {
                    names.push(name.clone());
                }
            }
        }

        let mut shstrtab = vec![0_u8];
        let mut add_name = |name: &str| {
            let offset = shstrtab.len() as u32;
            shstrtab.extend_from_slice(name.as_bytes());
            shstrtab.push(0);
            offset
        };

        // Section indices: 0 null, 1 .text, 2 .data, 3.. imports, then symtab, strtab, rela, shstrtab, fileinfo.
        let mut sections: Vec<SectionSpec> = vec![SectionSpec::default()];
        let text_data = if self.compress_text { compress(&self.text) } else { self.text.clone() };
        let text_flags =
            SHF_ALLOC | SHF_EXECINSTR | if self.compress_text { SHF_RPL_ZLIB } else { 0 };
        sections.push(SectionSpec::new(
            add_name(".text"),
            SHT_PROGBITS,
            text_flags,
            TEXT_ADDRESS,
            text_data,
            32,
        ));
        sections.push(SectionSpec::new(
            add_name(".data"),
            SHT_PROGBITS,
            SHF_ALLOC | SHF_WRITE,
            DATA_ADDRESS,
            self.data.clone(),
            32,
        ));

        let mut strtab = vec![0_u8];
        let mut symbols: Vec<[u8; 16]> = vec![[0; 16]];
        // .data section symbol
        symbols.push(symbol(0, DATA_ADDRESS, 3, 2));
        let data_symbol = 1_u32;

        let mut import_symbols: BTreeMap<(bool, String, String), u32> = BTreeMap::new();
        let mut import_address = IMPORT_ADDRESS;
        for ((data, library), names) in &libraries {
            let prefix = if *data { ".dimport_" } else { ".fimport_" };
            let section_index = sections.len() as u16;
            let mut contents = vec![0_u8; 8];
            contents.extend_from_slice(library.as_bytes());
            contents.push(0);
            let size = (8 * names.len() as u32).max(contents.len() as u32);
            contents.resize(size as usize, 0);
            sections.push(SectionSpec::new(
                add_name(&format!("{prefix}{library}")),
                SHT_RPL_IMPORTS,
                SHF_ALLOC,
                import_address,
                contents,
                4,
            ));
            for (n, name) in names.iter().enumerate() {
                let name_offset = strtab.len() as u32;
                strtab.extend_from_slice(name.as_bytes());
                strtab.push(0);
                let kind = if *data { 1 } else { 2 };
                symbols.push(symbol(
                    name_offset,
                    import_address + 8 * n as u32,
                    kind,
                    section_index,
                ));
                import_symbols
                    .insert((*data, library.clone(), name.clone()), symbols.len() as u32 - 1);
            }
            import_address += size.next_multiple_of(16);
        }

        let symtab_index = sections.len() as u32;
        sections.push(SectionSpec {
            link: symtab_index + 1,
            entry_size: 16,
            ..SectionSpec::new(add_name(".symtab"), SHT_SYMTAB, 0, 0, symbols.concat(), 4)
        });
        sections.push(SectionSpec::new(add_name(".strtab"), SHT_STRTAB, 0, 0, strtab, 1));

        let mut rela = Vec::new();
        for reloc in &self.relocs {
            let symbol_index = match &reloc.target {
                Target::Data => data_symbol,
                Target::Import { library, name, data } => {
                    import_symbols[&(*data, library.clone(), name.clone())]
                }
            };
            rela.extend_from_slice(&(TEXT_ADDRESS + reloc.text_offset).to_be_bytes());
            rela.extend_from_slice(&((symbol_index << 8) | u32::from(reloc.kind)).to_be_bytes());
            rela.extend_from_slice(&reloc.addend.to_be_bytes());
        }
        sections.push(SectionSpec {
            link: symtab_index,
            info: 1,
            entry_size: 12,
            ..SectionSpec::new(add_name(".rela.text"), SHT_RELA, 0, 0, rela, 4)
        });

        let shstrtab_index = sections.len() as u16;
        let shstrtab_name = add_name(".shstrtab");
        let fileinfo_name = add_name("");
        sections.push(SectionSpec::new(shstrtab_name, SHT_STRTAB, 0, 0, shstrtab.clone(), 1));

        let mut info = vec![0_u8; 0x60];
        let mut put = |index: usize, value: u32| {
            info[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
        };
        put(0, 0xCAFE_0402);
        put(1, self.text.len() as u32);
        put(2, 32);
        put(3, self.data.len() as u32);
        put(4, 32);
        put(9, DATA_ADDRESS + 0x8000);
        put(11, self.stack_size);
        put(13, FILE_INFO_FLAG_RPX);
        sections.push(SectionSpec::new(fileinfo_name, SHT_RPL_FILEINFO, 0, 0, info, 4));

        // Fix the .shstrtab copy now that every name has been added.
        sections[shstrtab_index as usize].data = shstrtab;

        write_elf(&sections, TEXT_ADDRESS + self.entry_offset, shstrtab_index)
    }
}

#[derive(Default, Clone)]
struct SectionSpec {
    name: u32,
    kind: u32,
    flags: u32,
    address: u32,
    data: Vec<u8>,
    align: u32,
    link: u32,
    info: u32,
    entry_size: u32,
}

impl SectionSpec {
    fn new(name: u32, kind: u32, flags: u32, address: u32, data: Vec<u8>, align: u32) -> Self {
        Self { name, kind, flags, address, data, align, ..Self::default() }
    }
}

fn symbol(name: u32, value: u32, kind: u8, section: u16) -> [u8; 16] {
    let mut bytes = [0_u8; 16];
    bytes[0..4].copy_from_slice(&name.to_be_bytes());
    bytes[4..8].copy_from_slice(&value.to_be_bytes());
    bytes[12] = (1 << 4) | kind; // STB_GLOBAL
    bytes[14..16].copy_from_slice(&section.to_be_bytes());
    bytes
}

fn compress(data: &[u8]) -> Vec<u8> {
    let mut out = (data.len() as u32).to_be_bytes().to_vec();
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    // Writing into a Vec cannot fail.
    let _ = encoder.write_all(data);
    out.extend(encoder.finish().unwrap_or_default());
    out
}

fn write_elf(sections: &[SectionSpec], entry: u32, shstrtab_index: u16) -> Vec<u8> {
    let mut file = vec![0_u8; 52];
    let mut offsets = Vec::new();
    for section in sections {
        while !file.len().is_multiple_of(4) {
            file.push(0);
        }
        offsets.push(file.len() as u32);
        file.extend_from_slice(&section.data);
    }
    while !file.len().is_multiple_of(4) {
        file.push(0);
    }
    let section_offset = file.len() as u32;
    for (section, offset) in sections.iter().zip(&offsets) {
        let has_data = !section.data.is_empty();
        for value in [
            section.name,
            section.kind,
            section.flags,
            section.address,
            if has_data { *offset } else { 0 },
            section.data.len() as u32,
            section.link,
            section.info,
            section.align,
            section.entry_size,
        ] {
            file.extend_from_slice(&value.to_be_bytes());
        }
    }

    file[0..16]
        .copy_from_slice(&[0x7F, b'E', b'L', b'F', 1, 2, 1, 0xCA, 0xFE, 0, 0, 0, 0, 0, 0, 0]);
    file[16..18].copy_from_slice(&ET_RPL.to_be_bytes());
    file[18..20].copy_from_slice(&EM_PPC.to_be_bytes());
    file[20..24].copy_from_slice(&1_u32.to_be_bytes());
    file[24..28].copy_from_slice(&entry.to_be_bytes());
    file[32..36].copy_from_slice(&section_offset.to_be_bytes());
    file[40..42].copy_from_slice(&52_u16.to_be_bytes());
    file[46..48].copy_from_slice(&40_u16.to_be_bytes());
    file[48..50].copy_from_slice(&(sections.len() as u16).to_be_bytes());
    file[50..52].copy_from_slice(&shstrtab_index.to_be_bytes());
    file
}

#[cfg(test)]
mod tests {
    use wiiu_mem_addr::Region;
    use wiiu_memory::Memory;

    use super::*;
    use crate::{ImportKind, ImportResolver, LinearAllocator, RplFile, SegmentKind, load};

    struct Stubs(Vec<(String, String, ImportKind)>);

    impl ImportResolver for Stubs {
        fn resolve(&mut self, library: &str, symbol: &str, kind: ImportKind) -> u32 {
            self.0.push((library.into(), symbol.into(), kind));
            0x0E00_0000 + 8 * (self.0.len() as u32 - 1)
        }
    }

    fn setup() -> (Memory, LinearAllocator) {
        let (text, data, load_region) = (
            Region::new("TEXT", 0x0100_0000, 0x10_0000),
            Region::new("MEM2", 0x1000_0000, 0x10_0000),
            Region::new("LOADER", 0xF600_0000, 0x10_0000),
        );
        let mut memory = Memory::new();
        for region in [text, data, load_region, Region::new("HLE", 0x0E00_0000, 0x1000)] {
            memory.map(region).unwrap();
        }
        (memory, LinearAllocator::new(text, data, load_region))
    }

    #[test]
    fn loads_relocates_and_resolves_imports() {
        let bytes = RplBuilder::new()
            .text(&[
                0x3C60_0000, // lis r3, data@ha
                0x3863_0000, // addi r3, r3, data@l
                0x4800_0001, // bl OSReport
                0x4800_0001, // bl OSReport (shares the trampoline)
                0x4E80_0020, // blr
            ])
            .data(b"hello\0")
            .load_data_address(0, 4, 0)
            .call_import(8, "coreinit", "OSReport")
            .call_import(12, "coreinit", "OSReport")
            .compress_text()
            .build();
        let file = RplFile::parse(&bytes).unwrap();
        assert!(file.file_info.is_rpx());
        assert_eq!(file.imported_libraries(), ["coreinit"]);

        let (mut memory, mut allocator) = setup();
        let mut stubs = Stubs(Vec::new());
        let module = load(&file, "test.rpx", &mut memory, &mut allocator, &mut stubs).unwrap();

        let text = module.segments[&SegmentKind::Text].0;
        let data = module.segments[&SegmentKind::Data].0;
        assert_eq!(module.entry, text);
        assert_eq!(module.sda_base, data + 0x8000);
        assert_eq!(memory.read_c_string(data, 16).unwrap(), "hello");
        assert_eq!(memory.read_u16(text + 2).unwrap(), (data >> 16) as u16);
        assert_eq!(memory.read_u16(text + 6).unwrap(), data as u16);
        assert_eq!(stubs.0, [("coreinit".to_owned(), "OSReport".to_owned(), ImportKind::Function)]);

        // Both calls land on the same trampoline, which jumps to the stub.
        let target = |at: u32| {
            let insn = memory.read_u32(at).unwrap();
            at.wrapping_add((((insn & 0x03FF_FFFC) << 6) as i32 >> 6) as u32)
        };
        assert_eq!(target(text + 8), target(text + 12));
        let thunk = target(text + 8);
        assert_eq!(memory.read_u32(thunk).unwrap(), 0x3D80_0E00);
    }
}
