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
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!               ▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ RPX/RPL file       │   │ lib.rs                   │   │ code in memory     │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ executable loader        │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Wii U programs come as RPX (the game) and RPL (libraries) files. This loads them into memory
//! and links them. Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
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
