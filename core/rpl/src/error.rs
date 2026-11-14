use wiiu_memory::MemoryError;

/// Why a module could not be parsed or loaded.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("file is truncated (needed {needed} bytes at offset {offset:#x})")]
    Truncated { offset: usize, needed: usize },
    #[error("not a big-endian 32-bit PowerPC ELF")]
    BadMagic,
    #[error("ELF type {0:#06x} is not an RPL/RPX")]
    NotRpl(u16),
    #[error("RPL has no file info section")]
    MissingFileInfo,
    #[error("section {index} failed to decompress: {source}")]
    Decompress { index: usize, source: std::io::Error },
    #[error("relocation type {0} is not supported")]
    UnsupportedRelocation(u8),
    #[error("relocation at {address:#010x} cannot reach {target:#010x}")]
    RelocationOutOfRange { address: u32, target: u32 },
    #[error("symbol {0:?} is undefined")]
    UndefinedSymbol(String),
    #[error("out of guest memory for {kind:?} segment of {size:#x} bytes")]
    OutOfMemory { kind: crate::SegmentKind, size: u32 },
    #[error(transparent)]
    Memory(#[from] MemoryError),
}

#[cfg(test)]
mod tests {
    use super::LoadError;

    /// Intent: loader errors tell which part of an RPL/RPX is wrong.
    /// Pass: offsets, ELF types and symbol names appear in the messages.
    #[test]
    fn messages_locate_the_problem() {
        let truncated = LoadError::Truncated { offset: 0x40, needed: 4 };
        assert_eq!(truncated.to_string(), "file is truncated (needed 4 bytes at offset 0x40)");
        assert_eq!(LoadError::NotRpl(2).to_string(), "ELF type 0x0002 is not an RPL/RPX");
        assert!(LoadError::UndefinedSymbol("main".into()).to_string().contains("\"main\""));
    }
}
