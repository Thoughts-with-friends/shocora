//! Places an [`RplFile`] into guest memory and links it.
use std::collections::HashMap;

use wiiu_memory::Memory;

use crate::{
    LoadError, RplFile,
    elf::{
        RELA_SIZE, Rela, SHF_RPL_TLS, SHN_ABS, SHN_UNDEF, SHT_NOBITS, SHT_RELA, SHT_RPL_CRCS,
        SHT_RPL_FILEINFO, SHT_RPL_IMPORTS, SYMBOL_SIZE, Symbol, name_at,
    },
    relocation::{self, R_PPC_DTPMOD32, R_PPC_DTPREL32, R_PPC_REL24, TRAMPOLINE_SIZE, Trampolines},
    rpl::Section,
};

/// The three segments the Cafe OS loader lays a module out in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SegmentKind {
    /// Executable code.
    Text,
    /// Writable data and `.bss`.
    Data,
    /// Read-only data (`.rodata`, export tables, ...).
    Load,
}

/// Whether an import names a function or a data object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    Function,
    Data,
}

/// Where segments go. Implemented by the OS layer (or [`crate::LinearAllocator`]).
pub trait SegmentAllocator {
    /// # Errors
    /// Fails when the target region is exhausted.
    fn allocate(&mut self, kind: SegmentKind, size: u32, align: u32) -> Result<u32, LoadError>;
}

/// What imported symbols resolve to. Implemented by the OS layer, which
/// returns HLE stub addresses or exports of previously loaded modules.
pub trait ImportResolver {
    fn resolve(&mut self, library: &str, symbol: &str, kind: ImportKind) -> u32;
}

/// A module after loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedModule {
    pub name: String,
    pub is_rpx: bool,
    pub entry: u32,
    /// `r13` for the module's small data area.
    pub sda_base: u32,
    /// `r2` for the module's read-only small data area.
    pub sda2_base: u32,
    pub stack_size: u32,
    /// `(start, size)` of each placed segment.
    pub segments: HashMap<SegmentKind, (u32, u32)>,
    /// Exports, relocated to their final addresses.
    pub exports: HashMap<String, u32>,
    /// The module's thread-local storage, if it has any.
    pub tls: Option<TlsImage>,
}

/// TLS index of the main executable. System RPLs are HLE and have no TLS, so
/// the RPX is the only module that ever needs one.
pub const TLS_MODULE_INDEX: u32 = 1;

/// Initial contents of a module's thread-local storage: every thread gets a
/// copy of `size` bytes from `start` on its first `__tls_get_addr`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct TlsImage {
    pub module_index: u32,
    pub start: u32,
    pub size: u32,
}

/// `STT_TLS`: the symbol's value is an offset into the TLS block.
const STT_TLS: u8 = 6;

/// Link-time extent of one segment and where it was placed.
#[derive(Debug, Clone, Copy)]
struct Placement {
    original_start: u32,
    original_end: u32,
    base: u32,
}

impl Placement {
    fn translate(&self, address: u32) -> u32 {
        address.wrapping_sub(self.original_start).wrapping_add(self.base)
    }
}

/// Link-time start of the data segment of every RPL.
const DATA_LINK_BASE: u32 = 0x1000_0000;
/// Link-time start of the loader segment (exports, symbol tables).
const LOAD_LINK_BASE: u32 = 0xC000_0000;

/// Segments are identified by link-time address range, as the Cafe OS loader
/// does: flags are unreliable (`.fexports` is marked executable but lives in
/// the loader segment).
fn segment_of(section: &Section) -> Option<SegmentKind> {
    let h = &section.header;
    if !h.is_alloc()
        || h.address == 0
        || matches!(h.kind, SHT_RPL_IMPORTS | SHT_RPL_CRCS | SHT_RPL_FILEINFO)
    {
        return None;
    }
    Some(match h.address {
        LOAD_LINK_BASE.. => SegmentKind::Load,
        DATA_LINK_BASE.. => SegmentKind::Data,
        _ => SegmentKind::Text,
    })
}

/// Upper bound of trampolines needed: one per `REL24` against an import.
fn count_import_branches(file: &RplFile) -> Result<u32, LoadError> {
    let mut count = 0;
    for rela in file.sections.iter().filter(|s| s.header.kind == SHT_RELA) {
        let Some(symtab) = file.sections.get(rela.header.link as usize) else { continue };
        for entry in rela.data.as_chunks::<RELA_SIZE>().0 {
            let r = Rela::parse(entry, 0)?;
            if r.kind != R_PPC_REL24 {
                continue;
            }
            let symbol = Symbol::parse(&symtab.data, r.symbol as usize * SYMBOL_SIZE)?;
            let in_import = file
                .sections
                .get(usize::from(symbol.section))
                .is_some_and(|s| s.header.kind == SHT_RPL_IMPORTS);
            count += u32::from(in_import);
        }
    }
    Ok(count)
}

