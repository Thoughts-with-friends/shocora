//! PowerPC SysV argument passing.
//!
//! Integers and pointers use r3..=r10, 64-bit integers an aligned register
//! pair (r3:r4, r5:r6, ...), floating point f1..=f8. Overflow arguments live
//! at `8(r1)` and up.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ CPU registers      │   │ hle/args.rs              │   │ typed Rust         │
//! │                    │──►│ ──────────────────────── │──►│ arguments          │
//! │                    │   │ argument passing         │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Reads a call's arguments from the PowerPC registers and stack the way the console's compiler
//! placed them. Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap, clippy::cast_sign_loss)]

use wiiu_memory::GuestPtr;

use super::{HleContext, HleError, HleResult};

/// Position in the argument list; `Copy` so varargs can resume from it.
#[derive(Debug, Clone, Copy)]
pub struct ArgReader {
    gpr: usize,
    fpr: usize,
    stack: u32,
}

impl Default for ArgReader {
    fn default() -> Self {
        Self::new()
    }
}

impl ArgReader {
    #[inline]
    pub const fn new() -> Self {
        Self { gpr: 3, fpr: 1, stack: 8 }
    }

    fn stack_word(&mut self, ctx: &HleContext<'_>) -> Result<u32, HleError> {
        let value = ctx.memory.read_u32(ctx.cpu.gpr[1].wrapping_add(self.stack))?;
        self.stack += 4;
        Ok(value)
    }

    /// # Errors
    /// Fails if a stack argument is unreadable.
    pub fn next_u32(&mut self, ctx: &HleContext<'_>) -> Result<u32, HleError> {
        if self.gpr <= 10 {
            self.gpr += 1;
            Ok(ctx.cpu.gpr[self.gpr - 1])
        } else {
            self.stack_word(ctx)
        }
    }

    /// # Errors
    /// Fails if a stack argument is unreadable.
    pub fn next_u64(&mut self, ctx: &HleContext<'_>) -> Result<u64, HleError> {
        if self.gpr.is_multiple_of(2) {
            self.gpr += 1;
        }
        if self.gpr <= 9 {
            let value =
                (u64::from(ctx.cpu.gpr[self.gpr]) << 32) | u64::from(ctx.cpu.gpr[self.gpr + 1]);
            self.gpr += 2;
            Ok(value)
        } else {
            self.gpr = 11;
            self.stack = self.stack.next_multiple_of(8);
            Ok((u64::from(self.stack_word(ctx)?) << 32) | u64::from(self.stack_word(ctx)?))
        }
    }

    /// # Errors
    /// Fails if a stack argument is unreadable.
    pub fn next_f64(&mut self, ctx: &HleContext<'_>) -> Result<f64, HleError> {
        if self.fpr <= 8 {
            self.fpr += 1;
            Ok(ctx.cpu.fpr[self.fpr - 1].ps0)
        } else {
            self.stack = self.stack.next_multiple_of(8);
            let bits = (u64::from(self.stack_word(ctx)?) << 32) | u64::from(self.stack_word(ctx)?);
            Ok(f64::from_bits(bits))
        }
    }
}

