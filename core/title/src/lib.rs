//! Wii U title formats.
//!
//! * [`TitleSource::open`] recognises what the user pointed at: an installed
//!   title directory (`code/`, `content/`, `meta/`) or a bare `.rpx`.
//! * [`meta`] parses `meta.xml`, `app.xml` and `cos.xml`.
//! * [`update`] finds a title's update and downloadable content: next to the title, in
//!   `update/` and `dlc/` by the exe, or in system storage (`mlc01`).
//! * [`wux`] reads compressed disc images (`.wux`) as a flat WUD stream.
//!   Decrypting the disc partitions needs console keys and is not done here.
mod error;
pub mod meta;
mod source;
pub mod update;
pub mod wux;

pub use error::TitleError;
pub use source::{Dlc, TitleInfo, TitleSource};

#[cfg(test)]
mod tests {
    use super::*;

    /// Intent: opening something that is neither a title folder nor an `.rpx` fails with a
    /// message the library view can show.
    /// Pass: a missing path gives an error (no panic) that names the path.
    #[test]
    fn opening_a_missing_path_fails_cleanly() {
        let path = std::path::Path::new("definitely/not/a/title");
        let error = TitleSource::open(path).unwrap_err();
        assert!(error.to_string().contains("not"));
    }
}
