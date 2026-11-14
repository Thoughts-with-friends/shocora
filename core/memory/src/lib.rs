//! Wii U guest memory.
//!
//! [`Memory`] owns the backing storage of every mapped [`Region`] and offers
//! big-endian accessors. [`GuestPtr`] is a typed effective address that reads
//! and writes through a [`Memory`].
mod error;
mod guest_ptr;
mod host;
mod memory;

pub use error::MemoryError;
pub use guest_ptr::{GuestPtr, GuestValue};
pub use memory::Memory;
pub use wiiu_mem_addr::Region;

#[cfg(test)]
mod tests {
    use super::*;

    /// Intent: guest memory is big-endian, as the Espresso CPU sees it, whatever the host.
    /// Pass: a word written through `Memory` reads back as bytes most significant first,
    /// and through a `GuestPtr`.
    #[test]
    fn guest_memory_is_big_endian() {
        let mut memory = Memory::new();
        memory.map(Region::new("T", 0x1000, 0x100)).unwrap();
        memory.write_u32(0x1000, 0x1122_3344).unwrap();
        let mut bytes = [0; 4];
        memory.read_bytes(0x1000, &mut bytes).unwrap();
        assert_eq!(bytes, [0x11, 0x22, 0x33, 0x44]);
        assert_eq!(GuestPtr::<u32>::new(0x1000).read(&memory).unwrap(), 0x1122_3344);
    }
}
