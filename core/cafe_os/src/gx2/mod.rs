//! `gx2.rpl`: the graphics library.
//!
//! GX2 never touches Latte directly: it appends PM4 packets to its command
//! buffer and submits them to the GPU on flush, exactly like the real library
//! writes to the ring buffer. While a display list is being recorded the same
//! packets land in that list instead.
pub mod aperture;
pub mod command;
pub(crate) mod display;
pub mod draw;
mod interrupt;
pub mod render_target;
mod setup;
pub mod shader;
pub mod state;
pub mod surface;

pub use surface::Gx2Surface;
use wiiu_gpu::{
    command::{opcode, pm4::type3_header},
    regs::base,
};

use crate::hle::{HleContext, HleError, HleExport};

/// Commands submitted to Latte once this many words are queued.
const FLUSH_THRESHOLD: usize = 0x4_0000;

/// A display list being recorded.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct DisplayList {
    pub address: u32,
    pub capacity: u32,
    pub words: Vec<u32>,
}

/// Host-side copy of the GX2 command buffer, plus the state of the library.
///
/// All threads feed one command stream, so that a flush or a wait for the GPU from
/// any of them covers everything issued before it: titles rewrite buffers as soon as
/// that wait returns. Display lists belong to a thread: guest threads share one host
/// core here, so workers the console would run on other cores interrupt each other's
/// recording, and packets must not land in the wrong list.
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct Gx2State {
    commands: Vec<u32>,
    /// Display lists being recorded, by the thread recording them.
    lists: std::collections::HashMap<u32, DisplayList>,
    /// Thread of the HLE call being executed.
    pub active_thread: u32,
    pub initialized: bool,
    /// Core that called `GX2Init`.
    pub main_core: u32,
    pub tv_enabled: bool,
    pub drc_enabled: bool,
    /// `(alloc, free)` callbacks set by `GX2RSetAllocator`.
    pub gx2r_allocator: (u32, u32),
    pub flip_requests: u32,
    pub flips_executed: u32,
    pub swap_interval: u32,
    /// Replaces the interval the title asks for (`Config::swap_interval`).
    pub forced_swap_interval: Option<u32>,
    /// The title gets at least this interval (`Config::min_swap_interval`).
    pub min_swap_interval: u32,
    /// Timebase value of the last flip shown.
    pub last_flip_time: u64,
    /// Timebase value of the last vertical blank.
    pub last_vsync_time: u64,
    pub vsync_count: u64,
    /// sync_count at the last flip shown.
    pub last_flip_vsync: u64,
    /// Number of command buffers submitted so far.
    pub timestamp: u64,
    /// `(function, user data)` per `GX2CallbackEventType`.
    pub event_callbacks: [(u32, u32); command::event::COUNT],
    pub apertures: aperture::Apertures,
    /// Timestamp interrupts the title asked for, as `(GPU timestamp, event kind)`: raised
    /// once the GPU has executed the commands up to them.
    #[serde(default)]
    pub pending_interrupts: Vec<(u64, u32)>,
}

impl Default for Gx2State {
    fn default() -> Self {
        Self {
            commands: Vec::new(),
            lists: std::collections::HashMap::new(),
            active_thread: 0,
            initialized: false,
            main_core: 0,
            tv_enabled: false,
            drc_enabled: false,
            gx2r_allocator: (0, 0),
            flip_requests: 0,
            flips_executed: 0,
            swap_interval: 1,
            forced_swap_interval: None,
            min_swap_interval: 0,
            last_flip_time: 0,
            last_vsync_time: 0,
            vsync_count: 0,
            last_flip_vsync: 0,
            timestamp: 0,
            event_callbacks: [(0, 0); command::event::COUNT],
            apertures: aperture::Apertures::default(),
            pending_interrupts: Vec::new(),
        }
    }
}

impl Gx2State {
    fn sink(&mut self) -> &mut Vec<u32> {
        match self.lists.get_mut(&self.active_thread) {
            Some(list) => &mut list.words,
            None => &mut self.commands,
        }
    }

