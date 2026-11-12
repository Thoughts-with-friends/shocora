use wiiu_interrupt::PendingInterrupts;
use wiiu_memory::Memory;

use crate::{
    GpuError, RenderCommand, Screen,
    command::{CommandProcessor, executor::CpContext},
    display::Display,
    latte::Latte,
    render::TextureUpload,
};

/// Counters exposed to the front end.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GpuStats {
    pub packets: u64,
    pub draws: u64,
    pub flips: u64,
    /// Draws that could not be issued (no shaders, geometry shaders, ...).
    pub skipped_draws: u64,
    /// Host time the GPU thread spent on command buffers.
    pub busy_nanos: u64,
}

/// Latte itself: the command processor and display controller, run by [`Gpu`](crate::Gpu) on the
/// emulator's thread or on one of its own.
#[derive(Debug, Default)]
pub(crate) struct Engine {
    cp: CommandProcessor,
    display: Display,
    latte: Latte,
    interrupts: PendingInterrupts,
    /// Command buffers executed: the retired timestamp.
    pub(crate) timestamp: u64,
}

/// What a saved state keeps of the GPU. Its caches fill again from guest memory.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct GpuState {
    cp: CommandProcessor,
    display: Display,
    interrupts: PendingInterrupts,
    pub(crate) timestamp: u64,
    color_targets: [crate::latte::SurfaceWords; 8],
    depth_target: crate::latte::SurfaceWords,
    surfaces: Vec<TextureUpload>,
}

impl Engine {
    /// Asks the renderer for what the GPU drew, which guest memory does not hold. The answer
    /// comes once the commands queued so far have reached the renderer; without one, the
    /// receiver just reports its sender gone.
    pub fn request_surfaces(&mut self) -> std::sync::mpsc::Receiver<Vec<TextureUpload>> {
        let (reply, answer) = std::sync::mpsc::channel();
        let keys = self.latte.host_surface_keys();
        self.latte
            .commands
            .push(RenderCommand::ReadSurfaces { keys, reply: crate::render::SurfaceReply(reply) });
        answer
    }

    pub fn save_state(&self, surfaces: Vec<TextureUpload>) -> GpuState {
        GpuState {
            cp: self.cp.clone(),
            display: self.display.clone(),
            interrupts: self.interrupts,
            timestamp: self.timestamp,
            color_targets: self.latte.color_targets,
            depth_target: self.latte.depth_target,
            surfaces,
        }
    }

    /// Goes on from `state`; the host renderer starts over like at a boot.
    pub fn restore_state(&mut self, state: GpuState, resolution_factor: u32) {
        *self = Self {
            cp: state.cp,
            display: state.display,
            interrupts: state.interrupts,
            timestamp: state.timestamp,
            ..Self::default()
        };
        self.latte.color_targets = state.color_targets;
        self.latte.depth_target = state.depth_target;
        self.reset_renderer();
        self.set_resolution_factor(resolution_factor);
        self.latte.restore_surfaces(state.surfaces);
    }

    /// Executes a PM4 command buffer.
    ///
    /// # Errors
    /// Fails on malformed packets or accesses outside guest memory.
    pub fn submit(&mut self, memory: &mut Memory, words: &[u32]) -> Result<(), GpuError> {
        self.timestamp += 1;
        let mut ctx = CpContext {
            memory,
            display: &mut self.display,
            interrupts: &mut self.interrupts,
            latte: &mut self.latte,
            timestamp: self.timestamp,
        };
        self.cp.execute(words, &mut ctx)
    }

    /// Draws the title's pictures this many times larger, as far as that stays within
    /// [`ResolutionScale::MAX_WIDTH`](crate::render::ResolutionScale::MAX_WIDTH).
    #[inline]
    pub fn set_resolution_factor(&mut self, factor: u32) {
        self.latte.set_resolution_factor(factor);
    }

    /// Makes the host renderer forget everything of the previous boot.
    #[inline]
    pub fn reset_renderer(&mut self) {
        self.latte.commands.push(RenderCommand::Reset);
    }

    /// Blanks both screens (boot / shutdown).
    pub fn blank_screens(&mut self) {
        for screen in Screen::ALL {
            Display::clear(&mut self.latte, screen, [0.0, 0.0, 0.0, 1.0]);
        }
    }

    /// Drains pending render commands for the host renderer.
    #[inline]
    pub fn take_render_commands(&mut self) -> Vec<RenderCommand> {
        std::mem::take(&mut self.latte.commands)
    }

    /// Render commands waiting for the host, to keep the queue bounded.
    #[inline]
    pub const fn pending_commands(&self) -> usize {
        self.latte.commands.len()
    }

    /// The CPU wrote `[address, address + size)` (effective) for the GPU to read: the title
    /// flushed it from the data cache, or the system wrote it.
    #[inline]
    pub fn note_cpu_write(&mut self, address: u32, size: u32) {
        self.latte.note_cpu_write(address, size);
    }

    #[inline]
    pub const fn registers(&self) -> &crate::regs::RegisterFile {
        &self.cp.regs
    }

    pub const fn stats(&self) -> GpuStats {
        GpuStats {
            packets: self.cp.stats.packets,
            draws: self.cp.stats.draws,
            flips: self.display.flips(),
            skipped_draws: self.latte.skipped_draws,
            busy_nanos: 0,
        }
    }
}
