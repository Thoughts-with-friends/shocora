//! Thread lifecycle exports.
#![allow(clippy::too_many_arguments)]

use wiiu_hle_macros::hle_export;
use wiiu_memory::GuestPtr;

use super::{
    Reschedule, ThreadRecord, ThreadState, Wait, block, core_from_attr, new_context, offset,
    sync_guest,
};
use crate::{
    hle::{HleContext, HleError},
    process::INTERNAL_LIBRARY,
};

/// `OS_THREAD_ATTR_DETACHED`.
const ATTR_DETACHED: u32 = 0x8;

/// Applies `edit` to a thread record and mirrors it into guest memory.
fn with_thread<R>(
    ctx: &mut HleContext<'_>,
    thread: u32,
    edit: impl FnOnce(&mut ThreadRecord) -> R,
) -> Result<Option<R>, HleError> {
    let Some(record) = ctx.os.coreinit.threads.threads.get_mut(&thread) else { return Ok(None) };
    let result = edit(record);
    sync_guest(ctx.memory, thread, record)?;
    Ok(Some(result))
}

/// Address threads return to when their entry point finishes.
fn thread_return_stub(ctx: &mut HleContext<'_>) -> Result<u32, HleError> {
    let stub = ctx.os.registry.function_stub(INTERNAL_LIBRARY, THREAD_RETURN.name);
    ctx.os.registry.commit(ctx.memory)?;
    Ok(stub)
}

fn create(
    ctx: &mut HleContext<'_>,
    thread: u32,
    entry: u32,
    argc: u32,
    argv: u32,
    stack_top: u32,
    stack_size: u32,
    priority: i32,
    attr: u32,
) -> Result<bool, HleError> {
    if thread == 0 || entry == 0 {
        return Ok(false);
    }
    if !take_over_from_other_cores(ctx, thread) {
        return Ok(false);
    }
    let lr = thread_return_stub(ctx)?;
    let core = core_from_attr(attr, ctx.cpu.core_id());
    let context = new_context(ctx.cpu, core, entry, argc, argv, stack_top, lr);
    let mut record = ThreadRecord::new(context, priority);
    record.detached = attr & ATTR_DETACHED != 0;

    // A thread created again at the same address starts with fresh TLS.
    if let Some(old) = ctx.os.coreinit.threads.threads.get(&thread).map(|r| r.context.tls_block) {
        if old != 0 {
            if let Some(heap) = ctx.os.coreinit.heaps.default_heap_mut() {
                heap.free(old);
            }
        }
    }
    ctx.memory.fill(thread, super::OS_THREAD_SIZE, 0)?;
    #[allow(clippy::cast_possible_truncation)]
    {
        ctx.memory.write_u8(thread + offset::ATTR, attr as u8)?;
        ctx.memory.write_u16(thread + offset::ID, ctx.os.coreinit.threads.threads.len() as u16)?;
    }
    ctx.memory.write_u32(thread + offset::STACK_BASE, stack_top)?;
    ctx.memory.write_u32(thread + offset::STACK_END, stack_top.wrapping_sub(stack_size))?;
    ctx.memory.write_u32(thread + offset::ENTRY, entry)?;
    ctx.memory.write_u32(record.context.gpr[1], 0)?;
    sync_guest(ctx.memory, thread, &record)?;
    ctx.os.coreinit.threads.threads.insert(thread, record);
    log::debug!(
        "OSCreateThread {thread:#010x}: entry {entry:#010x}, core {core}, priority {priority}"
    );
    Ok(true)
}

/// Side by side, the thread an earlier `OSCreateThread` started at `thread` may still be
/// another core's current one: running its last instructions, or terminated with nothing
/// else for that core to run. On the console it is long gone by then; here the core would
/// resume the new thread with the old registers (at the thread return stub) and end it at
/// once. A terminated one is taken off its core; a running one is waited for, as
/// `OSJoinThread` would. Returns `false` when the caller blocked (the call is run again).
fn take_over_from_other_cores(ctx: &mut HleContext<'_>, thread: u32) -> bool {
    let threads = &mut ctx.os.coreinit.threads;
    let me = ctx.cpu.core_id() as usize;
    let elsewhere = |cores: &[u32]| cores.iter().enumerate().any(|(c, t)| *t == thread && c != me);
    if !threads.multi_core || !elsewhere(&threads.cores) {
        return true;
    }
    if threads.threads.get(&thread).is_some_and(|t| t.state != ThreadState::Moribund) {
        block(ctx, Wait::Join(thread));
        return false;
    }
    for (core, slot) in threads.cores.iter_mut().enumerate() {
        if *slot == thread && core != me {
            *slot = 0;
        }
    }
    true
}

