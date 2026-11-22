//! Wii U virtual file system.
//!
//! Guest paths such as `/vol/content/data.bin` are resolved through a mount
//! table: the longest matching mount prefix picks a [`Device`], which serves
//! the rest of the path: a host directory, or an [`Overlay`] of several (an
//! update over its base title).
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//! ▲▲▲▲▲▲▲▲▲▲    ▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ paths like         │   │ lib.rs                   │   │ real files on PC   │
//! │ /vol/content       │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ virtual file system      │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The game asks for paths like /vol/content/…; this maps them to folders and files on your PC.
//! Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
mod device;
mod error;
mod overlay;
mod path;
mod vfs;

pub use device::{Device, DirEntry, HostDirectory, Metadata};
pub use error::FsError;
pub use overlay::Overlay;
pub use path::GuestPath;
pub use vfs::Vfs;

#[cfg(test)]
mod tests {
    use super::*;

    /// Intent: the crate's public types fit together: an in-memory `Overlay` mounted in a
    /// `Vfs` serves a file at a guest path.
    /// Pass: the file reads back and its metadata says it is a file of that size.
    #[test]
    fn overlay_files_serve_through_the_vfs() {
        let mut vfs = Vfs::new();
        vfs.mount("/vol/content", Overlay::new().file("/a.bin", b"abc".to_vec()).unwrap()).unwrap();
        assert_eq!(vfs.read("/vol/content/a.bin").unwrap(), b"abc");
        let meta = vfs.metadata("/vol/content/a.bin").unwrap();
        assert!(!meta.is_dir && meta.size == 3);
    }
}
