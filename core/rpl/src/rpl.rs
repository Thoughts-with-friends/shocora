//! RPL-specific structure on top of the ELF container.
use std::io::Read;

use crate::{
    LoadError,
    elf::{
        ET_RPL, ElfHeader, ReadBe, SECTION_HEADER_SIZE, SHF_RPL_ZLIB, SHT_NOBITS, SHT_RPL_EXPORTS,
        SHT_RPL_FILEINFO, SHT_RPL_IMPORTS, SHT_SYMTAB, SYMBOL_SIZE, SectionHeader, Symbol, name_at,
    },
};

/// File info flag: the module is the main executable (RPX).
pub const FILE_INFO_FLAG_RPX: u32 = 0x2;

/// `SHT_RPL_FILEINFO` contents (the fields the loader uses).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileInfo {
    pub version: u32,
    pub text_size: u32,
    pub text_align: u32,
    pub data_size: u32,
    pub data_align: u32,
    pub load_size: u32,
    pub load_align: u32,
    pub temp_size: u32,
    pub tramp_adjust: u32,
    pub sda_base: u32,
    pub sda2_base: u32,
    pub stack_size: u32,
    pub strings_offset: u32,
    pub flags: u32,
    pub heap_size: u32,
}

impl FileInfo {
    /// # Errors
    /// Fails if the section is shorter than the fixed fields.
    pub fn parse(bytes: &[u8]) -> Result<Self, LoadError> {
        let field = |n: usize| bytes.be_u32(4 * n);
        Ok(Self {
            version: field(0)?,
            text_size: field(1)?,
            text_align: field(2)?,
            data_size: field(3)?,
            data_align: field(4)?,
            load_size: field(5)?,
            load_align: field(6)?,
            temp_size: field(7)?,
            tramp_adjust: field(8)?,
            sda_base: field(9)?,
            sda2_base: field(10)?,
            stack_size: field(11)?,
            strings_offset: field(12)?,
            flags: field(13)?,
            heap_size: field(14)?,
        })
    }

    #[inline]
    pub const fn is_rpx(&self) -> bool {
        self.flags & FILE_INFO_FLAG_RPX != 0
    }
}

/// A section with its contents inflated.
#[derive(Debug, Clone)]
pub struct Section {
    pub index: usize,
    pub name: String,
    pub header: SectionHeader,
    /// Inflated contents (empty for `SHT_NOBITS`).
    pub data: Vec<u8>,
}

impl Section {
    /// In-memory size: inflated data length, or the header size for `.bss`.
    #[allow(clippy::cast_possible_truncation)]
    #[inline]
    pub fn memory_size(&self) -> u32 {
        if self.header.kind == SHT_NOBITS { self.header.size } else { self.data.len() as u32 }
    }

    /// Library name if this is an import section (`.fimport_coreinit` → `coreinit`).
    pub fn import_library(&self) -> Option<&str> {
        if self.header.kind != SHT_RPL_IMPORTS {
            return None;
        }
        self.name.strip_prefix(".fimport_").or_else(|| self.name.strip_prefix(".dimport_"))
    }

    #[inline]
    pub fn is_function_import(&self) -> bool {
        self.name.starts_with(".fimport_")
    }
}

/// One entry of an export table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub name: String,
    /// Address in the module's original (link-time) address space.
    pub address: u32,
    pub is_data: bool,
}

/// A parsed RPL/RPX.
#[derive(Debug, Clone)]
pub struct RplFile {
    pub header: ElfHeader,
    pub sections: Vec<Section>,
    pub file_info: FileInfo,
}

impl RplFile {
    /// Parses the container and inflates every compressed section.
    ///
    /// # Errors
    /// Fails on malformed headers, truncated data or bad zlib streams.
    pub fn parse(bytes: &[u8]) -> Result<Self, LoadError> {
        let header = ElfHeader::parse(bytes)?;
        if header.kind != ET_RPL {
            return Err(LoadError::NotRpl(header.kind));
        }
        let entry_size = usize::from(header.section_entry_size).max(SECTION_HEADER_SIZE);
        let headers = (0..usize::from(header.section_count))
            .map(|i| SectionHeader::parse(bytes, header.section_offset as usize + i * entry_size))
            .collect::<Result<Vec<_>, _>>()?;

        let mut sections = headers
            .iter()
            .enumerate()
            .map(|(index, h)| {
                Ok(Section {
                    index,
                    name: String::new(),
                    header: *h,
                    data: section_data(bytes, index, h)?,
                })
            })
            .collect::<Result<Vec<_>, LoadError>>()?;

        if let Some(names) =
            sections.get(usize::from(header.section_names_index)).map(|s| s.data.clone())
        {
            for section in &mut sections {
                section.name = name_at(&names, section.header.name);
            }
        }

        let file_info = sections
            .iter()
            .find(|s| s.header.kind == SHT_RPL_FILEINFO)
            .ok_or(LoadError::MissingFileInfo)
            .and_then(|s| FileInfo::parse(&s.data))?;

        Ok(Self { header, sections, file_info })
    }