    /// Appends a type-3 packet.
    pub(crate) fn emit(&mut self, opcode: u8, body: &[u32]) {
        let sink = self.sink();
        sink.push(type3_header(opcode, body.len().max(1)));
        if body.is_empty() {
            sink.push(0);
        } else {
            sink.extend_from_slice(body);
        }
    }

    /// Appends a word that already is part of a packet (`GX2CopyDisplayList`).
    pub(crate) fn push_raw(&mut self, word: u32) {
        self.sink().push(word);
    }

    /// `SET_CONTEXT_REG` for consecutive registers starting at `register`.
    pub(crate) fn set_context(&mut self, register: u32, values: &[u32]) {
        let sink = self.sink();
        sink.push(type3_header(opcode::SET_CONTEXT_REG, 1 + values.len()));
        sink.push(register - base::CONTEXT);
        sink.extend_from_slice(values);
    }

    /// `SET_CONFIG_REG` for consecutive registers starting at `register`.
    pub(crate) fn set_config(&mut self, register: u32, values: &[u32]) {
        let sink = self.sink();
        sink.push(type3_header(opcode::SET_CONFIG_REG, 1 + values.len()));
        sink.push(register - base::CONFIG);
        sink.extend_from_slice(values);
    }

    pub(crate) fn begin_display_list(&mut self, address: u32, capacity: u32) {
        self.lists.insert(self.active_thread, DisplayList { address, capacity, words: Vec::new() });
    }

    pub(crate) fn end_display_list(&mut self) -> Option<DisplayList> {
        self.lists.remove(&self.active_thread)
    }

    /// `(address, capacity)` of the list the active thread is recording.
    pub(crate) fn current_display_list(&self) -> Option<(u32, u32)> {
        self.lists.get(&self.active_thread).map(|list| (list.address, list.capacity))
    }

    pub(crate) fn recording(&self) -> bool {
        self.lists.contains_key(&self.active_thread)
    }
}

/// Effective → physical address, leaving anything unusual as it is.
pub(crate) const fn physical(address: u32) -> u32 {
    match wiiu_mem_addr::effective_to_physical(address) {
        Some(physical) => physical,
        None => address,
    }
}

/// Submits everything queued so far to Latte.
pub(crate) fn flush(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    let commands = std::mem::take(&mut ctx.os.gx2.commands);
    if !commands.is_empty() {
        ctx.os.gx2.timestamp += 1;
        ctx.gpu.submit(ctx.memory, &commands)?;
        // Keep the allocation for the next frame.
        let mut recycled = commands;
        recycled.clear();
        ctx.os.gx2.commands = recycled;
    }
    Ok(())
}

/// Avoids unbounded command buffers in titles that rarely flush.
pub(crate) fn flush_if_large(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    if ctx.os.gx2.commands.len() >= FLUSH_THRESHOLD { flush(ctx) } else { Ok(()) }
}