/// Loads `file` as module `name`.
///
/// # Errors
/// Fails if memory cannot be allocated, a symbol is undefined or a
/// relocation is unsupported/out of range.
pub fn load(
    file: &RplFile,
    name: &str,
    memory: &mut Memory,
    allocator: &mut dyn SegmentAllocator,
    resolver: &mut dyn ImportResolver,
) -> Result<LoadedModule, LoadError> {
    let trampoline_space = count_import_branches(file)? * TRAMPOLINE_SIZE;

    // 1. Lay out each segment, keeping link-time relative offsets.
    let mut placements = HashMap::new();
    let mut trampolines = Trampolines::new(0, 0);
    for kind in [SegmentKind::Text, SegmentKind::Data, SegmentKind::Load] {
        let members: Vec<&Section> =
            file.sections.iter().filter(|s| segment_of(s) == Some(kind)).collect();
        let Some(original_start) = members.iter().map(|s| s.header.address).min() else { continue };
        let original_end = members
            .iter()
            .map(|s| s.header.address + s.memory_size())
            .max()
            .unwrap_or(original_start);
        let align = members.iter().map(|s| s.header.align).max().unwrap_or(4);
        let extra = if kind == SegmentKind::Text { trampoline_space } else { 0 };
        let size = original_end - original_start;
        let base = allocator.allocate(kind, size + extra, align)?;
        if kind == SegmentKind::Text {
            trampolines = Trampolines::new(base + size.next_multiple_of(4), extra);
        }
        placements.insert(kind, Placement { original_start, original_end, base });
    }
    let section_address = |section: &Section| -> Option<u32> {
        segment_of(section)
            .and_then(|kind| placements.get(&kind))
            .map(|p| p.translate(section.header.address))
    };

    // 2. Copy contents.
    for section in &file.sections {
        let Some(address) = section_address(section) else { continue };
        if section.header.kind == SHT_NOBITS {
            memory.fill(address, section.header.size, 0)?;
        } else {
            memory.write_bytes(address, &section.data)?;
        }
    }

    // TLS sections are laid out contiguously in the data segment.
    let tls_sections: Vec<&Section> = file
        .sections
        .iter()
        .filter(|s| s.header.flags & SHF_RPL_TLS != 0 && segment_of(s).is_some())
        .collect();
    let tls = tls_sections.iter().min_by_key(|s| s.header.address).and_then(|first| {
        let end =
            tls_sections.iter().map(|s| s.header.address + s.memory_size()).max().unwrap_or(0);
        Some((
            first.header.address,
            TlsImage {
                module_index: TLS_MODULE_INDEX,
                start: section_address(first)?,
                size: end - first.header.address,
            },
        ))
    });

    // 3. Relocate.
    let mut symbol_cache: HashMap<(usize, u32), u32> = HashMap::new();
    for rela_section in file.sections.iter().filter(|s| s.header.kind == SHT_RELA) {
        let Some(target) = file.sections.get(rela_section.header.info as usize) else { continue };
        let Some(target_address) = section_address(target) else { continue };
        let symtab_index = rela_section.header.link as usize;
        for entry in rela_section.data.as_chunks::<RELA_SIZE>().0 {
            let rela = Rela::parse(entry, 0)?;
            let place = target_address + (rela.offset - target.header.address);
            if matches!(rela.kind, R_PPC_DTPMOD32 | R_PPC_DTPREL32) {
                let Some((link_start, image)) = tls else {
                    return Err(LoadError::UnsupportedRelocation(rela.kind));
                };
                let word = if rela.kind == R_PPC_DTPMOD32 {
                    image.module_index
                } else {
                    #[allow(clippy::cast_sign_loss)]
                    tls_offset(file, symtab_index, rela.symbol, link_start)?
                        .wrapping_add(rela.addend as u32)
                };
                memory.write_u32(place, word)?;
                continue;
            }
            let symbol_value = match symbol_cache.get(&(symtab_index, rela.symbol)) {
                Some(&value) => value,
                None => {
                    let value = resolve_symbol(
                        file,
                        symtab_index,
                        rela.symbol,
                        &section_address,
                        resolver,
                    )?;
                    symbol_cache.insert((symtab_index, rela.symbol), value);
                    value
                }
            };
            #[allow(clippy::cast_sign_loss)]
            let value = symbol_value.wrapping_add(rela.addend as u32);
            relocation::apply(memory, rela.kind, place, value, &mut trampolines)?;
        }
    }

    // 4. Translate addresses from the file info and header.
    let translate = |address: u32| -> u32 {
        placements
            .values()
            .find(|p| address >= p.original_start && address <= p.original_end + 0x8000)
            .map_or(address, |p| p.translate(address))
    };
    let exports = file
        .exports()?
        .into_iter()
        .map(|export| (export.name, translate(export.address)))
        .collect();
    let segments = placements
        .iter()
        .map(|(kind, p)| (*kind, (p.base, p.original_end - p.original_start)))
        .collect();

    let info = &file.file_info;
    Ok(LoadedModule {
        name: name.to_owned(),
        is_rpx: info.is_rpx(),
        entry: translate(file.header.entry),
        sda_base: if info.sda_base == 0 { 0 } else { translate(info.sda_base) },
        sda2_base: if info.sda2_base == 0 { 0 } else { translate(info.sda2_base) },
        stack_size: info.stack_size,
        segments,
        exports,
        tls: tls.map(|(_, image)| image),
    })
}

