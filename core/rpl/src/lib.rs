//! RPX/RPL loader.
//!
//! ```text
//!  bytes ──► elf::ElfFile ──► rpl::RplFile ──► loader::load ──► LoadedModule
//!            (headers,         (decompressed     (place segments,
//!             sections)         sections, file    relocate, resolve
//!                               info, imports,    imports through
//!                               exports)          ImportResolver)
//! ```
//!
//! Parsing is pure; only [`loader::load`] touches guest memory, and it asks
//! its caller where segments go ([`SegmentAllocator`]) and what imports
//! resolve to ([`ImportResolver`]).
mod allocator;
#[cfg(any(test, feature = "builder"))]
pub mod builder;
pub mod elf;
mod error;
pub mod loader;
mod relocation;
pub mod rpl;

pub use allocator::LinearAllocator;
pub use error::LoadError;
pub use loader::{
    ImportKind, ImportResolver, LoadedModule, SegmentAllocator, SegmentKind, TLS_MODULE_INDEX,
    TlsImage, load,
};
pub use rpl::{FileInfo, RplFile};

#[cfg(test)]
mod tests {
    use super::*;

    /// Intent: a program built by the test builder parses back with the public API: the
    /// round trip every loader test relies on.
    /// Pass: the built RPX parses, imports `coreinit::exit`, and its file info is present.
    #[test]
    fn built_programs_parse_back() {
        let rpx = builder::RplBuilder::new()
            .text(&[0x4800_0001, 0x4800_0000])
            .call_import(0, "coreinit", "exit")
            .build();
        let file = RplFile::parse(&rpx).unwrap();
        assert!(file.imported_libraries().contains(&"coreinit"));
        assert_eq!(TLS_MODULE_INDEX, 1);
    }
}
