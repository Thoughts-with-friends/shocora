//! Command submission, display lists, timestamps, flips and events (port of
//! Cemu's GX2_Command, GX2_Event and the swap logic of GX2.cpp).
//!
//! A flush hands the commands to the GPU, which may run on a thread of its own: a timestamp
//! is retired once the GPU has executed its command buffer, and the CPU waits for that only
//! where a title asks to (`GX2DrawDone`, `GX2WaitTimeStamp`).
#![allow(clippy::cast_possible_truncation)]

use wiiu_gpu::command::{opcode, pm4::type3_header};
use wiiu_hle_macros::hle_export;

use super::{flush, physical};
use crate::{
    coreinit::thread::{Reschedule, ThreadState, Wait, block},
    hle::{HleContext, HleError, HleExport, callback::GuestCall},
};

/// `GX2CallbackEventType`.
pub mod event {
    pub const TIMESTAMP_TOP: u32 = 0;
    pub const TIMESTAMP_BOTTOM: u32 = 1;
    pub const VSYNC: u32 = 2;
    pub const FLIP: u32 = 3;
    pub const COUNT: usize = 5;
}

/// Thread queues (not guest addresses) of the threads waiting for an event.
const VSYNC_QUEUE: u32 = 0xFFFF_FE00;
const FLIP_QUEUE: u32 = 0xFFFF_FE04;
/// `GX2SwapScanBuffers` waits once the guest is this many flips ahead.
const MAX_PENDING_FLIPS: u32 = 5;

fn sleep_on(ctx: &mut HleContext<'_>, queue: u32) -> bool {
    let woken =
        ctx.os.coreinit.threads.current_mut().is_some_and(|t| t.woken_from_queue == Some(queue));
    if woken {
        if let Some(thread) = ctx.os.coreinit.threads.current_mut() {
            thread.woken_from_queue = None;
        }
        return false;
    }
    block(ctx, Wait::Cond(queue));
    true
}

fn wake(os: &mut crate::CafeOs, queue: u32) {
    for thread in os.coreinit.threads.threads.values_mut() {
        if thread.state == ThreadState::Waiting(Wait::Cond(queue)) {
            thread.state = ThreadState::Ready;
            thread.woken_from_queue = Some(queue);
        }
    }
    os.coreinit.threads.request(Reschedule::Priority);
}

/// Waits until the GPU has executed command buffer `timestamp`. The thread blocks, so the
/// other threads and cores go on meanwhile; once the GPU is there, the HLE call starts over,
/// and this returns `false`. Returns whether the thread blocked.
fn wait_for_gpu(ctx: &mut HleContext<'_>, timestamp: u64) -> bool {
    if ctx
        .os
        .coreinit
        .threads
        .current_mut()
        .is_some_and(|t| matches!(t.woke_from, Some(Wait::Gpu(_))))
    {
        ctx.os.coreinit.threads.take_woke_from();
    }
    if ctx.gpu.retired_timestamp() >= timestamp {
        return false;
    }
    // A callback borrows the core of a thread it must not block.
    if ctx.os.callbacks.active() || ctx.os.coreinit.threads.current_mut().is_none() {
        ctx.gpu.wait_timestamp(timestamp);
        return false;
    }
    block(ctx, Wait::Gpu(timestamp));
    true
}

fn notify_event(os: &mut crate::CafeOs, kind: u32) {
    let (function, user_data) =
        os.gx2.event_callbacks.get(kind as usize).copied().unwrap_or((0, 0));
    if function != 0 {
        os.callbacks.push(GuestCall { function, args: vec![kind, user_data] });
    }
}

impl super::Gx2State {
    /// Vertical blanks per flip.
    pub(crate) fn effective_swap_interval(&self) -> u32 {
        self.forced_swap_interval.unwrap_or_else(|| self.swap_interval.max(self.min_swap_interval))
    }
}