pub const EXPORTS: &[HleExport] = &[
    display::GX2_INIT,
    display::GX2_SHUTDOWN,
    display::GX2_GET_MAIN_CORE_ID,
    display::GX2_SET_TV_ENABLE,
    display::GX2_SET_DRC_ENABLE,
    display::GX2_CALC_TV_SIZE,
    display::GX2_CALC_DRC_SIZE,
    display::GX2_CLEAR_COLOR,
    display::GX2_CLEAR_BUFFERS_EX,
    display::GX2_CLEAR_DEPTH_STENCIL_EX,
    display::GX2_SET_CLEAR_DEPTH,
    display::GX2_SET_CLEAR_STENCIL,
    display::GX2_COPY_COLOR_BUFFER_TO_SCAN_BUFFER,
    display::GX2_COPY_SURFACE,
    display::GX2_RESOLVE_AA_COLOR_BUFFER,
    display::GX2_CONVERT_DEPTH_BUFFER_TO_TEXTURE_SURFACE,
    display::GX2_GET_SYSTEM_TV_SCAN_MODE,
    display::GX2_GET_SYSTEM_TV_ASPECT_RATIO,
    display::GX2_IS_VIDEO_OUT_READY,
    display::GX2_GET_GPU_TIMEOUT,
    render_target::GX2_INIT_COLOR_BUFFER_REGS,
    render_target::GX2_INIT_DEPTH_BUFFER_REGS,
    render_target::GX2_SET_COLOR_BUFFER,
    render_target::GX2_SET_DEPTH_BUFFER,
    render_target::GX2_SET_CLEAR_DEPTH_STENCIL,
    setup::GX2R_SET_ALLOCATOR,
    setup::GX2_INVALIDATE,
    setup::GX2_SET_TV_BUFFER,
    setup::GX2_SET_DRC_BUFFER,
    setup::GX2_SET_TV_SCALE,
    setup::GX2_SET_DRC_SCALE,
    setup::GX2_GET_VERTEX_SHADER_GPRS,
    setup::GX2_GET_VERTEX_SHADER_STACK_ENTRIES,
    setup::GX2_GET_PIXEL_SHADER_GPRS,
    setup::GX2_GET_PIXEL_SHADER_STACK_ENTRIES,
    setup::GX2_GET_GEOMETRY_SHADER_GPRS,
    setup::GX2_GET_GEOMETRY_SHADER_STACK_ENTRIES,
    setup::GX2_CALC_GEOMETRY_SHADER_INPUT_RING_BUFFER_SIZE,
    setup::GX2_CALC_GEOMETRY_SHADER_OUTPUT_RING_BUFFER_SIZE,
    setup::GX2_PERF_INIT,
    setup::GX2_PERF_SET_COLLECTION_METHOD,
    setup::GX2_PERF_METRICS_CLEAR,
    setup::GX2_PERF_TAG_ENABLE,
    shader::GX2_CALC_FETCH_SHADER_SIZE_EX,
    shader::GX2_INIT_FETCH_SHADER_EX,
    shader::GX2_TEMP_GET_GPU_VERSION,
    shader::GX2_INIT_SAMPLER,
    shader::GX2_INIT_SAMPLER_ZM_FILTER,
    shader::GX2_INIT_SAMPLER_XY_FILTER,
    shader::GX2_INIT_SAMPLER_CLAMPING,
    shader::GX2_INIT_SAMPLER_BORDER_TYPE,
    shader::GX2_INIT_SAMPLER_DEPTH_COMPARE,
    shader::GX2_INIT_SAMPLER_LOD,
    aperture::GX2_ALLOCATE_TILING_APERTURE_EX,
    aperture::GX2_FREE_TILING_APERTURE,
    surface::GX2_CALC_SURFACE_SIZE_AND_ALIGNMENT,
    surface::GX2_GET_SURFACE_MIP_PITCH,
    surface::GX2_GET_SURFACE_MIP_SLICE_SIZE,
    surface::GX2_GET_SURFACE_FORMAT_BITS,
    surface::GX2_GET_SURFACE_SWIZZLE_OFFSET,
    surface::GX2_GET_SURFACE_SWIZZLE,
    surface::GX2_SET_SURFACE_SWIZZLE,
    surface::GX2_SURFACE_IS_COMPRESSED,
    surface::GX2_CHECK_SURFACE_USE_VS_FORMAT,
    surface::GX2_CALC_DEPTH_BUFFER_HIZ_INFO,
    surface::GX2_CALC_COLOR_BUFFER_AUX_INFO,
];

#[cfg(test)]
mod tests {
    use wiiu_gpu::command::pm4::type3_header;

    use super::Gx2State;

    /// Intent: GX2 builds PM4 type-3 packets like the console's GX2 does; a packet with no
    /// body still carries one zero word (the hardware has no empty packets).
    /// Pass: an empty `emit` gives header + 0; one with a body gives header + body.
    #[test]
    fn emit_builds_type3_packets() {
        let mut gx2 = Gx2State::default();
        gx2.emit(0x10, &[]);
        gx2.emit(0x20, &[7, 8]);
        assert_eq!(gx2.commands, [type3_header(0x10, 1), 0, type3_header(0x20, 2), 7, 8]);
    }
}
