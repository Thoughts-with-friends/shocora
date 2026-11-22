//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                 ▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ addresses          │   │ guest_ptr.rs             │   │ safe reads/writes  │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ typed guest pointers     │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! A game memory address that remembers what kind of data lives there. Terms such as HLE, JIT
//! or shader are explained in `docs/guide/wiiu_term.md`.

use std::{fmt, marker::PhantomData};

use crate::{Memory, MemoryError};

/// A value with a fixed big-endian guest representation.
pub trait GuestValue: Sized {
    /// Size in guest memory, in bytes.
    const SIZE: u32;

    /// # Errors
    /// Fails if the access touches unmapped memory.
    fn read(memory: &Memory, address: u32) -> Result<Self, MemoryError>;

    /// # Errors
    /// Fails if the access touches unmapped memory.
    fn write(self, memory: &mut Memory, address: u32) -> Result<(), MemoryError>;
}

macro_rules! guest_value {
    ($($ty:ty => $read:ident, $write:ident, $raw:ty;)*) => {$(
        impl GuestValue for $ty {
            const SIZE: u32 = size_of::<$ty>() as u32;

            #[inline]
            #[allow(clippy::cast_possible_wrap)]
            fn read(memory: &Memory, address: u32) -> Result<Self, MemoryError> {
                memory.$read(address).map(|v| v as Self)
            }

            #[inline]
            #[allow(clippy::cast_sign_loss)]
            fn write(self, memory: &mut Memory, address: u32) -> Result<(), MemoryError> {
                memory.$write(address, self as $raw)
            }
        }
    )*};
}

guest_value! {
    u8 => read_u8, write_u8, u8;
    i8 => read_u8, write_u8, u8;
    u16 => read_u16, write_u16, u16;
    i16 => read_u16, write_u16, u16;
    u32 => read_u32, write_u32, u32;
    i32 => read_u32, write_u32, u32;
    u64 => read_u64, write_u64, u64;
    i64 => read_u64, write_u64, u64;
}

impl GuestValue for f32 {
    const SIZE: u32 = 4;

    #[inline]
    fn read(memory: &Memory, address: u32) -> Result<Self, MemoryError> {
        memory.read_f32(address)
    }

    #[inline]
    fn write(self, memory: &mut Memory, address: u32) -> Result<(), MemoryError> {
        memory.write_f32(address, self)
    }
}

impl GuestValue for f64 {
    const SIZE: u32 = 8;

    #[inline]
    fn read(memory: &Memory, address: u32) -> Result<Self, MemoryError> {
        memory.read_f64(address)
    }

    #[inline]
    fn write(self, memory: &mut Memory, address: u32) -> Result<(), MemoryError> {
        memory.write_f64(address, self)
    }
}

/// A typed effective address (`T*` on the guest side).
///
/// Unlike a host pointer it carries no lifetime: it is plain data that is only
/// dereferenced through a [`Memory`].
#[repr(transparent)]
pub struct GuestPtr<T> {
    address: u32,
    _marker: PhantomData<fn() -> T>,
}

impl<T> GuestPtr<T> {
    pub const NULL: Self = Self::new(0);

    #[inline]
    pub const fn new(address: u32) -> Self {
        Self { address, _marker: PhantomData }
    }

    #[inline]
    pub const fn address(self) -> u32 {
        self.address
    }

    #[inline]
    pub const fn is_null(self) -> bool {
        self.address == 0
    }

    /// Reinterprets the pointee type.
    #[inline]
    pub const fn cast<U>(self) -> GuestPtr<U> {
        GuestPtr::new(self.address)
    }

    /// Adds a byte offset.
    #[inline]
    pub const fn byte_add(self, bytes: u32) -> Self {
        Self::new(self.address.wrapping_add(bytes))
    }
}

impl<T: GuestValue> GuestPtr<T> {
    /// Pointer arithmetic in units of `T`.
    #[inline]
    pub const fn add(self, count: u32) -> Self {
        self.byte_add(count.wrapping_mul(T::SIZE))
    }

    /// # Errors
    /// Fails if the pointee is unmapped.
    #[inline]
    pub fn read(self, memory: &Memory) -> Result<T, MemoryError> {
        T::read(memory, self.address)
    }

    /// # Errors
    /// Fails if the pointee is unmapped.
    #[inline]
    pub fn write(self, memory: &mut Memory, value: T) -> Result<(), MemoryError> {
        value.write(memory, self.address)
    }
}

impl<T> GuestValue for GuestPtr<T> {
    const SIZE: u32 = 4;

    fn read(memory: &Memory, address: u32) -> Result<Self, MemoryError> {
        memory.read_u32(address).map(Self::new)
    }

    fn write(self, memory: &mut Memory, address: u32) -> Result<(), MemoryError> {
        memory.write_u32(address, self.address)
    }
}

impl<T> Clone for GuestPtr<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for GuestPtr<T> {}

impl<T> PartialEq for GuestPtr<T> {
    fn eq(&self, other: &Self) -> bool {
        self.address == other.address
    }
}

impl<T> Eq for GuestPtr<T> {}

impl<T> Default for GuestPtr<T> {
    fn default() -> Self {
        Self::NULL
    }
}

impl<T> fmt::Debug for GuestPtr<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GuestPtr({:#010x})", self.address)
    }
}

impl<T> From<u32> for GuestPtr<T> {
    fn from(address: u32) -> Self {
        Self::new(address)
    }
}

#[cfg(test)]
mod tests {
    use wiiu_mem_addr::Region;

    use super::*;

    #[test]
    fn typed_access_and_arithmetic() {
        let mut memory = Memory::new();
        memory.map(Region::new("T", 0x2000, 0x100)).unwrap();
        let base = GuestPtr::<i16>::new(0x2000);
        base.add(3).write(&mut memory, -2).unwrap();
        assert_eq!(memory.read_u16(0x2006).unwrap(), 0xFFFE);
        assert_eq!(base.add(3).read(&memory).unwrap(), -2);

        let pp = GuestPtr::<GuestPtr<u32>>::new(0x2010);
        pp.write(&mut memory, GuestPtr::new(0x2020)).unwrap();
        pp.read(&memory).unwrap().write(&mut memory, 7).unwrap();
        assert_eq!(memory.read_u32(0x2020).unwrap(), 7);
    }
}
