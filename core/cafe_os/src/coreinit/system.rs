//! Small coreinit services: barriers, MMIO registers, the C runtime's locks,
//! fibers, `va_list` printing and thread queues.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ small system calls │   │ coreinit/system.rs       │   │ results            │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ misc. coreinit services  │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! A grab bag of small services: barriers, hardware registers, C runtime locks and similar.
//! Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
use wiiu_hle_macros::hle_export;
use wiiu_memory::GuestPtr;

use super::{
    printf::{self, VarArgs},
    thread::{Reschedule, ThreadState, Wait, block, sync},
};
use crate::{
    hle::{HleContext, HleError},
    process::INTERNAL_LIBRARY,
};

/// Longest guest string read for formatting.
const MAX_STRING: u32 = 0x1000;

/// `OSEnableInterrupts`: returns the previous state, like `OSDisableInterrupts`.
#[hle_export(lib = "coreinit", name = "OSEnableInterrupts")]
fn os_enable_interrupts(ctx: &mut HleContext<'_>) -> bool {
    std::mem::replace(&mut ctx.os.coreinit.interrupts_enabled, true)
}

#[hle_export(lib = "coreinit", name = "OSIsAddressRangeDCValid")]
fn os_is_address_range_dc_valid(ctx: &mut HleContext<'_>, address: u32, size: u32) -> bool {
    ctx.memory.is_mapped(address, size)
}

/// The debugger never runs; the guest reads the shutdown reason as "none".
#[hle_export(lib = "coreinit", name = "OSGetCurrentContext")]
fn os_get_current_context(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.coreinit.threads.current
}

// ---- Thread queues ---------------------------------------------------------

/// `OSSleepThread`: restarts once `OSWakeupThread` has made the thread ready.
#[hle_export(lib = "coreinit", name = "OSSleepThread")]
fn os_sleep_thread(ctx: &mut HleContext<'_>, queue: u32) {
    let threads = &mut ctx.os.coreinit.threads;
    let woken = threads.current_mut().is_some_and(|t| t.woken_from_queue == Some(queue));
    if woken {
        if let Some(thread) = threads.current_mut() {
            thread.woken_from_queue = None;
        }
    } else {
        block(ctx, Wait::Cond(queue));
    }
}

#[hle_export(lib = "coreinit", name = "OSWakeupThread")]
fn os_wakeup_thread(ctx: &mut HleContext<'_>, queue: u32) {
    let threads = &mut ctx.os.coreinit.threads;
    for thread in threads.threads.values_mut() {
        if thread.state == ThreadState::Waiting(Wait::Cond(queue)) && thread.pending_cond.is_none()
        {
            thread.state = ThreadState::Ready;
            thread.woken_from_queue = Some(queue);
        }
    }
    threads.request(Reschedule::Priority);
}

/// Runs a suspended thread regardless of how many times it was suspended.
#[hle_export(lib = "coreinit", name = "OSContinueThread")]
fn os_continue_thread(ctx: &mut HleContext<'_>, thread: u32) {
    if let Some(record) = ctx.os.coreinit.threads.threads.get_mut(&thread) {
        record.suspend_count = 0;
        if record.state == ThreadState::Suspended {
            record.state = ThreadState::Ready;
        }
    }
    ctx.os.coreinit.threads.request(Reschedule::Priority);
}

// ---- Green Hills runtime locks ---------------------------------------------

/// The guest mutex slot itself serves as the lock's key.
#[hle_export(lib = "coreinit", name = "__ghs_mtx_init")]
fn ghs_mtx_init(ctx: &mut HleContext<'_>, mutex: GuestPtr<u32>) -> Result<(), HleError> {
    mutex.write(ctx.memory, mutex.address())?;
    ctx.os.coreinit.threads.mutexes.remove(&mutex.address());
    Ok(())
}

#[hle_export(lib = "coreinit", name = "__ghs_mtx_dst")]
fn ghs_mtx_dst(ctx: &mut HleContext<'_>, mutex: u32) {
    ctx.os.coreinit.threads.mutexes.remove(&mutex);
}

#[hle_export(lib = "coreinit", name = "__ghs_mtx_lock")]
fn ghs_mtx_lock(ctx: &mut HleContext<'_>, mutex: u32) {
    sync::lock(ctx, mutex);
}

