//! Debug output: `OSReport`, `OSPanic`, `OSConsoleWrite`, `__os_snprintf`.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ game debug prints  │   │ coreinit/report.rs       │   │ shocora log        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ debug output             │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Messages the game prints for its developers end up in shocora's log. Terms such as HLE, JIT
//! or shader are explained in `docs/guide/wiiu_term.md`.
use wiiu_hle_macros::hle_export;
use wiiu_memory::GuestPtr;

use super::printf::{self, VarArgs};
use crate::hle::{ArgReader, HleContext, HleError, VaStart};

/// Longest guest string read for formatting.
const MAX_STRING: u32 = 0x1000;

/// Variadic arguments read from the calling core.
struct GuestVarArgs<'c, 'a> {
    reader: ArgReader,
    ctx: &'c HleContext<'a>,
}

impl VarArgs for GuestVarArgs<'_, '_> {
    fn next_int(&mut self) -> Result<u32, HleError> {
        self.reader.next_u32(self.ctx)
    }

    fn next_long(&mut self) -> Result<u64, HleError> {
        self.reader.next_u64(self.ctx)
    }

    fn next_double(&mut self) -> Result<f64, HleError> {
        self.reader.next_f64(self.ctx)
    }

    fn string_at(&mut self, address: u32) -> Result<String, HleError> {
        Ok(self.ctx.memory.read_c_string(address, MAX_STRING)?)
    }
}

fn format_guest(
    ctx: &HleContext<'_>,
    format: GuestPtr<u8>,
    va: VaStart,
) -> Result<String, HleError> {
    let text = ctx.memory.read_c_string(format.address(), MAX_STRING)?;
    printf::format(&text, &mut GuestVarArgs { reader: va.0, ctx })
}

#[hle_export(lib = "coreinit", name = "OSReport")]
fn os_report(ctx: &mut HleContext<'_>, format: GuestPtr<u8>, va: VaStart) -> Result<(), HleError> {
    let text = format_guest(ctx, format, va)?;
    ctx.os.print(text);
    Ok(())
}

#[hle_export(lib = "coreinit", name = "OSPanic")]
fn os_panic(
    ctx: &mut HleContext<'_>,
    file: GuestPtr<u8>,
    line: u32,
    format: GuestPtr<u8>,
    va: VaStart,
) -> Result<(), HleError> {
    let file = ctx.memory.read_c_string(file.address(), MAX_STRING)?;
    let message = format_guest(ctx, format, va)?;
    Err(HleError::Fatal(format!("OSPanic at {file}:{line}: {message}")))
}

#[hle_export(lib = "coreinit", name = "OSConsoleWrite")]
fn os_console_write(
    ctx: &mut HleContext<'_>,
    text: GuestPtr<u8>,
    size: u32,
) -> Result<(), HleError> {
    let mut bytes = vec![0; size.min(MAX_STRING) as usize];
    ctx.memory.read_bytes(text.address(), &mut bytes)?;
    ctx.os.print(String::from_utf8_lossy(&bytes).into_owned());
    Ok(())
}

/// Returns the length the full output would have, like `snprintf`.
#[hle_export(lib = "coreinit", name = "__os_snprintf")]
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
fn os_snprintf(
    ctx: &mut HleContext<'_>,
    buffer: GuestPtr<u8>,
    size: u32,
    format: GuestPtr<u8>,
    va: VaStart,
) -> Result<i32, HleError> {
    let text = format_guest(ctx, format, va)?;
    if size > 0 {
        let written = (text.len() as u32).min(size - 1);
        ctx.memory.write_bytes(buffer.address(), &text.as_bytes()[..written as usize])?;
        ctx.memory.write_u8(buffer.address() + written, 0)?;
    }
    Ok(text.len() as i32)
}

#[cfg(test)]
mod tests {
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn report_formats_register_and_float_arguments() {
        let mut env = TestEnv::new();
        env.memory.write_c_string(SCRATCH, "x=%d s=%s f=%.1f").unwrap();
        env.memory.write_c_string(SCRATCH + 0x100, "ok").unwrap();
        env.cpu.fpr[1].ps0 = 2.25;
        env.call("coreinit", "OSReport", &[SCRATCH, 42, SCRATCH + 0x100]);
        assert_eq!(env.os.console, ["x=42 s=ok f=2.2"]);
    }

    #[test]
    fn snprintf_truncates() {
        let mut env = TestEnv::new();
        env.memory.write_c_string(SCRATCH, "%05d").unwrap();
        let len = env.call("coreinit", "__os_snprintf", &[SCRATCH + 0x40, 4, SCRATCH, 7]);
        assert_eq!(len, 5);
        assert_eq!(env.memory.read_c_string(SCRATCH + 0x40, 16).unwrap(), "000");
    }
}
