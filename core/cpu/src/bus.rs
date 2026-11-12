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
