//! `FS*`: the coreinit file-system client, forwarding to IOSU's `/dev/fsa`.
//!
//! Only the synchronous calls exist so far; the `*Async` variants need guest
//! callbacks / message queues and arrive with the thread scheduler.
pub mod api;
pub mod mutate;

use std::collections::HashMap;

use wiiu_iosu::{FsaError, FsaStat};
use wiiu_memory::{Memory, MemoryError};

/// `FSStatus` codes.
pub mod status {
    pub const OK: i32 = 0;
    /// End of a directory listing or file.
    pub const END: i32 = -2;
    pub const EXISTS: i32 = -5;
    pub const NOT_FOUND: i32 = -6;
    pub const NOT_FILE: i32 = -7;
    pub const NOT_DIR: i32 = -8;
    pub const PERMISSION_ERROR: i32 = -10;
    pub const FATAL_ERROR: i32 = -0x400;
}

/// `FSFlag` bits of `FSStat::flag`.
const FLAG_IS_DIR: u32 = 0x8000_0000;
const FLAG_IS_FILE: u32 = 0x0100_0000;
/// `sizeof(FSStat)`.
pub const STAT_SIZE: u32 = 0x64;
/// Offset of the name in `FSDirEntry`.
pub const DIR_ENTRY_NAME: u32 = 0x64;
/// Room for the name in `FSDirEntry` (including the terminator).
pub const DIR_ENTRY_NAME_SIZE: usize = 0x80;

/// Per-client state.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub cwd: String,
}

impl Default for Client {
    fn default() -> Self {
        Self { cwd: "/".to_owned() }
    }
}

/// All registered `FSClient`s, keyed by their guest address.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct FsState {
    pub initialized: bool,
    pub clients: HashMap<u32, Client>,
}

impl FsState {
    /// Makes `path` absolute using the client's working directory.
    pub fn resolve(&self, client: u32, path: &str) -> String {
        if path.starts_with('/') {
            return path.to_owned();
        }
        let cwd = self.clients.get(&client).map_or("/", |c| c.cwd.as_str());
        format!("{}/{path}", cwd.trim_end_matches('/'))
    }
}

/// Maps an FSA failure to the `FSStatus` the guest sees.
pub const fn status_of(error: FsaError) -> i32 {
    match error {
        FsaError::NotFound => status::NOT_FOUND,
        FsaError::PermissionDenied => status::PERMISSION_ERROR,
        FsaError::EndOfDirectory => status::END,
        FsaError::IsDirectory => status::NOT_FILE,
        FsaError::InvalidHandle | FsaError::MediaError => status::FATAL_ERROR,
    }
}

/// Writes an `FSStat`.
///
/// # Errors
/// Fails if `address` is unmapped.
#[allow(clippy::cast_possible_truncation)]
pub fn write_stat(memory: &mut Memory, address: u32, stat: FsaStat) -> Result<(), MemoryError> {
    memory.fill(address, STAT_SIZE, 0)?;
    let flag = if stat.is_directory { FLAG_IS_DIR } else { FLAG_IS_FILE };
    let size = stat.size.min(u64::from(u32::MAX)) as u32;
    memory.write_u32(address, flag)?;
    memory.write_u32(address + 0x04, 0x666)?; // permissions
    memory.write_u32(address + 0x10, size)?;
    memory.write_u32(address + 0x14, size)?; // allocated size
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_paths() {
        let mut fs = FsState::default();
        fs.clients.insert(1, Client { cwd: "/vol/content/".into() });
        assert_eq!(fs.resolve(1, "a/b.bin"), "/vol/content/a/b.bin");
        assert_eq!(fs.resolve(1, "/vol/save/x"), "/vol/save/x");
        assert_eq!(fs.resolve(2, "x"), "/x");
    }
}
