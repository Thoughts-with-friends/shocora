use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

use crate::{FsError, GuestPath};

/// File or directory attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata {
    pub is_dir: bool,
    pub size: u64,
}

/// One directory listing entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub metadata: Metadata,
}

/// Something that can be mounted: paths are relative to the mount point.
pub trait Device: Send {
    /// # Errors
    /// Fails if the path does not exist.
    fn metadata(&self, path: &GuestPath) -> Result<Metadata, FsError>;
    /// # Errors
    /// Fails if the path is not a readable file.
    fn read_at(&self, path: &GuestPath, offset: u64, buf: &mut [u8]) -> Result<usize, FsError>;
    /// # Errors
    /// Fails if the device is read-only or the write fails.
    fn write_at(&mut self, path: &GuestPath, offset: u64, data: &[u8]) -> Result<usize, FsError>;
    /// # Errors
    /// Fails if the path is not a directory.
    fn read_dir(&self, path: &GuestPath) -> Result<Vec<DirEntry>, FsError>;
    /// # Errors
    /// Fails if the device is read-only or the path does not exist.
    fn remove(&mut self, path: &GuestPath) -> Result<(), FsError>;
    /// # Errors
    /// Fails if the device is read-only or `from` does not exist.
    fn rename(&mut self, from: &GuestPath, to: &GuestPath) -> Result<(), FsError>;
    /// # Errors
    /// Fails if the device is read-only.
    fn make_dir(&mut self, path: &GuestPath) -> Result<(), FsError>;
    /// # Errors
    /// Fails if the device is read-only or the file cannot be resized.
    fn truncate(&mut self, path: &GuestPath, len: u64) -> Result<(), FsError>;

    /// What the device is, for people: where its files come from.
    fn describe(&self) -> String {
        "device".to_owned()
    }
}

/// A host directory exposed as a device.
#[derive(Debug, Clone)]
pub struct HostDirectory {
    root: PathBuf,
    writable: bool,
}

impl HostDirectory {
    #[inline]
    pub fn read_only(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), writable: false }
    }

    #[inline]
    pub fn writable(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), writable: true }
    }

    fn host_path(&self, path: &GuestPath) -> PathBuf {
        path.components().fold(self.root.clone(), |acc, part| acc.join(part))
    }

    fn check_writable(&self, path: &GuestPath) -> Result<(), FsError> {
        if self.writable {
            return Ok(());
        }
        Err(FsError::Io {
            path: path.to_string(),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "read-only device"),
        })
    }

    fn io_error(path: &GuestPath) -> impl FnOnce(std::io::Error) -> FsError + '_ {
        move |source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                FsError::NotFound(path.to_string())
            } else {
                FsError::Io { path: path.to_string(), source }
            }
        }
    }
}

fn metadata_of(meta: &fs::Metadata) -> Metadata {
    Metadata { is_dir: meta.is_dir(), size: if meta.is_dir() { 0 } else { meta.len() } }
}

impl Device for HostDirectory {
    fn describe(&self) -> String {
        let access = if self.writable { "writable" } else { "read-only" };
        format!("host folder {} ({access})", self.root.display())
    }

    fn metadata(&self, path: &GuestPath) -> Result<Metadata, FsError> {
        fs::metadata(self.host_path(path)).map(|m| metadata_of(&m)).map_err(Self::io_error(path))
    }

    fn read_at(&self, path: &GuestPath, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let mut file = fs::File::open(self.host_path(path)).map_err(Self::io_error(path))?;
        file.seek(SeekFrom::Start(offset)).map_err(Self::io_error(path))?;
        let mut done = 0;
        while done < buf.len() {
            match file.read(&mut buf[done..]).map_err(Self::io_error(path))? {
                0 => break,
                n => done += n,
            }
        }
        Ok(done)
    }

    fn write_at(&mut self, path: &GuestPath, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        self.check_writable(path)?;
        let host = self.host_path(path);
        if let Some(parent) = host.parent() {
            fs::create_dir_all(parent).map_err(Self::io_error(path))?;
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(host)
            .map_err(Self::io_error(path))?;
        file.seek(SeekFrom::Start(offset)).map_err(Self::io_error(path))?;
        file.write_all(data).map_err(Self::io_error(path))?;
        Ok(data.len())
    }

    fn read_dir(&self, path: &GuestPath) -> Result<Vec<DirEntry>, FsError> {
        let mut entries = fs::read_dir(self.host_path(path))
            .map_err(Self::io_error(path))?
            .filter_map(Result::ok)
            .filter_map(|e| {
                let metadata = e.metadata().ok()?;
                Some(DirEntry {
                    name: e.file_name().to_string_lossy().into_owned(),
                    metadata: metadata_of(&metadata),
                })
            })
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn remove(&mut self, path: &GuestPath) -> Result<(), FsError> {
        self.check_writable(path)?;
        let host = self.host_path(path);
        let result = if host.is_dir() { fs::remove_dir_all(host) } else { fs::remove_file(host) };
        result.map_err(Self::io_error(path))
    }

    fn rename(&mut self, from: &GuestPath, to: &GuestPath) -> Result<(), FsError> {
        self.check_writable(from)?;
        fs::rename(self.host_path(from), self.host_path(to)).map_err(Self::io_error(from))
    }

    fn make_dir(&mut self, path: &GuestPath) -> Result<(), FsError> {
        self.check_writable(path)?;
        fs::create_dir_all(self.host_path(path)).map_err(Self::io_error(path))
    }

    fn truncate(&mut self, path: &GuestPath, len: u64) -> Result<(), FsError> {
        self.check_writable(path)?;
        let file = fs::OpenOptions::new()
            .write(true)
            .open(self.host_path(path))
            .map_err(Self::io_error(path))?;
        file.set_len(len).map_err(Self::io_error(path))
    }
}

/// Tests against the developer's own title (`shocora.toml`); they pass without one.
#[cfg(test)]
mod rom_tests {
    use wiiu_test_roms::{report, require};

    use super::*;

    /// Intent: a read-only host folder serves a title's `content/` the way `/vol/content`
    /// needs it: listing, sizes, and reads at an offset.
    /// Pass: the root lists the same entries as the host folder, and the first 16 bytes of
    /// the first file read through the device equal the host file's.
    #[test]
    fn serves_the_title_content() {
        let title = require!(wiiu_test_roms::title(), "no title");
        let device = HostDirectory::read_only(title.join("content"));
        let root = GuestPath::new("/").unwrap();
        let entries = device.read_dir(&root).unwrap();
        let host = std::fs::read_dir(title.join("content")).unwrap().count();
        assert_eq!(entries.len(), host);
        let dirs = entries.iter().filter(|e| e.metadata.is_dir).count();
        report("content root entries", format!("{} ({dirs} folders)", entries.len()));
        let mut names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        names.sort_unstable();
        report("content folders", names.join(", "));

        let Some(file) = entries.iter().find(|e| !e.metadata.is_dir) else { return };
        let path = GuestPath::new(&format!("/{}", file.name)).unwrap();
        let mut buf = [0_u8; 16];
        let read = device.read_at(&path, 0, &mut buf).unwrap();
        let host = std::fs::read(title.join("content").join(&file.name)).unwrap();
        assert_eq!(&buf[..read], &host[..read]);
        report("first file", format!("{} ({} bytes)", file.name, file.metadata.size));
    }
}
