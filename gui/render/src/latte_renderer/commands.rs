//! Running the emulator's command lists.
use super::*;

impl LatteRenderer {
    /// The console rebooted: what the last boot left must not be taken for the new one's, above
    /// all its shaders, whose ids start over.
    fn reset(&mut self) {
        self.submit();
        // Everything goes at once: let the GPU finish what still uses it first.
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        self.save_pipeline_cache();
        self.surfaces.clear();
        self.uploaded.clear();
        self.drawn_to.clear();
        self.resolution = ResolutionScale::default();
        // Threads still compiling keep the old map: its ids mean nothing from now on.
        self.modules = Arc::default();
        self.pipelines.clear();
        self.compiling.clear();
        self.pipeline_epoch += 1;
        self.bind_groups.clear();
        self.feedback_copies.clear();
        self.pending_scan.clear();
        self.presented.clear();
        self.frame_generator.reset();
        self.frame_draws = 0;
    }
}

impl RenderBackend for LatteRenderer {
    fn execute(&mut self, commands: &[RenderCommand]) {
        if let Some(at) =
            commands.iter().rposition(|command| matches!(command, RenderCommand::Reset))
        {
            self.execute(&commands[..at]);
            self.reset();
            self.execute(&commands[at + 1..]);
            return;
        }
        // New pipelines stall the host's draw thread: compiled together up front on other
        // threads, their cost does not add up.
        let new: Vec<&DrawCommand> = commands
            .iter()
            .filter_map(|command| match command {
                // Stream-out is compiled when its turn comes: see `pipeline`.
                RenderCommand::Draw(draw) if draw.stream_only => None,
                RenderCommand::Draw(draw) => {
                    (!self.pipelines.contains_key(&pipeline_key(draw))).then_some(draw.as_ref())
                }
                RenderCommand::ReadSurfaces { .. }
                | RenderCommand::ClearScreen { .. }
                | RenderCommand::ArenaWrite { .. }
                | RenderCommand::ArenaWrap
                | RenderCommand::Reset
                | RenderCommand::Upload(_)
                | RenderCommand::ForgetTexture(_)
                | RenderCommand::SetResolution(_)
                | RenderCommand::Clear { .. }
                | RenderCommand::CopySurface { .. }
                | RenderCommand::CopyToScanBuffer { .. }
                | RenderCommand::Flip => None,
            })
            .collect();
        if !new.is_empty() {
            self.compile_pipelines(&new);
        }
        let deadline = self.pipeline_setting.background().then(|| Instant::now() + PIPELINE_WAIT);
        self.wait_for_pipelines(deadline);
        for command in commands {
            match command {
                RenderCommand::ClearScreen { screen, color } => self.clear_screen(*screen, *color),
                RenderCommand::ArenaWrite { offset, bytes } => {
                    self.write_arena(u64::from(*offset), bytes);
                }
                RenderCommand::ArenaWrap => {
                    self.arena_wraps += 1;
                    self.submit();
                }
                RenderCommand::Reset => self.reset(),
                RenderCommand::ReadSurfaces { keys, reply } => {
                    let surfaces = self.read_surfaces(keys);
                    // Nobody waiting any more is no reason to stop.
                    let _ = reply.0.send(surfaces);
                }
                RenderCommand::SetResolution(resolution) => self.set_resolution(*resolution),
                RenderCommand::Upload(upload) => self.upload(upload),
                RenderCommand::ForgetTexture(key) => self.forget_texture(*key),
                RenderCommand::Clear { color, depth } => self.clear(*color, *depth),
                RenderCommand::Draw(draw) => {
                    self.frame_draws += 1;
                    if self.max_draws.is_none_or(|limit| self.frame_draws <= limit) {
                        self.draw(draw);
                    }
                }
                RenderCommand::CopySurface {
                    source,
                    source_slice,
                    target,
                    target_level,
                    target_slice,
                    // The surface's first level already is the one Latte picked.
                    source_level: _,
                } => {
                    self.copy_surface(
                        *source,
                        *source_slice,
                        *target,
                        *target_level,
                        *target_slice,
                    );
                }
                RenderCommand::CopyToScanBuffer { surface, slice, screen } => {
                    self.pending_scan.insert(*screen, (*surface, *slice));
                }
                RenderCommand::Flip => self.flip(),
            }
        }
    }
}