    /// Every entry of the `.fexports` and `.dexports` tables.
    ///
    /// # Errors
    /// Fails if a table is truncated.
    pub fn exports(&self) -> Result<Vec<Export>, LoadError> {
        let mut exports = Vec::new();
        for section in self.sections.iter().filter(|s| s.header.kind == SHT_RPL_EXPORTS) {
            let is_data = section.name.starts_with(".dexports");
            let count = section.data.be_u32(0)?;
            for n in 0..count as usize {
                let address = section.data.be_u32(8 + n * 8)?;
                let name_offset = section.data.be_u32(12 + n * 8)? & 0x7FFF_FFFF;
                exports.push(Export {
                    name: name_at(&section.data, name_offset),
                    address,
                    is_data,
                });
            }
        }
        Ok(exports)
    }

    /// Every imported symbol as `(library, symbol, is_function)`.
    ///
    /// # Errors
    /// Fails if a symbol table is truncated.
    pub fn imported_symbols(&self) -> Result<Vec<(String, String, bool)>, LoadError> {
        let mut imports = Vec::new();
        for table in self.sections.iter().filter(|s| s.header.kind == SHT_SYMTAB) {
            let names = self.sections.get(table.header.link as usize).map_or(&[][..], |s| &s.data);
            for entry in 0..table.data.len() / SYMBOL_SIZE {
                let symbol = Symbol::parse(&table.data, entry * SYMBOL_SIZE)?;
                let Some(section) = self.sections.get(usize::from(symbol.section)) else {
                    continue;
                };
                let Some(library) = section.import_library() else { continue };
                imports.push((
                    library.to_owned(),
                    name_at(names, symbol.name),
                    section.is_function_import(),
                ));
            }
        }
        imports.sort();
        imports.dedup();
        Ok(imports)
    }

    /// Every symbol defined by the module's code as `(link address, name)`.
    ///
    /// # Errors
    /// Fails if a symbol table is truncated.
    pub fn code_symbols(&self) -> Result<Vec<(u32, String)>, LoadError> {
        let mut symbols = Vec::new();
        for table in self.sections.iter().filter(|s| s.header.kind == SHT_SYMTAB) {
            let names = self.sections.get(table.header.link as usize).map_or(&[][..], |s| &s.data);
            for entry in 0..table.data.len() / SYMBOL_SIZE {
                let symbol = Symbol::parse(&table.data, entry * SYMBOL_SIZE)?;
                let is_code = self
                    .sections
                    .get(usize::from(symbol.section))
                    .is_some_and(|s| s.name == ".text");
                if is_code {
                    symbols.push((symbol.value, name_at(names, symbol.name)));
                }
            }
        }
        symbols.sort();
        Ok(symbols)
    }

    /// Names of the libraries this module imports from.
    pub fn imported_libraries(&self) -> Vec<&str> {
        let mut libraries: Vec<&str> =
            self.sections.iter().filter_map(Section::import_library).collect();
        libraries.sort_unstable();
        libraries.dedup();
        libraries
    }
}

fn section_data(bytes: &[u8], index: usize, header: &SectionHeader) -> Result<Vec<u8>, LoadError> {
    if header.kind == SHT_NOBITS || header.size == 0 {
        return Ok(Vec::new());
    }
    let raw = bytes.range_checked(header.offset as usize, header.size as usize)?;
    if header.flags & SHF_RPL_ZLIB == 0 {
        return Ok(raw.to_vec());
    }
    let inflated_size = raw.be_u32(0)? as usize;
    let mut data = Vec::with_capacity(inflated_size);
    flate2::read::ZlibDecoder::new(&raw[4..])
        .read_to_end(&mut data)
        .map_err(|source| LoadError::Decompress { index, source })?;
    Ok(data)
}

/// Tests against the developer's own title (`shocora.toml`); they pass without one.
#[cfg(test)]
mod rom_tests {
    use wiiu_test_roms::{report, require};

    use super::RplFile;

    /// Intent: parsing the `.rpx` inflates its sections and reads `SHT_RPL_FILEINFO` (the
    /// stack and heap sizes the OS gives the title) and its imports (which system
    /// libraries it links against).
    /// Pass: the file info has a stack size, the text section is not empty, and the title
    /// imports from `coreinit` like every Wii U program.
    #[test]
    fn parses_the_rpx_with_file_info_and_imports() {
        let title = require!(wiiu_test_roms::title(), "no title");
        let file = RplFile::parse(&std::fs::read(wiiu_test_roms::rpx_of(&title).unwrap()).unwrap())
            .unwrap();
        let info = &file.file_info;
        assert!(info.stack_size > 0);
        assert!(file.sections.iter().any(|s| s.name == ".text" && !s.data.is_empty()));
        let libraries = file.imported_libraries();
        assert!(libraries.contains(&"coreinit"));
        let imports = file.imported_symbols().unwrap();
        let functions = imports.iter().filter(|(_, _, function)| *function).count();
        report(
            "text / data / load size",
            format!("{:#x} / {:#x} / {:#x}", info.text_size, info.data_size, info.load_size),
        );
        report("stack size", format!("{:#x}", info.stack_size));
        report("heap size", format!("{:#x}", info.heap_size));
        report("sda base", format!("{:#010x}", info.sda_base));
        report("imported libraries", libraries.join(", "));
        report("imported functions / data", format!("{functions} / {}", imports.len() - functions));
        report("exports", file.exports().unwrap().len());
    }
}