#[hle_export(lib = "coreinit", name = "OSCreateThread")]
fn os_create_thread(
    ctx: &mut HleContext<'_>,
    thread: u32,
    entry: u32,
    argc: u32,
    argv: u32,
    stack_top: u32,
    stack_size: u32,
    priority: i32,
    attr: u32,
) -> Result<bool, HleError> {
    create(ctx, thread, entry, argc, argv, stack_top, stack_size, priority, attr)
}

#[hle_export(lib = "coreinit", name = "OSCreateThreadType")]
fn os_create_thread_type(
    ctx: &mut HleContext<'_>,
    thread: u32,
    entry: u32,
    argc: u32,
    argv: u32,
    stack_top: u32,
    stack_size: u32,
    priority: i32,
    attr: u32,
    _thread_type: u32,
) -> Result<bool, HleError> {
    create(ctx, thread, entry, argc, argv, stack_top, stack_size, priority, attr)
}

/// Restarts a terminated thread (typically a core's default thread) at `entry`.
#[hle_export(lib = "coreinit", name = "OSRunThread")]
fn os_run_thread(
    ctx: &mut HleContext<'_>,
    thread: u32,
    entry: u32,
    argc: u32,
    argv: u32,
) -> Result<bool, HleError> {
    let lr = thread_return_stub(ctx)?;
    let parent = ctx.cpu.clone();
    let stack_top = ctx.memory.read_u32(thread + offset::STACK_BASE)?;
    let started = with_thread(ctx, thread, |record| {
        if record.state != ThreadState::Moribund {
            return false;
        }
        let core = record.context.core_id();
        record.context = new_context(&parent, core, entry, argc, argv, stack_top, lr);
        record.state = ThreadState::Ready;
        record.suspend_count = 0;
        true
    })?;
    if started == Some(true) {
        ctx.os.coreinit.threads.request(Reschedule::Priority);
    }
    Ok(started == Some(true))
}

/// Returns the previous suspend count.
#[hle_export(lib = "coreinit", name = "OSResumeThread")]
fn os_resume_thread(ctx: &mut HleContext<'_>, thread: u32) -> Result<i32, HleError> {
    let previous = with_thread(ctx, thread, |record| {
        let previous = record.suspend_count;
        if previous > 0 {
            record.suspend_count -= 1;
            if record.suspend_count == 0 && record.state == ThreadState::Suspended {
                record.state = ThreadState::Ready;
            }
        }
        previous
    })?;
    ctx.os.coreinit.threads.request(Reschedule::Priority);
    Ok(previous.unwrap_or(0))
}

#[hle_export(lib = "coreinit", name = "OSSuspendThread")]
fn os_suspend_thread(ctx: &mut HleContext<'_>, thread: u32) -> Result<i32, HleError> {
    let current = ctx.os.coreinit.threads.current;
    let previous = with_thread(ctx, thread, |record| {
        let previous = record.suspend_count;
        record.suspend_count += 1;
        if matches!(record.state, ThreadState::Ready | ThreadState::Running) {
            record.state = ThreadState::Suspended;
        }
        previous
    })?;
    if thread == current {
        ctx.os.coreinit.threads.request(Reschedule::Block);
    }
    Ok(previous.unwrap_or(0))
}

/// Waits for `thread` to terminate and returns its exit value through `exit_out`.
#[hle_export(lib = "coreinit", name = "OSJoinThread")]
fn os_join_thread(
    ctx: &mut HleContext<'_>,
    thread: u32,
    exit_out: GuestPtr<i32>,
) -> Result<bool, HleError> {
    let Some((detached, state, exit_value)) =
        ctx.os.coreinit.threads.threads.get(&thread).map(|r| (r.detached, r.state, r.exit_value))
    else {
        return Ok(false);
    };
    if detached {
        return Ok(false);
    }
    if state != ThreadState::Moribund {
        block(ctx, Wait::Join(thread));
        return Ok(false);
    }
    ctx.os.coreinit.threads.take_woke_from();
    if !exit_out.is_null() {
        exit_out.write(ctx.memory, exit_value)?;
    }
    Ok(true)
}

#[hle_export(lib = "coreinit", name = "OSDetachThread")]
fn os_detach_thread(ctx: &mut HleContext<'_>, thread: u32) -> Result<(), HleError> {
    with_thread(ctx, thread, |record| record.detached = true)?;
    Ok(())
}

