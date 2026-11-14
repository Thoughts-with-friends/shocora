//! IOSU: the OS running on the Starbuck ARM co-processor.
//!
//! Cafe OS reaches IOSU only through IPC to named devices (`/dev/fsa`,
//! `/dev/mcp`, ...). [`Iosu`] owns those devices and routes requests to them.
//! Requests are typed for now; decoding the raw IPC buffers Cafe OS builds is
//! done by the caller.
mod error;
pub mod fsa;

use std::collections::HashMap;

pub use error::IosError;
pub use fsa::{Fsa, FsaError, FsaStat, OpenDir, OpenFile};
use wiiu_fs::Vfs;

/// Devices IOSU exposes to Cafe OS.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceKind {
    /// File system access.
    Fsa,
    /// Title/system management.
    Mcp,
    /// Application control (save directories, title metadata).
    Acp,
    /// Accounts.
    Act,
}

impl DeviceKind {
    pub fn from_path(path: &str) -> Option<Self> {
        Some(match path {
            "/dev/fsa" => Self::Fsa,
            "/dev/mcp" => Self::Mcp,
            "/dev/acp_main" => Self::Acp,
            "/dev/act" => Self::Act,
            _ => return None,
        })
    }
}

/// An open device handle (`IOS_Open` result).
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceHandle(pub u32);

/// IOSU state.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct Iosu {
    pub fsa: Fsa,
    open_devices: HashMap<DeviceHandle, DeviceKind>,
    next_handle: u32,
}

impl Iosu {
    #[inline]
    pub fn new(vfs: Vfs) -> Self {
        Self { fsa: Fsa::new(vfs), ..Self::default() }
    }

    /// `IOS_Open`.
    ///
    /// # Errors
    /// Fails for unknown device paths.
    pub fn open(&mut self, path: &str) -> Result<DeviceHandle, IosError> {
        let kind =
            DeviceKind::from_path(path).ok_or_else(|| IosError::NoSuchDevice(path.to_owned()))?;
        self.next_handle += 1;
        let handle = DeviceHandle(self.next_handle);
        self.open_devices.insert(handle, kind);
        Ok(handle)
    }

    /// `IOS_Close`.
    ///
    /// # Errors
    /// Fails for handles that are not open.
    #[inline]
    pub fn close(&mut self, handle: DeviceHandle) -> Result<(), IosError> {
        self.open_devices.remove(&handle).map(|_| ()).ok_or(IosError::BadHandle(handle.0))
    }

    #[inline]
    pub fn device(&self, handle: DeviceHandle) -> Option<DeviceKind> {
        self.open_devices.get(&handle).copied()
    }

    /// Takes over the mounted volumes of `running`, which a saved state does not hold.
    #[inline]
    pub fn adopt_volumes(&mut self, running: &mut Self) {
        std::mem::swap(self.fsa.vfs_mut(), running.fsa.vfs_mut());
    }

    /// The open device handles (`IOS_Open`), by handle.
    pub fn handles(&self) -> Vec<(DeviceHandle, DeviceKind)> {
        let mut handles: Vec<_> = self.open_devices.iter().map(|(h, k)| (*h, *k)).collect();
        handles.sort_unstable_by_key(|(handle, _)| handle.0);
        handles
    }

    #[inline]
    pub const fn vfs(&self) -> &Vfs {
        self.fsa.vfs()
    }

    #[inline]
    pub const fn vfs_mut(&mut self) -> &mut Vfs {
        self.fsa.vfs_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_known_devices_only() {
        let mut iosu = Iosu::default();
        let fsa = iosu.open("/dev/fsa").unwrap();
        assert_eq!(iosu.device(fsa), Some(DeviceKind::Fsa));
        assert!(iosu.open("/dev/nope").is_err());
        iosu.close(fsa).unwrap();
        assert!(iosu.close(fsa).is_err());
    }
}
