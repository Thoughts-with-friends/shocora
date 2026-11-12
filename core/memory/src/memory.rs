use wiiu_mem_addr::Region;

use crate::{MemoryError, host::AddressSpace};

/// The guest's effective address space.
///
/// Only explicitly mapped regions are accessible through this API; every other
/// access fails with [`MemoryError::Unmapped`]. All regions share one host
/// reservation at their guest offsets, see [`Self::host_base`]. Backing pages
/// are zero-initialised and committed lazily by the host OS, so mapping the
/// full 1 GiB of MEM2 is cheap until the guest touches it.
#[derive(Default)]
pub struct Memory {
    space: AddressSpace,
    /// Sorted by start address.
    regions: Vec<Region>,
}

impl std::fmt::Debug for Memory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.regions.iter()).finish()
    }
}

macro_rules! be_accessors {
    ($($read:ident, $write:ident: $ty:ty;)*) => {$(
        #[doc = concat!("Reads a big-endian `", stringify!($ty), "`.")]
        ///
        /// # Errors
        /// Fails if the access touches unmapped memory.
        #[inline]
        pub fn $read(&self, address: u32) -> Result<$ty, MemoryError> {
            let mut bytes = [0; size_of::<$ty>()];
            bytes.copy_from_slice(self.slice(address, size_of::<$ty>() as u32)?);
            Ok(<$ty>::from_be_bytes(bytes))
        }

        #[doc = concat!("Writes a big-endian `", stringify!($ty), "`.")]
        ///
        /// # Errors
        /// Fails if the access touches unmapped memory.
        #[inline]
        pub fn $write(&mut self, address: u32, value: $ty) -> Result<(), MemoryError> {
            self.slice_mut(address, size_of::<$ty>() as u32)?.copy_from_slice(&value.to_be_bytes());
            Ok(())
        }
    )*};
}

impl Memory {
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Maps every region of a game process ([`wiiu_mem_addr::GAME_REGIONS`]).
    ///
    /// # Errors
    /// Fails if any of them is already mapped.
    pub fn with_game_layout() -> Result<Self, MemoryError> {
        let mut memory = Self::new();
        for region in wiiu_mem_addr::GAME_REGIONS {
            memory.map(region)?;
        }
        Ok(memory)
    }

    /// Backs `region` with zeroed storage.
    ///
    /// # Errors
    /// Fails if `region` overlaps an existing mapping.
    pub fn map(&mut self, region: Region) -> Result<(), MemoryError> {
        let overlaps = self
            .regions
            .iter()
            .any(|r| u64::from(region.start) < r.end() && u64::from(r.start) < region.end());
        if overlaps {
            return Err(MemoryError::Overlap { name: region.name, start: region.start });
        }
        self.space.commit(region.start, region.size);
        let index = self.regions.partition_point(|r| r.start < region.start);
        self.regions.insert(index, region);
        Ok(())
    }

    /// Maps what `other`, a [`Self::share`] of this memory, mapped since. Its pages are
    /// already there.
    pub fn adopt_regions(&mut self, other: &Self) {
        for region in &other.regions {
            if !self.regions.contains(region) {
                // Regions of one address space never overlap: the other handle checked.
                let _ = self.map(*region);
            }
        }
    }

