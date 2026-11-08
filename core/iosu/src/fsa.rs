//! `/dev/fsa`: file system access on top of the [`Vfs`].
use std::collections::HashMap;

use wiiu_fs::{FsError, Vfs};

/// FSA failures, mapped to `FSStatus`/`FSError` codes by coreinit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FsaError {
    #[error("not found")]
    NotFound,
    #[error("permission denied")]
    PermissionDenied,
    #[error("invalid handle")]
    InvalidHandle,
    #[error("end of directory")]
    EndOfDirectory,
    #[error("is a directory")]
    IsDirectory,
    #[error("media error")]
    MediaError,
}

impl From<FsError> for FsaError {
    fn from(error: FsError) -> Self {
        match error {
            FsError::NotFound(_) | FsError::NotMounted(_) | FsError::InvalidPath(_) => {
                Self::NotFound
            }
            FsError::Io { source, .. } if source.kind() == std::io::ErrorKind::PermissionDenied => {
                Self::PermissionDenied
            }
            FsError::Io { .. } => Self::MediaError,
        }
    }
}

/// `FSStat` essentials.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsaStat {
    pub is_directory: bool,
    pub size: u64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct OpenFile {
    path: String,
    position: u64,
    size: u64,
    writable: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct OpenDir {
    entries: Vec<(String, FsaStat)>,
    next: usize,
}

/// FSA device state: the mount table plus open file/directory handles.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct Fsa {
    /// Host folders: a loaded state keeps those of the running title.
    #[serde(skip)]
    vfs: Vfs,
    files: HashMap<u32, OpenFile>,
    dirs: HashMap<u32, OpenDir>,
    next_handle: u32,
}

impl Fsa {
    #[inline]
    pub fn new(vfs: Vfs) -> Self {
        Self { vfs, ..Self::default() }
    }

    #[inline]
    pub const fn vfs_mut(&mut self) -> &mut Vfs {
        &mut self.vfs
    }

    fn allocate_handle(&mut self) -> u32 {
        self.next_handle += 1;
        self.next_handle
    }

    /// # Errors
    /// Fails if the path is missing (for read modes) or a directory.
    #[inline]
    pub fn stat(&self, path: &str) -> Result<FsaStat, FsaError> {
        let meta = self.vfs.metadata(path)?;
        Ok(FsaStat { is_directory: meta.is_dir, size: meta.size })
    }

    /// Opens a file with an `fopen`-style mode (`"r"`, `"w"`, `"a"`, `"r+"`, ...).
    ///
    /// # Errors
    /// Fails if the file is missing in a read mode or is a directory.
    pub fn open_file(&mut self, path: &str, mode: &str) -> Result<u32, FsaError> {
        let writable = mode.contains(['w', 'a', '+']);
        let creates = mode.contains(['w', 'a']);
        let size = match self.vfs.metadata(path) {
            Ok(meta) if meta.is_dir => return Err(FsaError::IsDirectory),
            Ok(meta) if !mode.contains('w') => meta.size,
            Ok(_) => {
                self.vfs.truncate(path, 0)?;
                0
            }
            Err(_) if creates => {
                self.vfs.write_at(path, 0, &[])?;
                0
            }
            Err(error) => return Err(error.into()),
        };
        let handle = self.allocate_handle();
        let position = if mode.contains('a') { size } else { 0 };
        self.files.insert(handle, OpenFile { path: path.to_owned(), position, size, writable });
        Ok(handle)
    }

    /// Reads up to `len` bytes at the file position.
    ///
    /// # Errors
    /// Fails for unknown handles or host I/O errors.
    pub fn read_file(&mut self, handle: u32, len: usize) -> Result<Vec<u8>, FsaError> {
        let file = self.files.get_mut(&handle).ok_or(FsaError::InvalidHandle)?;
        let mut buf = vec![0; len];
        let read = self.vfs.read_at(&file.path, file.position, &mut buf)?;
        buf.truncate(read);
        file.position += read as u64;
        Ok(buf)
    }

    /// Writes at the file position.
    ///
    /// # Errors
    /// Fails for unknown or read-only handles and host I/O errors.
    pub fn write_file(&mut self, handle: u32, data: &[u8]) -> Result<usize, FsaError> {
        let file = self.files.get_mut(&handle).ok_or(FsaError::InvalidHandle)?;
        if !file.writable {
            return Err(FsaError::PermissionDenied);
        }
        let written = self.vfs.write_at(&file.path, file.position, data)?;
        file.position += written as u64;
        file.size = file.size.max(file.position);
        Ok(written)
    }

    /// # Errors
    /// Fails for unknown handles.
    #[inline]
    pub fn set_position(&mut self, handle: u32, position: u64) -> Result<(), FsaError> {
        self.files.get_mut(&handle).ok_or(FsaError::InvalidHandle)?.position = position;
        Ok(())
    }