/// Offset of a TLS symbol from the start of the module's TLS block.
fn tls_offset(
    file: &RplFile,
    symtab_index: usize,
    index: u32,
    link_start: u32,
) -> Result<u32, LoadError> {
    let symtab = file
        .sections
        .get(symtab_index)
        .ok_or_else(|| LoadError::UndefinedSymbol(format!("#{index}")))?;
    let symbol = Symbol::parse(&symtab.data, index as usize * SYMBOL_SIZE)?;
    // `STT_TLS` symbols already hold the offset; section symbols hold the
    // section's link-time address.
    Ok(if symbol.info & 0xF == STT_TLS {
        symbol.value
    } else {
        symbol.value.wrapping_sub(link_start)
    })
}

fn resolve_symbol(
    file: &RplFile,
    symtab_index: usize,
    index: u32,
    section_address: &dyn Fn(&Section) -> Option<u32>,
    resolver: &mut dyn ImportResolver,
) -> Result<u32, LoadError> {
    let symtab = file
        .sections
        .get(symtab_index)
        .ok_or_else(|| LoadError::UndefinedSymbol(format!("#{index}")))?;
    let symbol = Symbol::parse(&symtab.data, index as usize * SYMBOL_SIZE)?;
    let strtab = file
        .sections
        .get(symtab.header.link as usize)
        .map(|s| s.data.as_slice())
        .unwrap_or_default();
    let name = || name_at(strtab, symbol.name);

    match symbol.section {
        // Retail RPXs reference undefined (weak) symbols such as `$UNDEF`;
        // like Cafe OS, they resolve to 0. Each symbol is resolved once (cached).
        SHN_UNDEF => {
            log::debug!("undefined symbol {:?} resolved to 0", name());
            Ok(0)
        }
        SHN_ABS => Ok(symbol.value),
        section_index => {
            let section = file
                .sections
                .get(usize::from(section_index))
                .ok_or_else(|| LoadError::UndefinedSymbol(name()))?;
            if let Some(library) = section.import_library() {
                let kind = if section.is_function_import() {
                    ImportKind::Function
                } else {
                    ImportKind::Data
                };
                return Ok(resolver.resolve(library, &name(), kind));
            }
            let base =
                section_address(section).ok_or_else(|| LoadError::UndefinedSymbol(name()))?;
            Ok(base.wrapping_add(symbol.value.wrapping_sub(section.header.address)))
        }
    }
}

/// Tests against the developer's own title (`shocora.toml`); they pass without one.
#[cfg(test)]
mod rom_tests {
    use wiiu_mem_addr::{MEM2, Region, TEXT};
    use wiiu_memory::Memory;
    use wiiu_test_roms::{report, require};

    use super::*;
    use crate::LinearAllocator;

    /// Resolves every import to one fake stub, counting them.
    struct Stubs(usize);

    impl ImportResolver for Stubs {
        fn resolve(&mut self, _library: &str, _symbol: &str, _kind: ImportKind) -> u32 {
            self.0 += 1;
            0x0E00_0000
        }
    }

    /// Intent: the loader places the `.rpx` segments in guest memory, applies every
    /// relocation and finds the entry point, with nothing from the OS but addresses for
    /// its imports.
    /// Pass: loading succeeds; the entry point lies inside the placed text segment; the
    /// first word there is not zero (the code was copied and relocated).
    #[test]
    fn loads_the_rpx_into_guest_memory() {
        let title = require!(wiiu_test_roms::title(), "no title");
        let file = RplFile::parse(&std::fs::read(wiiu_test_roms::rpx_of(&title).unwrap()).unwrap())
            .unwrap();
        let mut memory = Memory::with_game_layout().unwrap();
        let text = Region::new("TEXT", TEXT.start, TEXT.size);
        let data = Region::new("DATA", MEM2.start, 0x1000_0000);
        let read_only = Region::new("LOAD", MEM2.start + 0x1000_0000, 0x1000_0000);
        let mut allocator = LinearAllocator::new(text, data, read_only);
        let mut stubs = Stubs(0);
        let module = load(&file, "main", &mut memory, &mut allocator, &mut stubs).unwrap();
        let (start, size) = module.segments[&SegmentKind::Text];
        assert!((start..start + size).contains(&module.entry));
        assert_ne!(memory.read_u32(module.entry).unwrap(), 0);
        report("entry", format!("{:#010x}", module.entry));
        for kind in [SegmentKind::Text, SegmentKind::Data, SegmentKind::Load] {
            if let Some((start, size)) = module.segments.get(&kind) {
                report(&format!("{kind:?} segment"), format!("{start:#010x} + {size:#x}"));
            }
        }
        report(
            "sda / sda2 base",
            format!("{:#010x} / {:#010x}", module.sda_base, module.sda2_base),
        );
        report("imports resolved", stubs.0);
        report("TLS", format!("{:?}", module.tls));
    }
}
