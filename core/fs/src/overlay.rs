//! Read-only stack of devices, as an installed update lies over its base title.
use std::collections::{BTreeMap, HashMap};

use crate::{Device, DirEntry, FsError, GuestPath, Metadata};

/// Serves each path from the first layer that has it; directory listings merge.
/// Single files can be replaced outright, whatever the layers hold.
#[derive(Default)]
pub struct Overlay {
    /// Topmost first.
    layers: Vec<Box<dyn Device>>,
    files: HashMap<GuestPath, Vec<u8>>,
}

impl std::fmt::Debug for Overlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Overlay")
            .field("layers", &self.layers.len())
            .field("files", &self.files.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Overlay {
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `device` below the layers already present.
    #[must_use]
    pub fn layer(mut self, device: impl Device + 'static) -> Self {
        self.layers.push(Box::new(device));
        self
    }

    /// Serves `contents` at `path` (relative to the mount point).
    ///
    /// # Errors
    /// Fails if `path` is not a valid absolute path.
    #[inline]
    pub fn file(mut self, path: &str, contents: impl Into<Vec<u8>>) -> Result<Self, FsError> {
        self.files.insert(GuestPath::new(path)?, contents.into());
        Ok(self)
    }

    fn layer_with(&self, path: &GuestPath) -> Result<&dyn Device, FsError> {
        // One layer answers for itself, without a lookup before every read.
        if let [only] = self.layers.as_slice() {
            return Ok(only.as_ref());
        }
        self.layers
            .iter()
            .find(|layer| layer.metadata(path).is_ok())
            .map(AsRef::as_ref)
            .ok_or_else(|| FsError::NotFound(path.to_string()))
    }

    fn read_only(path: &GuestPath) -> FsError {
        FsError::Io {
            path: path.to_string(),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "read-only device"),
        }
    }

    /// The replaced files directly inside `dir`.
    fn files_in<'a>(&'a self, dir: &'a GuestPath) -> impl Iterator<Item = DirEntry> + 'a {
        self.files.iter().filter_map(move |(path, contents)| {
            let (parent, name) = path.as_str().rsplit_once('/')?;
            let parent = if parent.is_empty() { "/" } else { parent };
            (parent == dir.as_str()).then(|| DirEntry {
                name: name.to_owned(),
                metadata: Metadata { is_dir: false, size: contents.len() as u64 },
            })
        })
    }
}

impl Device for Overlay {
    fn describe(&self) -> String {
        let layers: Vec<String> = self.layers.iter().map(|layer| layer.describe()).collect();
        let mut text = format!("overlay, topmost first: {}", layers.join(" over "));
        let mut files: Vec<&str> = self.files.keys().map(GuestPath::as_str).collect();
        files.sort_unstable();
        if !files.is_empty() {
            text.push_str(&format!("; replaced files: {}", files.join(", ")));
        }
        text
    }

    fn metadata(&self, path: &GuestPath) -> Result<Metadata, FsError> {
        if let Some(contents) = self.files.get(path) {
            return Ok(Metadata { is_dir: false, size: contents.len() as u64 });
        }
        self.layer_with(path)?.metadata(path)
    }

    fn read_at(&self, path: &GuestPath, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        if let Some(contents) = self.files.get(path) {
            let start = usize::try_from(offset).unwrap_or(usize::MAX).min(contents.len());
            let len = buf.len().min(contents.len() - start);
            buf[..len].copy_from_slice(&contents[start..start + len]);
            return Ok(len);
        }
        self.layer_with(path)?.read_at(path, offset, buf)
    }

    fn write_at(&mut self, path: &GuestPath, _offset: u64, _data: &[u8]) -> Result<usize, FsError> {
        Err(Self::read_only(path))
    }

    fn read_dir(&self, path: &GuestPath) -> Result<Vec<DirEntry>, FsError> {
        let mut merged = BTreeMap::new();
        let mut found = false;
        // Lower layers first, so upper ones overwrite what they share.
        for layer in self.layers.iter().rev() {
            if let Ok(entries) = layer.read_dir(path) {
                found = true;
                merged.extend(entries.into_iter().map(|e| (e.name.clone(), e)));
            }
        }
        if !found {
            return Err(FsError::NotFound(path.to_string()));
        }
        merged.extend(self.files_in(path).map(|e| (e.name.clone(), e)));
        Ok(merged.into_values().collect())
    }

    fn remove(&mut self, path: &GuestPath) -> Result<(), FsError> {
        Err(Self::read_only(path))
    }

    fn rename(&mut self, from: &GuestPath, _to: &GuestPath) -> Result<(), FsError> {
        Err(Self::read_only(from))
    }

    fn make_dir(&mut self, path: &GuestPath) -> Result<(), FsError> {
        Err(Self::read_only(path))
    }

    fn truncate(&mut self, path: &GuestPath, _len: u64) -> Result<(), FsError> {
        Err(Self::read_only(path))
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;
    use crate::HostDirectory;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("shocora-overlay-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read(device: &Overlay, path: &str) -> Vec<u8> {
        let path = GuestPath::new(path).unwrap();
        let mut buf = vec![0; device.metadata(&path).unwrap().size as usize];
        let n = device.read_at(&path, 0, &mut buf).unwrap();
        buf.truncate(n);
        buf
    }

    #[test]
    fn upper_layer_and_files_win() {
        let (upper, lower) = (temp_dir("upper"), temp_dir("lower"));
        fs::create_dir_all(upper.join("d")).unwrap();
        fs::create_dir_all(lower.join("d")).unwrap();
        fs::write(upper.join("d/shared"), b"update").unwrap();
        fs::write(lower.join("d/shared"), b"base").unwrap();
        fs::write(lower.join("d/only_base"), b"base").unwrap();
        fs::write(lower.join("d/replaced"), b"base").unwrap();

        let overlay = Overlay::new()
            .layer(HostDirectory::read_only(&upper))
            .layer(HostDirectory::read_only(&lower))
            .file("/d/replaced", b"mine".to_vec())
            .unwrap();
        assert_eq!(read(&overlay, "/d/shared"), b"update");
        assert_eq!(read(&overlay, "/d/only_base"), b"base");
        assert_eq!(read(&overlay, "/d/replaced"), b"mine");
        let names: Vec<String> = overlay
            .read_dir(&GuestPath::new("/d").unwrap())
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["only_base", "replaced", "shared"]);
        assert!(overlay.metadata(&GuestPath::new("/missing").unwrap()).is_err());
        let _ = (fs::remove_dir_all(upper), fs::remove_dir_all(lower));
    }
}
