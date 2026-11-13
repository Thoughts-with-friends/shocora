//! Showing surfaces on the TV and the GamePad, and the flip.
use super::*;

pub(super) const BLIT_SHADER: &str = "
struct VsOut { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VsOut;
    out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@fragment fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(source, source_sampler, input.uv).rgb, 1.0);
}
";

impl LatteRenderer {
    fn present(&mut self, screen: Screen, source: SurfaceKey, slice: u32) {
        let Some(surface) = self.surfaces.get_mut(&source) else {
            self.log_once(format!("scan buffer copy from unknown surface {source:?}"));
            return;
        };
        if source.format.is_depth() || !matches!(source.format.kind(), TextureKind::Float) {
            return;
        }
        let source_view = surface.target_view(0, slice.min(source.layers.saturating_sub(1)));
        let (width, height) = (surface.texture.width(), surface.texture.height());
        let stale =
            self.screen_targets.get(&screen).is_none_or(|t| t.width != width || t.height != height);
        if stale {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(match screen {
                    Screen::Tv => "shocora TV",
                    Screen::Drc => "shocora GamePad",
                }),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.generation += 1;
            let image = ScreenImage { view, texture, width, height, generation: self.generation };
            self.screen_targets.insert(screen, image);
        }
        let Some(target) = self.screen_targets.get(&screen).cloned() else { return };
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shocora blit"),
            layout: &self.blit_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.blit_sampler),
                },
            ],
        });
        let pipeline = self.blit_pipeline.clone();
        let encoder = self.encoder();
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shocora present"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);
        self.presented.push(screen);
        if self.resolution.factor > 1 || !self.upscaler.shows(screen) {
            self.screens.set(screen, target);
        }
    }

    pub(super) fn flip(&mut self) {
        self.end_pass();
        for (screen, (key, slice)) in std::mem::take(&mut self.pending_scan) {
            self.present(screen, key, slice);
        }
        self.submit();
        // A picture this large has nothing left to gain from xBRZ, and it would take too long.
        let drawn_large = self.resolution.factor > 1;
        if !drawn_large {
            self.upscaler.update(
                &self.device,
                &self.queue,
                &self.presented,
                &self.screen_targets,
                &self.screens,
                &mut self.generation,
            );
        }
        let upscaled =
            !drawn_large && self.presented.iter().any(|screen| self.upscaler.shows(*screen));
        self.frame_generator.update(
            &self.device,
            &self.queue,
            &self.presented,
            &self.screen_targets,
            &self.screens,
            &mut self.generation,
            upscaled,
        );
        for (screen, streamer) in Screen::ALL.into_iter().zip(&mut self.streamers) {
            streamer.update(
                &self.device,
                &self.queue,
                self.presented
                    .contains(&screen)
                    .then(|| self.screen_targets.get(&screen))
                    .flatten(),
                &self.blit_pipeline,
                &self.blit_layout,
                &self.blit_sampler,
            );
        }
        self.presented.clear();
        self.flips += 1;
        if self.flips.is_multiple_of(600) && std::env::var_os("SHOCORA_PROFILE").is_some() {
            self.report_memory();
        }
        self.frame_draws = 0;
        if self.bind_groups.len() > MAX_BIND_GROUPS {
            self.bind_groups.clear();
        }
        if self.flips.is_multiple_of(CLEANUP_FLIPS) {
            self.clean_up_pipelines();
        }
    }

    pub(super) fn clear_screen(&mut self, screen: Screen, color: [f32; 4]) {
        let (width, height) = screen.default_size();
        let bytes: Vec<u8> = color
            .map(|c| (c.clamp(0.0, 1.0) * 255.0) as u8)
            .iter()
            .copied()
            .cycle()
            .take((width * height * 4) as usize)
            .collect();
        let texture = self.device.create_texture_with_data(
            &self.queue,
            &wgpu::TextureDescriptor {
                label: Some("shocora blank screen"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &bytes,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.generation += 1;
        let image = ScreenImage { view, texture, width, height, generation: self.generation };
        self.screen_targets.insert(screen, image.clone());
        self.screens.set(screen, image);
    }
}