fn exit_current(ctx: &mut HleContext<'_>, value: i32) -> Result<(), HleError> {
    let current = ctx.os.coreinit.threads.current;
    if current == ctx.os.coreinit.main_thread {
        log::info!("main thread exited with {value}");
        ctx.os.exit_code = Some(value);
        ctx.os.frame_done = true;
        return Ok(());
    }
    with_thread(ctx, current, |record| {
        record.state = ThreadState::Moribund;
        record.exit_value = value;
    })?;
    ctx.os.coreinit.threads.request(Reschedule::Block);
    Ok(())
}

#[hle_export(lib = "coreinit", name = "OSExitThread")]
fn os_exit_thread(ctx: &mut HleContext<'_>, value: i32) -> Result<(), HleError> {
    exit_current(ctx, value)
}

/// A thread's entry point returned (`lr` of every created thread).
#[hle_export(lib = "shocora", name = "__shocora_ThreadReturn")]
fn thread_return(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    let value = ctx.cpu.gpr[3].cast_signed();
    exit_current(ctx, value)
}

#[hle_export(lib = "coreinit", name = "OSGetCurrentThread")]
fn os_get_current_thread(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.coreinit.threads.current
}

/// `__tls_get_addr(tls_index*)`: address of a thread-local variable of the
/// current thread. Each thread's block is a copy of the module's TLS image,
/// made on first use (as Cafe OS does).
#[hle_export(lib = "coreinit", name = "__tls_get_addr")]
fn tls_get_addr(ctx: &mut HleContext<'_>, index: GuestPtr<u32>) -> Result<u32, HleError> {
    let module = ctx.memory.read_u32(index.address())?;
    let offset = ctx.memory.read_u32(index.address() + 4)?;
    let Some(image) = ctx.os.coreinit.tls_image.filter(|image| image.module_index == module) else {
        return Err(HleError::Fatal(format!("__tls_get_addr: no TLS module {module}")));
    };
    // The block travels with the running thread's registers.
    let block = ctx.cpu.tls_block;
    if block != 0 {
        return Ok(block + offset);
    }
    let size = image.size.max(4);
    let block = match ctx.os.coreinit.heaps.default_heap_mut().and_then(|h| h.alloc(size, 0x20)) {
        Some(block) => block,
        None => ctx.os.registry.allocate_data(size, 0x20),
    };
    ctx.memory.copy_within(block, image.start, image.size)?;
    ctx.cpu.tls_block = block;
    Ok(block + offset)
}

#[hle_export(lib = "coreinit", name = "OSGetDefaultThread")]
fn os_get_default_thread(ctx: &mut HleContext<'_>, core: u32) -> u32 {
    ctx.os.coreinit.default_threads.get(core as usize).copied().unwrap_or(0)
}

#[hle_export(lib = "coreinit", name = "OSGetThreadPriority")]
fn os_get_thread_priority(ctx: &mut HleContext<'_>, thread: u32) -> i32 {
    ctx.os.coreinit.threads.threads.get(&thread).map_or(0, |t| t.priority)
}

#[hle_export(lib = "coreinit", name = "OSSetThreadPriority")]
fn os_set_thread_priority(
    ctx: &mut HleContext<'_>,
    thread: u32,
    priority: i32,
) -> Result<bool, HleError> {
    if !(0..=31).contains(&priority) {
        return Ok(false);
    }
    let changed = with_thread(ctx, thread, |record| record.priority = priority)?.is_some();
    ctx.os.coreinit.threads.request(Reschedule::Priority);
    Ok(changed)
}

#[hle_export(lib = "coreinit", name = "OSSetThreadName")]
fn os_set_thread_name(ctx: &mut HleContext<'_>, thread: u32, name: u32) -> Result<(), HleError> {
    with_thread(ctx, thread, |record| record.name = name)?;
    Ok(())
}

#[hle_export(lib = "coreinit", name = "OSGetThreadName")]
fn os_get_thread_name(ctx: &mut HleContext<'_>, thread: u32) -> u32 {
    ctx.os.coreinit.threads.threads.get(&thread).map_or(0, |t| t.name)
}

#[hle_export(lib = "coreinit", name = "OSIsThreadTerminated")]
fn os_is_thread_terminated(ctx: &mut HleContext<'_>, thread: u32) -> bool {
    ctx.os.coreinit.threads.threads.get(&thread).is_none_or(|t| t.state == ThreadState::Moribund)
}

#[hle_export(lib = "coreinit", name = "OSIsThreadSuspended")]
fn os_is_thread_suspended(ctx: &mut HleContext<'_>, thread: u32) -> bool {
    ctx.os.coreinit.threads.threads.get(&thread).is_some_and(|t| t.suspend_count > 0)
}