#[hle_export(lib = "coreinit", name = "__ghs_mtx_unlock")]
fn ghs_mtx_unlock(ctx: &mut HleContext<'_>, mutex: u32) {
    sync::unlock(ctx, mutex);
}

/// Per-`FILE` locks: one slot suffices, `stdio` is never contended here.
#[hle_export(lib = "coreinit", name = "__ghs_flock_ptr")]
fn ghs_flock_ptr(ctx: &mut HleContext<'_>, _iob: u32) -> u32 {
    if ctx.os.coreinit.ghs_flock == 0 {
        ctx.os.coreinit.ghs_flock = ctx.os.registry.allocate_data(4, 4);
    }
    ctx.os.coreinit.ghs_flock
}

/// `errno` is per-thread on hardware; only the C runtime reads it back.
#[hle_export(lib = "coreinit", name = "__gh_set_errno")]
fn gh_set_errno(ctx: &mut HleContext<'_>, value: u32) {
    ctx.os.coreinit.errno = value;
}

#[hle_export(lib = "coreinit", name = "__gh_get_errno")]
fn gh_get_errno(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.coreinit.errno
}

// ---- Fibers ----------------------------------------------------------------

/// `OSSwitchFiberEx`: calls `function` on `stack`; returning switches back.
#[hle_export(lib = "coreinit", name = "OSSwitchFiberEx")]
fn os_switch_fiber_ex(
    ctx: &mut HleContext<'_>,
    arg0: u32,
    arg1: u32,
    arg2: u32,
    arg3: u32,
    function: u32,
    stack: u32,
) -> Result<(), HleError> {
    let stub = ctx.os.registry.function_stub(INTERNAL_LIBRARY, FIBER_RETURN.name);
    ctx.os.registry.commit(ctx.memory)?;
    let me = ctx.os.coreinit.threads.current;
    ctx.os.coreinit.fibers.entry(me).or_default().push((ctx.cpu.gpr[1], ctx.cpu.lr));
    ctx.cpu.gpr[1] = (stack & !0xF).wrapping_sub(0x10);
    ctx.memory.write_u32(ctx.cpu.gpr[1], 0)?;
    ctx.cpu.gpr[3] = arg0;
    ctx.cpu.gpr[4] = arg1;
    ctx.cpu.gpr[5] = arg2;
    ctx.cpu.gpr[6] = arg3;
    ctx.cpu.lr = stub;
    ctx.cpu.pc = function;
    Ok(())
}

#[hle_export(lib = "coreinit", name = "OSSwitchFiber")]
fn os_switch_fiber(ctx: &mut HleContext<'_>, function: u32, stack: u32) -> Result<(), HleError> {
    os_switch_fiber_ex(ctx, 0, 0, 0, 0, function, stack)
}

/// The fiber's function returned: back to the caller of `OSSwitchFiberEx`.
#[hle_export(lib = "shocora", name = "__shocora_FiberReturn")]
fn fiber_return(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    let me = ctx.os.coreinit.threads.current;
    let (stack, lr) = ctx
        .os
        .coreinit
        .fibers
        .get_mut(&me)
        .and_then(Vec::pop)
        .ok_or_else(|| HleError::Fatal("fiber returned with no caller".into()))?;
    ctx.cpu.gpr[1] = stack;
    ctx.cpu.lr = lr;
    Ok(())
}

// ---- va_list printing ------------------------------------------------------

/// A guest `va_list` (SysV PowerPC): register save area plus overflow area.
struct VaList<'c, 'a> {
    ctx: &'c HleContext<'a>,
    gpr: u32,
    fpr: u32,
    overflow: u32,
    save_area: u32,
}

impl VarArgs for VaList<'_, '_> {
    fn next_int(&mut self) -> Result<u32, HleError> {
        if self.gpr < 8 {
            self.gpr += 1;
            return Ok(self.ctx.memory.read_u32(self.save_area + 4 * (self.gpr - 1))?);
        }
        self.overflow += 4;
        Ok(self.ctx.memory.read_u32(self.overflow - 4)?)
    }