    /// `(position, size)` of an open file.
    ///
    /// # Errors
    /// Fails for unknown handles.
    #[inline]
    pub fn file_info(&self, handle: u32) -> Result<(u64, u64), FsaError> {
        self.files.get(&handle).map(|f| (f.position, f.size)).ok_or(FsaError::InvalidHandle)
    }

    /// # Errors
    /// Fails for unknown handles.
    #[inline]
    pub fn close_file(&mut self, handle: u32) -> Result<(), FsaError> {
        self.files.remove(&handle).map(|_| ()).ok_or(FsaError::InvalidHandle)
    }

    /// # Errors
    /// Fails if the path is missing or read-only.
    #[inline]
    pub fn remove(&mut self, path: &str) -> Result<(), FsaError> {
        Ok(self.vfs.remove(path)?)
    }

    /// # Errors
    /// Fails if `from` is missing or read-only.
    #[inline]
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), FsaError> {
        Ok(self.vfs.rename(from, to)?)
    }

    /// # Errors
    /// Fails if the path is read-only.
    #[inline]
    pub fn make_dir(&mut self, path: &str) -> Result<(), FsaError> {
        Ok(self.vfs.make_dir(path)?)
    }

    /// Cuts an open file at its position.
    ///
    /// # Errors
    /// Fails for unknown or read-only handles.
    pub fn truncate(&mut self, handle: u32) -> Result<(), FsaError> {
        let file = self.files.get_mut(&handle).ok_or(FsaError::InvalidHandle)?;
        if !file.writable {
            return Err(FsaError::PermissionDenied);
        }
        self.vfs.truncate(&file.path, file.position)?;
        file.size = file.position;
        Ok(())
    }

    /// # Errors
    /// Fails if the path is not a directory.
    pub fn open_dir(&mut self, path: &str) -> Result<u32, FsaError> {
        let entries = self
            .vfs
            .read_dir(path)?
            .into_iter()
            .map(|e| (e.name, FsaStat { is_directory: e.metadata.is_dir, size: e.metadata.size }))
            .collect();
        let handle = self.allocate_handle();
        self.dirs.insert(handle, OpenDir { entries, next: 0 });
        Ok(handle)
    }

    /// Next directory entry.
    ///
    /// # Errors
    /// [`FsaError::EndOfDirectory`] after the last entry.
    pub fn read_dir(&mut self, handle: u32) -> Result<(String, FsaStat), FsaError> {
        let dir = self.dirs.get_mut(&handle).ok_or(FsaError::InvalidHandle)?;
        let entry = dir.entries.get(dir.next).cloned().ok_or(FsaError::EndOfDirectory)?;
        dir.next += 1;
        Ok(entry)
    }

    /// Restarts a directory listing.
    ///
    /// # Errors
    /// Fails for unknown handles.
    #[inline]
    pub fn rewind_dir(&mut self, handle: u32) -> Result<(), FsaError> {
        self.dirs.get_mut(&handle).ok_or(FsaError::InvalidHandle)?.next = 0;
        Ok(())
    }

    /// # Errors
    /// Fails for unknown handles.
    #[inline]
    pub fn close_dir(&mut self, handle: u32) -> Result<(), FsaError> {
        self.dirs.remove(&handle).map(|_| ()).ok_or(FsaError::InvalidHandle)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use wiiu_fs::HostDirectory;

    use super::*;

    #[test]
    fn file_and_directory_handles() {
        let root = std::env::temp_dir().join(format!("shocora-fsa-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("content")).unwrap();
        fs::write(root.join("content/a.bin"), b"abcdef").unwrap();
        let mut vfs = Vfs::new();
        vfs.mount("/vol/content", HostDirectory::read_only(root.join("content"))).unwrap();
        vfs.mount("/vol/save", HostDirectory::writable(root.join("save"))).unwrap();
        let mut fsa = Fsa::new(vfs);

        let f = fsa.open_file("/vol/content/a.bin", "r").unwrap();
        assert_eq!(fsa.read_file(f, 4).unwrap(), b"abcd");
        assert_eq!(fsa.read_file(f, 4).unwrap(), b"ef");
        assert_eq!(fsa.file_info(f).unwrap(), (6, 6));
        assert_eq!(fsa.write_file(f, b"x"), Err(FsaError::PermissionDenied));
        fsa.close_file(f).unwrap();
        assert_eq!(fsa.open_file("/vol/content/none", "r"), Err(FsaError::NotFound));

        let w = fsa.open_file("/vol/save/s.dat", "w").unwrap();
        fsa.write_file(w, b"save").unwrap();
        fsa.close_file(w).unwrap();
        assert_eq!(fs::read(root.join("save/s.dat")).unwrap(), b"save");

        let d = fsa.open_dir("/vol/content").unwrap();
        assert_eq!(fsa.read_dir(d).unwrap().0, "a.bin");
        assert_eq!(fsa.read_dir(d), Err(FsaError::EndOfDirectory));
        fsa.close_dir(d).unwrap();
        let _ = fs::remove_dir_all(&root);
    }
}