#[hle_export(lib = "coreinit", name = "OSGetThreadSpecific")]
fn os_get_thread_specific(ctx: &mut HleContext<'_>, slot: u32) -> u32 {
    ctx.cpu.thread_specific.get(slot as usize).copied().unwrap_or(0)
}

#[hle_export(lib = "coreinit", name = "OSSetThreadSpecific")]
fn os_set_thread_specific(ctx: &mut HleContext<'_>, slot: u32, value: u32) {
    if let Some(entry) = ctx.cpu.thread_specific.get_mut(slot as usize) {
        *entry = value;
    }
}

/// Moves a thread to the first core in `mask` (takes effect when it next runs).
#[hle_export(lib = "coreinit", name = "OSSetThreadAffinity")]
fn os_set_thread_affinity(
    ctx: &mut HleContext<'_>,
    thread: u32,
    mask: u32,
) -> Result<bool, HleError> {
    #[allow(clippy::cast_possible_truncation)]
    ctx.memory.write_u8(thread + offset::ATTR, (mask & 7) as u8)?;
    let current = ctx.os.coreinit.threads.current;
    let core = core_from_attr(mask, ctx.cpu.core_id());
    if thread == current {
        ctx.cpu.spr[wiiu_cpu::state::spr::UPIR as usize] = core;
    }
    Ok(with_thread(ctx, thread, |record| {
        record.context.spr[wiiu_cpu::state::spr::UPIR as usize] = core;
    })?
    .is_some())
}

#[hle_export(lib = "coreinit", name = "OSGetThreadAffinity")]
fn os_get_thread_affinity(ctx: &mut HleContext<'_>, thread: u32) -> Result<u32, HleError> {
    Ok(u32::from(ctx.memory.read_u8(thread + offset::ATTR)?) & 7)
}

#[hle_export(lib = "coreinit", name = "OSYieldThread")]
fn os_yield_thread(ctx: &mut HleContext<'_>) {
    ctx.os.coreinit.threads.request(Reschedule::Yield);
}

#[hle_export(lib = "coreinit", name = "OSSleepTicks")]
fn os_sleep_ticks(ctx: &mut HleContext<'_>, ticks: i64) {
    if let Some(Wait::Sleep(_)) = ctx.os.coreinit.threads.take_woke_from() {
        return;
    }
    let deadline = ctx.os.timebase.ticks_since_boot().saturating_add(ticks.max(0).cast_unsigned());
    block(ctx, Wait::Sleep(deadline));
}

#[hle_export(lib = "coreinit", name = "OSGetStackPointer")]
fn os_get_stack_pointer(ctx: &mut HleContext<'_>) -> u32 {
    ctx.cpu.gpr[1]
}

#[hle_export(lib = "coreinit", name = "OSTestThreadCancel")]
fn os_test_thread_cancel(_ctx: &mut HleContext<'_>) {}

/// Returns the previous callback (never stored, so always 0).
#[hle_export(lib = "coreinit", name = "OSSetThreadCleanupCallback")]
fn os_set_thread_cleanup_callback(_ctx: &mut HleContext<'_>, _thread: u32, _callback: u32) -> u32 {
    0
}

#[hle_export(lib = "coreinit", name = "OSSetThreadDeallocator")]
fn os_set_thread_deallocator(_ctx: &mut HleContext<'_>, _thread: u32, _deallocator: u32) -> u32 {
    0
}

#[hle_export(lib = "coreinit", name = "OSCheckActiveThreads")]
fn os_check_active_threads(ctx: &mut HleContext<'_>) -> u32 {
    let threads = &ctx.os.coreinit.threads.threads;
    #[allow(clippy::cast_possible_truncation)]
    let active = threads.values().filter(|t| t.state != ThreadState::Moribund).count() as u32;
    active
}

/// Returns the previous enable state.
#[hle_export(lib = "coreinit", name = "OSDisableInterrupts")]
fn os_disable_interrupts(ctx: &mut HleContext<'_>) -> bool {
    std::mem::replace(&mut ctx.os.coreinit.interrupts_enabled, false)
}

#[hle_export(lib = "coreinit", name = "OSRestoreInterrupts")]
fn os_restore_interrupts(ctx: &mut HleContext<'_>, enable: bool) -> bool {
    std::mem::replace(&mut ctx.os.coreinit.interrupts_enabled, enable)
}

#[hle_export(lib = "coreinit", name = "OSIsInterruptEnabled")]
fn os_is_interrupt_enabled(ctx: &mut HleContext<'_>) -> bool {
    ctx.os.coreinit.interrupts_enabled
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;