impl crate::CafeOs {
    /// The display's vertical blank: wakes waiters and raises events. One pending
    /// flip is shown once the swap interval has passed: a title swapping faster
    /// fills the queue and blocks in `GX2SwapScanBuffers`, which is what paces it.
    pub fn vsync(&mut self) {
        let now = self.timebase.ticks_since_epoch();
        self.gx2.vsync_count += 1;
        self.gx2.last_vsync_time = now;
        notify_event(self, event::VSYNC);
        wake(self, VSYNC_QUEUE);
        let interval = u64::from(self.gx2.effective_swap_interval().max(1));
        if self.gx2.flip_requests != self.gx2.flips_executed
            && self.gx2.vsync_count - self.gx2.last_flip_vsync >= interval
        {
            self.gx2.flips_executed = self.gx2.flips_executed.wrapping_add(1);
            self.gx2.last_flip_vsync = self.gx2.vsync_count;
            self.gx2.last_flip_time = now;
            notify_event(self, event::FLIP);
            wake(self, FLIP_QUEUE);
        }
    }
}

#[hle_export(lib = "gx2", name = "GX2Flush")]
fn gx2_flush(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    flush(ctx)
}

#[hle_export(lib = "gx2", name = "GX2DrawDone")]
fn gx2_draw_done(ctx: &mut HleContext<'_>) -> Result<bool, HleError> {
    flush(ctx)?;
    wait_for_gpu(ctx, ctx.gpu.submitted_timestamp());
    Ok(true)
}

