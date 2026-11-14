//! Wii U virtual file system.
//!
//! Guest paths such as `/vol/content/data.bin` are resolved through a mount
//! table: the longest matching mount prefix picks a [`Device`], which serves
//! the rest of the path: a host directory, or an [`Overlay`] of several (an
//! update over its base title).
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