    /// Regions currently mapped, in address order.
    #[inline]
    pub fn regions(&self) -> impl Iterator<Item = Region> + '_ {
        self.regions.iter().copied()
    }

    #[inline]
    pub fn is_mapped(&self, address: u32, len: u32) -> bool {
        self.contains(address, len)
    }

    /// Host address of guest address 0. Guest address `a` is at `host_base() + a`.
    ///
    /// Only mapped regions may be dereferenced; see [`Self::is_mapped`].
    #[inline]
    pub fn host_base(&self) -> *mut u8 {
        self.space.base()
    }

    /// Another handle on the same guest memory, for a thread of its own (the GPU's): writes
    /// through either are seen by both, like on the console, where Espresso and Latte share
    /// MEM1 and MEM2. Who writes what when is up to the guest, as there.
    ///
    /// Regions mapped later through one handle are not known to the other.
    #[must_use]
    pub fn share(&self) -> Self {
        Self { space: self.space.clone(), regions: self.regions.clone() }
    }

    #[inline]
    fn contains(&self, address: u32, len: u32) -> bool {
        // Most accesses hit the same few regions: a short linear search wins.
        self.regions.iter().any(|r| r.contains_range(address, len))
    }

    /// Host view of `[address, address + len)`.
    ///
    /// # Errors
    /// Fails if the range is not entirely inside one mapped region.
    #[inline]
    pub fn slice(&self, address: u32, len: u32) -> Result<&[u8], MemoryError> {
        if !self.contains(address, len) {
            return Err(MemoryError::Unmapped { address, len });
        }
        // SAFETY: the range is committed (checked above) and the borrow of `self` guards it.
        Ok(unsafe {
            std::slice::from_raw_parts(self.space.base().add(address as usize), len as usize)
        })
    }

    /// Mutable host view of `[address, address + len)`.
    ///
    /// # Errors
    /// Fails if the range is not entirely inside one mapped region.
    #[inline]
    pub fn slice_mut(&mut self, address: u32, len: u32) -> Result<&mut [u8], MemoryError> {
        if !self.contains(address, len) {
            return Err(MemoryError::Unmapped { address, len });
        }
        // SAFETY: as in `slice`; `&mut self` makes the view exclusive.
        Ok(unsafe {
            std::slice::from_raw_parts_mut(self.space.base().add(address as usize), len as usize)
        })
    }

    be_accessors! {
        read_u8, write_u8: u8;
        read_u16, write_u16: u16;
        read_u32, write_u32: u32;
        read_u64, write_u64: u64;
    }

    /// # Errors
    /// Fails if the access touches unmapped memory.
    #[inline]
    pub fn read_f32(&self, address: u32) -> Result<f32, MemoryError> {
        self.read_u32(address).map(f32::from_bits)
    }

    /// # Errors
    /// Fails if the access touches unmapped memory.
    #[inline]
    pub fn write_f32(&mut self, address: u32, value: f32) -> Result<(), MemoryError> {
        self.write_u32(address, value.to_bits())
    }

    /// # Errors
    /// Fails if the access touches unmapped memory.
    #[inline]
    pub fn read_f64(&self, address: u32) -> Result<f64, MemoryError> {
        self.read_u64(address).map(f64::from_bits)
    }

    /// # Errors
    /// Fails if the access touches unmapped memory.
    #[inline]
    pub fn write_f64(&mut self, address: u32, value: f64) -> Result<(), MemoryError> {
        self.write_u64(address, value.to_bits())
    }

    /// Copies guest bytes into `out`.
    ///
    /// # Errors
    /// Fails if the range is unmapped.
    #[inline]
    pub fn read_bytes(&self, address: u32, out: &mut [u8]) -> Result<(), MemoryError> {
        out.copy_from_slice(self.slice(address, len_u32(out.len()))?);
        Ok(())
    }

    /// Copies `bytes` into guest memory.
    ///
    /// # Errors
    /// Fails if the range is unmapped.
    #[inline]
    pub fn write_bytes(&mut self, address: u32, bytes: &[u8]) -> Result<(), MemoryError> {
        self.slice_mut(address, len_u32(bytes.len()))?.copy_from_slice(bytes);
        Ok(())
    }

    /// Sets `len` bytes to `value`.
    ///
    /// # Errors
    /// Fails if the range is unmapped.
    pub fn fill(&mut self, address: u32, len: u32, value: u8) -> Result<(), MemoryError> {
        if len == 0 {
            return Ok(());
        }
        self.slice_mut(address, len)?.fill(value);
        Ok(())
    }

    /// Guest-to-guest copy with `memmove` semantics.
    ///
    /// # Errors
    /// Fails if either range is unmapped.
    pub fn copy_within(&mut self, dst: u32, src: u32, len: u32) -> Result<(), MemoryError> {
        if len == 0 {
            return Ok(());
        }
        if !self.contains(src, len) {
            return Err(MemoryError::Unmapped { address: src, len });
        }
        if !self.contains(dst, len) {
            return Err(MemoryError::Unmapped { address: dst, len });
        }
        // SAFETY: both ranges are committed; `copy` allows overlap.
        unsafe {
            let base = self.space.base();
            std::ptr::copy(base.add(src as usize), base.add(dst as usize), len as usize);
        }
        Ok(())
    }

    /// Reads a NUL-terminated string of at most `max_len` bytes (lossy UTF-8).
    ///
    /// # Errors
    /// Fails if memory runs out before the terminator.
    pub fn read_c_string(&self, address: u32, max_len: u32) -> Result<String, MemoryError> {
        let mut bytes = Vec::new();
        for offset in 0..max_len {
            match self.read_u8(address.wrapping_add(offset))? {
                0 => break,
                byte => bytes.push(byte),
            }
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Writes `text` followed by a NUL terminator.
    ///
    /// # Errors
    /// Fails if the range is unmapped.
    #[inline]
    pub fn write_c_string(&mut self, address: u32, text: &str) -> Result<(), MemoryError> {
        self.write_bytes(address, text.as_bytes())?;
        self.write_u8(address + len_u32(text.len()), 0)
    }
}

/// Guest lengths are 32-bit; host buffers larger than 4 GiB cannot address guest memory.
#[expect(clippy::cast_possible_truncation, reason = "guest address space is 32-bit")]
const fn len_u32(len: usize) -> u32 {
    len as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_memory() -> Memory {
        let mut memory = Memory::new();
        memory.map(Region::new("A", 0x1000, 0x100)).unwrap();
        memory.map(Region::new("B", 0x0000, 0x100)).unwrap();
        memory
    }

    #[test]
    fn big_endian_round_trip() {
        let mut memory = test_memory();
        memory.write_u32(0x1000, 0x1122_3344).unwrap();
        assert_eq!(memory.read_u8(0x1000).unwrap(), 0x11);
        assert_eq!(memory.read_u16(0x1002).unwrap(), 0x3344);
        memory.write_f64(0x1010, 1.5).unwrap();
        assert!((memory.read_f64(0x1010).unwrap() - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn rejects_unmapped_and_straddling_accesses() {
        let memory = test_memory();
        assert!(memory.read_u8(0x0800).is_err());
        assert!(memory.read_u32(0x10FE).is_err());
        assert!(memory.read_u32(0x10FC).is_ok());
    }

    #[test]
    fn rejects_overlapping_maps() {
        let mut memory = test_memory();
        assert!(memory.map(Region::new("C", 0x10F0, 0x100)).is_err());
        assert!(memory.map(Region::new("D", 0x0100, 0x0F00)).is_ok());
    }

    #[test]
    fn c_strings_and_moves() {
        let mut memory = test_memory();
        memory.write_c_string(0x1000, "shocora").unwrap();
        assert_eq!(memory.read_c_string(0x1000, 64).unwrap(), "shocora");
        memory.copy_within(0x1002, 0x1000, 5).unwrap();
        assert_eq!(memory.read_c_string(0x1000, 64).unwrap(), "shshoco");
        memory.copy_within(0x0010, 0x1000, 3).unwrap();
        assert_eq!(memory.read_c_string(0x0010, 3).unwrap(), "shs");
    }
}