    fn next_long(&mut self) -> Result<u64, HleError> {
        if self.gpr % 2 == 1 {
            self.gpr += 1;
        }
        if self.gpr < 7 {
            self.gpr += 2;
            let high = self.ctx.memory.read_u32(self.save_area + 4 * (self.gpr - 2))?;
            let low = self.ctx.memory.read_u32(self.save_area + 4 * (self.gpr - 1))?;
            return Ok((u64::from(high) << 32) | u64::from(low));
        }
        self.overflow = self.overflow.next_multiple_of(8) + 8;
        Ok(self.ctx.memory.read_u64(self.overflow - 8)?)
    }

    fn next_double(&mut self) -> Result<f64, HleError> {
        if self.fpr < 8 {
            self.fpr += 1;
            return Ok(self.ctx.memory.read_f64(self.save_area + 8 * 4 + 8 * (self.fpr - 1))?);
        }
        self.overflow = self.overflow.next_multiple_of(8) + 8;
        Ok(self.ctx.memory.read_f64(self.overflow - 8)?)
    }

    fn string_at(&mut self, address: u32) -> Result<String, HleError> {
        Ok(self.ctx.memory.read_c_string(address, MAX_STRING)?)
    }
}

fn format_va_list(ctx: &HleContext<'_>, format: u32, list: u32) -> Result<String, HleError> {
    let text = ctx.memory.read_c_string(format, MAX_STRING)?;
    let mut args = VaList {
        ctx,
        gpr: u32::from(ctx.memory.read_u8(list)?),
        fpr: u32::from(ctx.memory.read_u8(list + 1)?),
        overflow: ctx.memory.read_u32(list + 4)?,
        save_area: ctx.memory.read_u32(list + 8)?,
    };
    printf::format(&text, &mut args)
}

#[hle_export(lib = "coreinit", name = "OSVReport")]
fn os_v_report(ctx: &mut HleContext<'_>, format: u32, list: u32) -> Result<(), HleError> {
    let text = format_va_list(ctx, format, list)?;
    ctx.os.print(text);
    Ok(())
}

#[hle_export(lib = "coreinit", name = "COSWarn")]
fn cos_warn(
    ctx: &mut HleContext<'_>,
    _module: u32,
    format: u32,
    va: crate::hle::VaStart,
) -> Result<(), HleError> {
    struct Args<'c, 'a>(crate::hle::ArgReader, &'c HleContext<'a>);
    impl VarArgs for Args<'_, '_> {
        fn next_int(&mut self) -> Result<u32, HleError> {
            self.0.next_u32(self.1)
        }
        fn next_long(&mut self) -> Result<u64, HleError> {
            self.0.next_u64(self.1)
        }
        fn next_double(&mut self) -> Result<f64, HleError> {
            self.0.next_f64(self.1)
        }
        fn string_at(&mut self, address: u32) -> Result<String, HleError> {
            Ok(self.1.memory.read_c_string(address, MAX_STRING)?)
        }
    }
    let text = ctx.memory.read_c_string(format, MAX_STRING)?;
    let text = printf::format(&text, &mut Args(va.0, ctx))?;
    ctx.os.print(text);
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn v_report_reads_the_save_area() {
        let mut env = TestEnv::new();
        let (list, save, format) = (SCRATCH, SCRATCH + 0x100, SCRATCH + 0x200);
        env.memory.write_u8(list, 1).unwrap();
        env.memory.write_u32(list + 8, save).unwrap();
        env.memory.write_u32(save + 4, 42).unwrap();
        env.memory.write_c_string(format, "x=%d").unwrap();
        env.call("coreinit", "OSVReport", &[format, list]);
        assert_eq!(env.os.console, ["x=42"]);
    }

    #[test]
    fn va_list_overflows_to_the_stack_area() {
        let mut env = TestEnv::new();
        let (list, overflow, format) = (SCRATCH, SCRATCH + 0x100, SCRATCH + 0x200);
        env.memory.write_u8(list, 8).unwrap();
        env.memory.write_u32(list + 4, overflow).unwrap();
        env.memory.write_u32(overflow, 7).unwrap();
        env.memory.write_c_string(format, "%d").unwrap();
        env.call("coreinit", "OSVReport", &[format, list]);
        assert_eq!(env.os.console, ["7"]);
    }
}
