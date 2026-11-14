use crate::{Device, DirEntry, FsError, GuestPath, Metadata};

struct Mount {
    point: GuestPath,
    device: Box<dyn Device>,
}

/// The mount table.
#[derive(Default)]
pub struct Vfs {
    /// Sorted by descending mount-point length so the first match is the longest.
    mounts: Vec<Mount>,
}

impl std::fmt::Debug for Vfs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.mounts.iter().map(|m| m.point.as_str())).finish()
    }
}

impl Vfs {
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mounts `device` at `point`, replacing any previous mount there.
    ///
    /// # Errors
    /// Fails if `point` is not a valid absolute path.
    pub fn mount(&mut self, point: &str, device: impl Device + 'static) -> Result<(), FsError> {
        let point = GuestPath::new(point)?;
        self.mounts.retain(|m| m.point != point);
        self.mounts.push(Mount { point, device: Box::new(device) });
        self.mounts.sort_by_key(|m| std::cmp::Reverse(m.point.as_str().len()));
        Ok(())
    }

    #[inline]
    pub fn unmount_all(&mut self) {
        self.mounts.clear();
    }

    /// Every mount point, longest first.
    #[inline]
    pub fn mount_points(&self) -> Vec<&str> {
        self.mounts.iter().map(|m| m.point.as_str()).collect()
    }

    /// Every mount, longest point first: the point and what its device is.
    pub fn mounts(&self) -> Vec<(&str, String)> {
        self.mounts.iter().map(|m| (m.point.as_str(), m.device.describe())).collect()
    }

    #[inline]
    pub fn is_mounted(&self, point: &str) -> bool {
        GuestPath::new(point).is_ok_and(|p| self.mounts.iter().any(|m| m.point == p))
    }

    fn resolve(&self, raw: &str) -> Result<(&Mount, GuestPath), FsError> {
        let path = GuestPath::new(raw)?;
        self.mounts
            .iter()
            .find_map(|m| path.strip_prefix(&m.point).map(|rest| (m, rest)))
            .ok_or_else(|| FsError::NotMounted(raw.to_owned()))
    }

    /// # Errors
    /// Fails if nothing is mounted there or the path does not exist.
    #[inline]
    pub fn metadata(&self, path: &str) -> Result<Metadata, FsError> {
        let (mount, rest) = self.resolve(path)?;
        mount.device.metadata(&rest)
    }

    /// # Errors
    /// Fails if nothing is mounted there or the file cannot be read.
    #[inline]
    pub fn read_at(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let (mount, rest) = self.resolve(path)?;
        mount.device.read_at(&rest, offset, buf)
    }

    /// Reads a whole file.
    ///
    /// # Errors
    /// Fails if nothing is mounted there or the file cannot be read.
    #[allow(clippy::cast_possible_truncation)]
    pub fn read(&self, path: &str) -> Result<Vec<u8>, FsError> {
        let (mount, rest) = self.resolve(path)?;
        let size = mount.device.metadata(&rest)?.size;
        let mut buf = vec![0; size as usize];
        let read = mount.device.read_at(&rest, 0, &mut buf)?;
        buf.truncate(read);
        Ok(buf)
    }

    /// # Errors
    /// Fails if nothing is mounted there or the device rejects the write.
    pub fn write_at(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        let path = GuestPath::new(path)?;
        let mount = self
            .mounts
            .iter_mut()
            .find(|m| path.strip_prefix(&m.point).is_some())
            .ok_or_else(|| FsError::NotMounted(path.to_string()))?;
        let rest =
            path.strip_prefix(&mount.point).ok_or_else(|| FsError::NotMounted(path.to_string()))?;
        mount.device.write_at(&rest, offset, data)
    }