/// A Rust type readable as one guest argument.
pub trait FromArg: Sized {
    /// # Errors
    /// Fails if a stack argument is unreadable.
    fn from_arg(ctx: &HleContext<'_>, args: &mut ArgReader) -> Result<Self, HleError>;
}

macro_rules! from_u32 {
    ($($ty:ty),*) => {$(
        impl FromArg for $ty {
            fn from_arg(ctx: &HleContext<'_>, args: &mut ArgReader) -> Result<Self, HleError> {
                args.next_u32(ctx).map(|v| v as Self)
            }
        }
    )*};
}
from_u32!(u32, i32, u16, i16, u8, i8);

impl FromArg for bool {
    fn from_arg(ctx: &HleContext<'_>, args: &mut ArgReader) -> Result<Self, HleError> {
        args.next_u32(ctx).map(|v| v != 0)
    }
}

impl FromArg for u64 {
    fn from_arg(ctx: &HleContext<'_>, args: &mut ArgReader) -> Result<Self, HleError> {
        args.next_u64(ctx)
    }
}

impl FromArg for i64 {
    fn from_arg(ctx: &HleContext<'_>, args: &mut ArgReader) -> Result<Self, HleError> {
        args.next_u64(ctx).map(|v| v as Self)
    }
}

impl FromArg for f64 {
    fn from_arg(ctx: &HleContext<'_>, args: &mut ArgReader) -> Result<Self, HleError> {
        args.next_f64(ctx)
    }
}

impl FromArg for f32 {
    fn from_arg(ctx: &HleContext<'_>, args: &mut ArgReader) -> Result<Self, HleError> {
        args.next_f64(ctx).map(|v| v as Self)
    }
}

impl<T> FromArg for GuestPtr<T> {
    fn from_arg(ctx: &HleContext<'_>, args: &mut ArgReader) -> Result<Self, HleError> {
        args.next_u32(ctx).map(Self::new)
    }
}

/// The start of a `...` argument list: the reader positioned after the
/// named parameters.
#[derive(Debug, Clone, Copy)]
pub struct VaStart(pub ArgReader);

impl FromArg for VaStart {
    fn from_arg(_ctx: &HleContext<'_>, args: &mut ArgReader) -> Result<Self, HleError> {
        Ok(Self(*args))
    }
}

/// A Rust value written back as the guest return value.
pub trait IntoReturn {
    /// # Errors
    /// Propagates an `Err` returned by the HLE function.
    fn write_return(self, ctx: &mut HleContext<'_>) -> HleResult;
}

impl IntoReturn for () {
    fn write_return(self, _ctx: &mut HleContext<'_>) -> HleResult {
        Ok(())
    }
}

macro_rules! into_r3 {
    ($($ty:ty),*) => {$(
        impl IntoReturn for $ty {
            fn write_return(self, ctx: &mut HleContext<'_>) -> HleResult {
                ctx.cpu.gpr[3] = self as u32;
                Ok(())
            }
        }
    )*};
}
into_r3!(u32, i32, u16, i16, u8, i8);

impl IntoReturn for bool {
    fn write_return(self, ctx: &mut HleContext<'_>) -> HleResult {
        ctx.cpu.gpr[3] = u32::from(self);
        Ok(())
    }
}

impl IntoReturn for u64 {
    fn write_return(self, ctx: &mut HleContext<'_>) -> HleResult {
        ctx.cpu.gpr[3] = (self >> 32) as u32;
        ctx.cpu.gpr[4] = self as u32;
        Ok(())
    }
}

impl IntoReturn for i64 {
    fn write_return(self, ctx: &mut HleContext<'_>) -> HleResult {
        (self as u64).write_return(ctx)
    }
}

impl IntoReturn for f64 {
    fn write_return(self, ctx: &mut HleContext<'_>) -> HleResult {
        ctx.cpu.fpr[1].ps0 = self;
        Ok(())
    }
}

impl IntoReturn for f32 {
    fn write_return(self, ctx: &mut HleContext<'_>) -> HleResult {
        f64::from(self).write_return(ctx)
    }
}

impl<T> IntoReturn for GuestPtr<T> {
    fn write_return(self, ctx: &mut HleContext<'_>) -> HleResult {
        ctx.cpu.gpr[3] = self.address();
        Ok(())
    }
}

impl<T: IntoReturn> IntoReturn for Result<T, HleError> {
    fn write_return(self, ctx: &mut HleContext<'_>) -> HleResult {
        self?.write_return(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::test_env::TestEnv;

    #[test]
    fn reads_mixed_arguments() {
        let mut env = TestEnv::new();
        let ctx = env.ctx();
        ctx.cpu.gpr[3] = 1;
        ctx.cpu.gpr[5] = 0x1122_3344; // u64 skips r4 → r5:r6
        ctx.cpu.gpr[6] = 0x5566_7788;
        ctx.cpu.gpr[7] = 9;
        ctx.cpu.fpr[1].ps0 = 2.5;
        let mut args = ArgReader::new();
        assert_eq!(u32::from_arg(&ctx, &mut args).unwrap(), 1);
        assert_eq!(u64::from_arg(&ctx, &mut args).unwrap(), 0x1122_3344_5566_7788);
        assert!((f32::from_arg(&ctx, &mut args).unwrap() - 2.5).abs() < f32::EPSILON);
        assert_eq!(i32::from_arg(&ctx, &mut args).unwrap(), 9);
    }

    #[test]
    fn overflow_arguments_come_from_the_stack() {
        let mut env = TestEnv::new();
        let ctx = env.ctx();
        let sp = ctx.cpu.gpr[1];
        ctx.memory.write_u32(sp + 8, 0xCAFE).unwrap();
        let mut args = ArgReader::new();
        for _ in 3..=10 {
            args.next_u32(&ctx).unwrap();
        }
        assert_eq!(args.next_u32(&ctx).unwrap(), 0xCAFE);
    }

    #[test]
    fn writes_returns() {
        let mut env = TestEnv::new();
        let mut ctx = env.ctx();
        (-2_i64).write_return(&mut ctx).unwrap();
        assert_eq!((ctx.cpu.gpr[3], ctx.cpu.gpr[4]), (u32::MAX, 0xFFFF_FFFE));
        true.write_return(&mut ctx).unwrap();
        assert_eq!(ctx.cpu.gpr[3], 1);
    }
}