#[hle_export(lib = "gx2", name = "GX2WaitForVsync")]
fn gx2_wait_for_vsync(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    flush(ctx)?;
    sleep_on(ctx, VSYNC_QUEUE);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2WaitForFlip")]
fn gx2_wait_for_flip(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    flush(ctx)?;
    if ctx.os.gx2.flip_requests != ctx.os.gx2.flips_executed {
        sleep_on(ctx, FLIP_QUEUE);
    }
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2WaitForFreeScanBuffer")]
fn gx2_wait_for_free_scan_buffer(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    gx2_wait_for_flip(ctx)
}

#[hle_export(lib = "gx2", name = "GX2SwapScanBuffers")]
fn gx2_swap_scan_buffers(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    let restarted = ctx
        .os
        .coreinit
        .threads
        .current_mut()
        .is_some_and(|t| t.woken_from_queue == Some(FLIP_QUEUE));
    if restarted {
        if let Some(thread) = ctx.os.coreinit.threads.current_mut() {
            thread.woken_from_queue = None;
        }
    } else {
        // A GPU thread frames behind holds the title back here, as a full command ring does.
        if let Some(timestamp) = ctx.gpu.frame_wait() {
            flush(ctx)?;
            if wait_for_gpu(ctx, timestamp) {
                return Ok(());
            }
        }
        ctx.os.gx2.flip_requests += 1;
        ctx.os.gx2.emit(opcode::HLE_SWAP_SCAN_BUFFERS, &[]);
        flush(ctx)?;
        ctx.gpu.end_frame();
    }
    if ctx.os.gx2.flip_requests.wrapping_sub(ctx.os.gx2.flips_executed) > MAX_PENDING_FLIPS {
        block(ctx, Wait::Cond(FLIP_QUEUE));
    }
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2GetSwapStatus")]
fn gx2_get_swap_status(
    ctx: &mut HleContext<'_>,
    requests: u32,
    executed: u32,
    last_flip: u32,
    last_vsync: u32,
) -> Result<(), HleError> {
    ctx.memory.write_u32(requests, ctx.os.gx2.flip_requests)?;
    ctx.memory.write_u32(executed, ctx.os.gx2.flips_executed)?;
    ctx.memory.write_u64(last_flip, ctx.os.gx2.last_flip_time)?;
    ctx.memory.write_u64(last_vsync, ctx.os.gx2.last_vsync_time)?;
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetSwapInterval")]
fn gx2_set_swap_interval(ctx: &mut HleContext<'_>, interval: u32) {
    if interval < 20 {
        ctx.os.gx2.swap_interval = interval;
    }
}

#[hle_export(lib = "gx2", name = "GX2GetSwapInterval")]
fn gx2_get_swap_interval(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.gx2.effective_swap_interval()
}

#[hle_export(lib = "gx2", name = "GX2SetEventCallback")]
fn gx2_set_event_callback(ctx: &mut HleContext<'_>, kind: u32, function: u32, user_data: u32) {
    if let Some(slot) = ctx.os.gx2.event_callbacks.get_mut(kind as usize) {
        *slot = (function, user_data);
    }
}

#[hle_export(lib = "gx2", name = "GX2GetEventCallback")]
fn gx2_get_event_callback(
    ctx: &mut HleContext<'_>,
    kind: u32,
    function: u32,
    user_data: u32,
) -> Result<(), HleError> {
    let (f, u) = ctx.os.gx2.event_callbacks.get(kind as usize).copied().unwrap_or((0, 0));
    if function != 0 {
        ctx.memory.write_u32(function, f)?;
    }
    if user_data != 0 {
        ctx.memory.write_u32(user_data, u)?;
    }
    Ok(())
}

// ---- Timestamps ------------------------------------------------------------

#[hle_export(lib = "gx2", name = "GX2GetLastSubmittedTimeStamp")]
fn gx2_get_last_submitted_time_stamp(ctx: &mut HleContext<'_>) -> u64 {
    ctx.os.gx2.timestamp
}

/// GX2 counts the command buffers it flushed, the GPU those it was given: the same ones,
/// but the GPU's count may start elsewhere (a saved state of another build).
fn gpu_timestamp(ctx: &HleContext<'_>, time_stamp: u64) -> u64 {
    let offset = ctx.gpu.submitted_timestamp().wrapping_sub(ctx.os.gx2.timestamp);
    time_stamp.wrapping_add(offset)
}

#[hle_export(lib = "gx2", name = "GX2GetRetiredTimeStamp")]
fn gx2_get_retired_time_stamp(ctx: &mut HleContext<'_>) -> Result<u64, HleError> {
    flush(ctx)?;
    let behind = ctx.gpu.submitted_timestamp().saturating_sub(ctx.gpu.retired_timestamp());
    Ok(ctx.os.gx2.timestamp.saturating_sub(behind))
}

#[hle_export(lib = "gx2", name = "GX2WaitTimeStamp")]
fn gx2_wait_time_stamp(ctx: &mut HleContext<'_>, time_stamp: u64) -> Result<bool, HleError> {
    flush(ctx)?;
    wait_for_gpu(ctx, gpu_timestamp(ctx, time_stamp));
    Ok(true)
}

/// Value written when the command reaches the pipe; flushed immediately, so
/// polling guests make progress. The GPU writes it after the commands before it, which a
/// title may be waiting for.
#[hle_export(lib = "gx2", name = "GX2SubmitUserTimeStamp")]
fn gx2_submit_user_time_stamp(
    ctx: &mut HleContext<'_>,
    out: u32,
    value: u64,
    kind: u32,
    interrupt: u32,
) -> Result<(), HleError> {
    // MEM_WRITE stores the low dword first; titles compare the words in that order.
    let (low, high) = (value as u32, (value >> 32) as u32);
    if let Some(at) = wiiu_mem_addr::effective_to_physical(out) {
        ctx.os.gx2.emit(opcode::MEM_WRITE, &[at, 0, low, high]);
        flush(ctx)?;
    } else {
        flush(ctx)?;
        ctx.gpu.wait_idle();
        ctx.memory.write_u32(out, low)?;
        ctx.memory.write_u32(out + 4, high)?;
    }
    if interrupt != 0 {
        let kind = if kind == 0 { event::TIMESTAMP_TOP } else { event::TIMESTAMP_BOTTOM };
        notify_event(ctx.os, kind);
    }
    Ok(())
}

/// GPU and CPU share one clock here.
#[hle_export(lib = "gx2", name = "GX2GPUTimeToCPUTime")]
const fn gx2_gpu_time_to_cpu_time(_ctx: &mut HleContext<'_>, time: u64) -> u64 {
    time
}

#[hle_export(lib = "gx2", name = "GX2SampleTopGPUCycle")]
fn gx2_sample_top_gpu_cycle(ctx: &mut HleContext<'_>, out: u32) -> Result<(), HleError> {
    let time = ctx.os.timebase.ticks_since_boot();
    Ok(ctx.memory.write_u64(out, time)?)
}

/// Like Cemu: never report a valid value, or titles start scaling their
/// resolution dynamically.
#[hle_export(lib = "gx2", name = "GX2SampleBottomGPUCycle")]
fn gx2_sample_bottom_gpu_cycle(ctx: &mut HleContext<'_>, out: u32) -> Result<(), HleError> {
    Ok(ctx.memory.write_u64(out, u64::MAX)?)
}

// ---- Display lists ---------------------------------------------------------

#[hle_export(lib = "gx2", name = "GX2BeginDisplayList")]
fn gx2_begin_display_list(ctx: &mut HleContext<'_>, address: u32, size: u32) {
    ctx.os.gx2.begin_display_list(address, size);
}

#[hle_export(lib = "gx2", name = "GX2BeginDisplayListEx")]
fn gx2_begin_display_list_ex(ctx: &mut HleContext<'_>, address: u32, size: u32, _profiling: u32) {
    ctx.os.gx2.begin_display_list(address, size);
}

/// Copies the recorded words into the guest buffer and returns their size.
#[hle_export(lib = "gx2", name = "GX2EndDisplayList")]
fn gx2_end_display_list(ctx: &mut HleContext<'_>, _address: u32) -> Result<u32, HleError> {
    let Some(list) = ctx.os.gx2.end_display_list() else {
        log::warn!("GX2EndDisplayList without a matching begin");
        return Ok(0);
    };
    let bytes = 4 * list.words.len() as u32;
    if bytes > list.capacity {
        log::warn!("display list overflow: {bytes:#x} bytes in a {:#x}-byte buffer", list.capacity);
    }
    let count = list.words.len().min(list.capacity as usize / 4);
    for (i, word) in list.words[..count].iter().enumerate() {
        ctx.memory.write_u32(list.address + 4 * i as u32, *word)?;
    }
    Ok(count as u32 * 4)
}

#[hle_export(lib = "gx2", name = "GX2GetDisplayListWriteStatus")]
fn gx2_get_display_list_write_status(ctx: &mut HleContext<'_>) -> bool {
    ctx.os.gx2.recording()
}

#[hle_export(lib = "gx2", name = "GX2GetCurrentDisplayList")]
fn gx2_get_current_display_list(
    ctx: &mut HleContext<'_>,
    address: u32,
    size: u32,
) -> Result<bool, HleError> {
    let Some((list, capacity)) = ctx.os.gx2.current_display_list() else { return Ok(false) };
    if address != 0 {
        ctx.memory.write_u32(address, list)?;
    }
    if size != 0 {
        ctx.memory.write_u32(size, capacity)?;
    }
    Ok(true)
}

#[hle_export(lib = "gx2", name = "GX2CallDisplayList")]
fn gx2_call_display_list(ctx: &mut HleContext<'_>, address: u32, size: u32) {
    ctx.os.gx2.emit(opcode::INDIRECT_BUFFER, &[physical(address), 0, size / 4]);
}

/// Bypasses the write gatherer: earlier commands must reach the GPU first.
#[hle_export(lib = "gx2", name = "GX2DirectCallDisplayList")]
fn gx2_direct_call_display_list(
    ctx: &mut HleContext<'_>,
    address: u32,
    size: u32,
) -> Result<(), HleError> {
    ctx.os.gx2.emit(opcode::INDIRECT_BUFFER, &[physical(address), 0, size / 4]);
    if !ctx.os.gx2.recording() {
        flush(ctx)?;
    }
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2CopyDisplayList")]
fn gx2_copy_display_list(
    ctx: &mut HleContext<'_>,
    address: u32,
    size: u32,
) -> Result<(), HleError> {
    for i in 0..size / 4 {
        let word = ctx.memory.read_u32(address + 4 * i)?;
        ctx.os.gx2.push_raw(word);
    }
    Ok(())
}

const PATCH_FETCH_SHADER: u32 = 1;
const PATCH_VERTEX_SHADER: u32 = 2;
const PATCH_PIXEL_SHADER: u32 = 5;

/// Redirects a recorded `SQ_PGM_START_*` to the shader's current program.
#[hle_export(lib = "gx2", name = "GX2PatchDisplayList")]
fn gx2_patch_display_list(
    ctx: &mut HleContext<'_>,
    list: u32,
    kind: u32,
    offset: u32,
    object: u32,
) -> Result<(), HleError> {
    let program = match kind {
        PATCH_FETCH_SHADER => ctx.memory.read_u32(object + 0x0C)?,
        PATCH_VERTEX_SHADER => match ctx.memory.read_u32(object + 0xD4)? {
            0 => ctx.memory.read_u32(object + 0x124 + 0x0C)?,
            address => address,
        },
        PATCH_PIXEL_SHADER => match ctx.memory.read_u32(object + 0xA8)? {
            0 => ctx.memory.read_u32(object + 0xD8 + 0x0C)?,
            address => address,
        },
        other => {
            log::warn!("GX2PatchDisplayList: unsupported patch type {other}");
            return Ok(());
        }
    };
    Ok(ctx.memory.write_u32(list + (offset & !3) + 8, physical(program) >> 8)?)
}

// ---- Context states --------------------------------------------------------

#[hle_export(lib = "gx2", name = "GX2SetDefaultState")]
fn gx2_set_default_state(ctx: &mut HleContext<'_>) {
    super::state::set_default_state(&mut ctx.os.gx2);
}

/// `GX2ContextState::loadDL_size`.
const CONTEXT_LOAD_LIST_SIZE: u32 = 0x9804;
/// `GX2ContextState::loadDL_buffer`.
const CONTEXT_LOAD_LIST: u32 = 0x9E00;

/// A context starts from the default state; the GPU keeps its copy of the registers
/// host-side, so the guest structure only holds the display list that restores it.
#[hle_export(lib = "gx2", name = "GX2SetupContextStateEx")]
fn gx2_setup_context_state_ex(
    ctx: &mut HleContext<'_>,
    state: u32,
    _profiling: u32,
) -> Result<(), HleError> {
    super::state::set_default_state(&mut ctx.os.gx2);
    ctx.os.gx2.emit(opcode::HLE_CONTEXT_STATE, &[0, state]);
    let list = [type3_header(opcode::HLE_CONTEXT_STATE, 2), 1, state];
    for (i, word) in list.iter().enumerate() {
        ctx.memory.write_u32(state + CONTEXT_LOAD_LIST + 4 * i as u32, *word)?;
    }
    ctx.memory.write_u32(state + CONTEXT_LOAD_LIST_SIZE, 4 * list.len() as u32)?;
    Ok(())
}

/// The list titles call themselves instead of `GX2SetContextState`.
#[hle_export(lib = "gx2", name = "GX2GetContextStateDisplayList")]
fn gx2_get_context_state_display_list(
    ctx: &mut HleContext<'_>,
    state: u32,
    list: u32,
    size: u32,
) -> Result<(), HleError> {
    ctx.memory.write_u32(list, state + CONTEXT_LOAD_LIST)?;
    let bytes = ctx.memory.read_u32(state + CONTEXT_LOAD_LIST_SIZE)?;
    Ok(ctx.memory.write_u32(size, bytes)?)
}

#[hle_export(lib = "gx2", name = "GX2SetContextState")]
fn gx2_set_context_state(ctx: &mut HleContext<'_>, state: u32) {
    ctx.os.gx2.emit(opcode::HLE_CONTEXT_STATE, &[1, state]);
}

pub const EXPORTS: &[HleExport] = &[
    GX2_FLUSH,
    GX2_DRAW_DONE,
    GX2_WAIT_FOR_VSYNC,
    GX2_WAIT_FOR_FLIP,
    GX2_WAIT_FOR_FREE_SCAN_BUFFER,
    GX2_SWAP_SCAN_BUFFERS,
    GX2_GET_SWAP_STATUS,
    GX2_SET_SWAP_INTERVAL,
    GX2_GET_SWAP_INTERVAL,
    GX2_SET_EVENT_CALLBACK,
    GX2_GET_EVENT_CALLBACK,
    GX2_GET_LAST_SUBMITTED_TIME_STAMP,
    GX2_GET_RETIRED_TIME_STAMP,
    GX2_WAIT_TIME_STAMP,
    GX2_SUBMIT_USER_TIME_STAMP,
    GX2_GPU_TIME_TO_CPU_TIME,
    GX2_SAMPLE_TOP_GPU_CYCLE,
    GX2_SAMPLE_BOTTOM_GPU_CYCLE,
    GX2_BEGIN_DISPLAY_LIST,
    GX2_BEGIN_DISPLAY_LIST_EX,
    GX2_END_DISPLAY_LIST,
    GX2_GET_DISPLAY_LIST_WRITE_STATUS,
    GX2_GET_CURRENT_DISPLAY_LIST,
    GX2_GET_CONTEXT_STATE_DISPLAY_LIST,
    GX2_CALL_DISPLAY_LIST,
    GX2_DIRECT_CALL_DISPLAY_LIST,
    GX2_COPY_DISPLAY_LIST,
    GX2_PATCH_DISPLAY_LIST,
    GX2_SET_DEFAULT_STATE,
    GX2_SETUP_CONTEXT_STATE_EX,
    GX2_SET_CONTEXT_STATE,
];

#[cfg(test)]
mod tests {
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn display_lists_record_into_guest_memory() {
        let mut env = TestEnv::new();
        let list = SCRATCH + 0x100;
        env.call("gx2", "GX2BeginDisplayListEx", &[list, 0x100, 0]);
        env.call("gx2", "GX2SetPrimitiveRestartIndex", &[7]);
        let size = env.call("gx2", "GX2EndDisplayList", &[list]);
        assert_eq!(size, 12);
        assert_eq!(env.memory.read_u32(list + 8).unwrap(), 7);
        env.call("gx2", "GX2CallDisplayList", &[list, size]);
        env.call("gx2", "GX2DrawDone", &[]);
        assert_eq!(
            env.gpu.registers().get(wiiu_gpu::regs::context::VGT_MULTI_PRIM_IB_RESET_INDX),
            7
        );
    }

    #[test]
    fn context_state_list_restores_the_state_it_belongs_to() {
        let mut env = TestEnv::new();
        let state = SCRATCH + 0x1000;
        let (list_out, size_out) = (SCRATCH, SCRATCH + 4);
        env.call("gx2", "GX2SetupContextStateEx", &[state, 0]);
        env.call("gx2", "GX2GetContextStateDisplayList", &[state, list_out, size_out]);
        let list = env.memory.read_u32(list_out).unwrap();
        assert_eq!(list, state + super::CONTEXT_LOAD_LIST);
        assert_eq!(env.memory.read_u32(size_out).unwrap(), 12);
        assert_eq!(env.memory.read_u32(list + 8).unwrap(), state);
    }

    #[test]
    fn current_display_list_is_known_only_while_recording() {
        let mut env = TestEnv::new();
        let (address, size) = (SCRATCH, SCRATCH + 4);
        assert_eq!(env.call("gx2", "GX2GetCurrentDisplayList", &[address, size]), 0);
        env.call("gx2", "GX2BeginDisplayListEx", &[SCRATCH + 0x100, 0x80, 0]);
        assert_eq!(env.call("gx2", "GX2GetCurrentDisplayList", &[address, size]), 1);
        assert_eq!(env.memory.read_u32(address).unwrap(), SCRATCH + 0x100);
        assert_eq!(env.memory.read_u32(size).unwrap(), 0x80);
    }

    #[test]
    fn swap_status_counts_requests_and_vsync_retires_them() {
        let mut env = TestEnv::new();
        env.call("gx2", "GX2SwapScanBuffers", &[]);
        let (requests, executed) = (SCRATCH, SCRATCH + 4);
        env.call("gx2", "GX2GetSwapStatus", &[requests, executed, SCRATCH + 8, SCRATCH + 16]);
        assert_eq!(env.memory.read_u32(requests).unwrap(), 1);
        assert_eq!(env.memory.read_u32(executed).unwrap(), 0);
        env.os.vsync();
        env.call("gx2", "GX2GetSwapStatus", &[requests, executed, SCRATCH + 8, SCRATCH + 16]);
        assert_eq!(env.memory.read_u32(executed).unwrap(), 1);
    }

    #[test]
    fn the_least_interval_slows_fast_titles_only() {
        let mut env = TestEnv::new();
        env.os.gx2.min_swap_interval = 2;
        env.call("gx2", "GX2SetSwapInterval", &[1]);
        assert_eq!(env.call("gx2", "GX2GetSwapInterval", &[]), 2, "60 fps becomes 30");
        env.call("gx2", "GX2SetSwapInterval", &[3]);
        assert_eq!(env.call("gx2", "GX2GetSwapInterval", &[]), 3, "20 fps stays");
        env.os.gx2.forced_swap_interval = Some(1);
        assert_eq!(env.call("gx2", "GX2GetSwapInterval", &[]), 1, "forcing wins");
    }

    #[test]
    fn one_flip_per_swap_interval() {
        let mut env = TestEnv::new();
        env.call("gx2", "GX2SetSwapInterval", &[2]);
        for _ in 0..3 {
            env.call("gx2", "GX2SwapScanBuffers", &[]);
        }
        let executed: Vec<u32> = (0..6)
            .map(|_| {
                env.os.vsync();
                env.os.gx2.flips_executed
            })
            .collect();
        assert_eq!(executed, [0, 1, 1, 2, 2, 3]);
    }
}