    /// # Errors
    /// Fails if nothing is mounted there or the path is not a directory.
    #[inline]
    pub fn read_dir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        let (mount, rest) = self.resolve(path)?;
        mount.device.read_dir(&rest)
    }

    fn resolve_mut(&mut self, path: &str) -> Result<(&mut Mount, GuestPath), FsError> {
        let path = GuestPath::new(path)?;
        self.mounts
            .iter_mut()
            .find_map(|m| path.strip_prefix(&m.point).map(|rest| (m, rest)))
            .ok_or_else(|| FsError::NotMounted(path.to_string()))
    }

    /// # Errors
    /// Fails if nothing is mounted there or the device rejects the removal.
    #[inline]
    pub fn remove(&mut self, path: &str) -> Result<(), FsError> {
        let (mount, rest) = self.resolve_mut(path)?;
        mount.device.remove(&rest)
    }

    /// Renames within one mount.
    ///
    /// # Errors
    /// Fails if nothing is mounted there or the device rejects the rename.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), FsError> {
        let to = GuestPath::new(to)?;
        let (mount, rest) = self.resolve_mut(from)?;
        let target =
            to.strip_prefix(&mount.point).ok_or_else(|| FsError::NotMounted(to.to_string()))?;
        mount.device.rename(&rest, &target)
    }

    /// # Errors
    /// Fails if nothing is mounted there or the device rejects the creation.
    #[inline]
    pub fn make_dir(&mut self, path: &str) -> Result<(), FsError> {
        let (mount, rest) = self.resolve_mut(path)?;
        mount.device.make_dir(&rest)
    }

    /// # Errors
    /// Fails if nothing is mounted there or the device rejects the resize.
    #[inline]
    pub fn truncate(&mut self, path: &str, len: u64) -> Result<(), FsError> {
        let (mount, rest) = self.resolve_mut(path)?;
        mount.device.truncate(&rest, len)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::HostDirectory;

    #[test]
    fn longest_mount_wins_and_reads_files() {
        let root = std::env::temp_dir().join(format!("shocora-vfs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("content/sub")).unwrap();
        fs::create_dir_all(root.join("save")).unwrap();
        fs::write(root.join("content/sub/a.txt"), b"hello").unwrap();

        let mut vfs = Vfs::new();
        vfs.mount("/vol", HostDirectory::read_only(&root)).unwrap();
        vfs.mount("/vol/save", HostDirectory::writable(root.join("save"))).unwrap();

        assert_eq!(vfs.read("/vol/content/sub/a.txt").unwrap(), b"hello");
        let mut buf = [0_u8; 3];
        assert_eq!(vfs.read_at("/vol/content/sub/a.txt", 2, &mut buf).unwrap(), 3);
        assert_eq!(&buf, b"llo");
        assert!(vfs.metadata("/vol/content/sub").unwrap().is_dir);
        assert_eq!(vfs.read_dir("/vol/content/sub").unwrap()[0].name, "a.txt");

        vfs.write_at("/vol/save/user/x.bin", 0, b"ok").unwrap();
        assert_eq!(fs::read(root.join("save/user/x.bin")).unwrap(), b"ok");
        assert!(vfs.write_at("/vol/content/y", 0, b"no").is_err());
        assert!(matches!(vfs.read("/dev/x"), Err(FsError::NotMounted(_))));
        assert!(matches!(vfs.read("/vol/missing"), Err(FsError::NotFound(_))));
        let _ = fs::remove_dir_all(&root);
    }
}

/// Tests against the developer's own title (`shocora.toml`); they pass without one.
#[cfg(test)]
mod rom_tests {
    use wiiu_test_roms::{report, require};

    use super::*;
    use crate::HostDirectory;

    /// Intent: the title's folders mounted as the console's volumes (`/vol/code`,
    /// `/vol/content`, `/vol/meta`) give the paths the title opens.
    /// Pass: `/vol/code/app.xml` and `/vol/meta/meta.xml` read back byte for byte, and
    /// `/vol/content` is a folder.
    #[test]
    fn mounts_the_title_volumes() {
        let title = require!(wiiu_test_roms::title(), "no title");
        let mut vfs = Vfs::new();
        for (point, dir) in
            [("/vol/code", "code"), ("/vol/content", "content"), ("/vol/meta", "meta")]
        {
            vfs.mount(point, HostDirectory::read_only(title.join(dir))).unwrap();
        }
        let app = vfs.read("/vol/code/app.xml").unwrap();
        assert_eq!(app, std::fs::read(title.join("code/app.xml")).unwrap());
        let meta = vfs.read("/vol/meta/meta.xml").unwrap();
        assert_eq!(meta, std::fs::read(title.join("meta/meta.xml")).unwrap());
        assert!(vfs.metadata("/vol/content").unwrap().is_dir);
        let code: Vec<String> =
            vfs.read_dir("/vol/code").unwrap().into_iter().map(|e| e.name).collect();
        report("/vol/code", code.join(", "));
        report("/vol/meta/meta.xml bytes", meta.len());
    }
}
