/// A failed bus access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("bus error at {address:#010x}")]
pub struct BusError {
    pub address: u32,
}

/// Everything the CPU needs from the outside world.
///
/// All accesses are big-endian. Reads take `&mut self` so implementations can
/// model side effects of MMIO reads.
pub trait Bus {
    /// # Errors
    /// Fails if `address` is not backed by memory or a device.
    fn read_u8(&mut self, address: u32) -> Result<u8, BusError>;
    /// # Errors
    /// Fails if `address` is not backed by memory or a device.
    fn read_u16(&mut self, address: u32) -> Result<u16, BusError>;
    /// # Errors
    /// Fails if `address` is not backed by memory or a device.
    fn read_u32(&mut self, address: u32) -> Result<u32, BusError>;
    /// # Errors
    /// Fails if `address` is not backed by memory or a device.
    fn read_u64(&mut self, address: u32) -> Result<u64, BusError>;
    /// # Errors
    /// Fails if `address` is not backed by memory or a device.
    fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError>;
    /// # Errors
    /// Fails if `address` is not backed by memory or a device.
    fn write_u16(&mut self, address: u32, value: u16) -> Result<(), BusError>;
    /// # Errors
    /// Fails if `address` is not backed by memory or a device.
    fn write_u32(&mut self, address: u32, value: u32) -> Result<(), BusError>;
    /// # Errors
    /// Fails if `address` is not backed by memory or a device.
    fn write_u64(&mut self, address: u32, value: u64) -> Result<(), BusError>;

    /// Current value of the 64-bit timebase (`mftb`).
    fn timebase(&mut self) -> u64;

    /// Instruction fetch. Defaults to a data read.
    ///
    /// # Errors
    /// Fails if `address` is not backed by memory.
    fn fetch(&mut self, address: u32) -> Result<u32, BusError> {
        self.read_u32(address)
    }

    /// Stores `new` at `address` if it still holds `expected`, in one step that other cores
    /// cannot come between (`stwcx.`). Returns whether it stored.
    ///
    /// # Errors
    /// Fails if `address` is not backed by memory.
    fn compare_exchange_u32(
        &mut self,
        address: u32,
        expected: u32,
        new: u32,
    ) -> Result<bool, BusError> {
        let matches = self.read_u32(address)? == expected;
        if matches {
            self.write_u32(address, new)?;
        }
        Ok(matches)
    }

    /// Host address of guest address 0 when the guest's memory is flat, which
    /// lets the JIT access it directly. Guest bugs that touch unmapped memory
    /// then fault in the host instead of reporting a [`BusError`].
    fn fast_memory(&mut self) -> Option<*mut u8> {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{Bus, BusError};

    /// A word-addressed toy bus.
    #[derive(Default)]
    struct Words(HashMap<u32, u32>);

    #[allow(clippy::cast_possible_truncation, reason = "a toy bus")]
    impl Bus for Words {
        fn read_u8(&mut self, a: u32) -> Result<u8, BusError> {
            Ok(self.read_u32(a)? as u8)
        }
        fn read_u16(&mut self, a: u32) -> Result<u16, BusError> {
            Ok(self.read_u32(a)? as u16)
        }
        fn read_u32(&mut self, address: u32) -> Result<u32, BusError> {
            self.0.get(&address).copied().ok_or(BusError { address })
        }
        fn read_u64(&mut self, a: u32) -> Result<u64, BusError> {
            Ok(self.read_u32(a)?.into())
        }
        fn write_u8(&mut self, a: u32, v: u8) -> Result<(), BusError> {
            self.write_u32(a, v.into())
        }
        fn write_u16(&mut self, a: u32, v: u16) -> Result<(), BusError> {
            self.write_u32(a, v.into())
        }
        fn write_u32(&mut self, address: u32, value: u32) -> Result<(), BusError> {
            self.0.insert(address, value);
            Ok(())
        }
        fn write_u64(&mut self, a: u32, v: u64) -> Result<(), BusError> {
            self.write_u32(a, v as u32)
        }
        fn timebase(&mut self) -> u64 {
            0
        }
    }

    /// Intent: the default `compare_exchange_u32` is the `stwcx.` rule: store only if the
    /// word still holds what `lwarx` read; and instruction fetch defaults to a data read.
    /// Pass: a matching compare stores and returns true, a stale one leaves the word and
    /// returns false; fetching an unmapped word reports its address.
    #[test]
    fn default_reservation_store_and_fetch() {
        let mut bus = Words::default();
        bus.write_u32(0x100, 1).unwrap();
        assert!(bus.compare_exchange_u32(0x100, 1, 2).unwrap());
        assert!(!bus.compare_exchange_u32(0x100, 1, 3).unwrap());
        assert_eq!(bus.read_u32(0x100).unwrap(), 2);
        assert_eq!(bus.fetch(0x100).unwrap(), 2);
        assert_eq!(bus.fetch(0x200).unwrap_err().address, 0x200);
        assert!(bus.fast_memory().is_none());
    }
}
