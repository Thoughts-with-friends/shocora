//! Render passes, resource bindings and draws (stream-out included).
use super::*;

/// The stream-out buffer and what rendering into nothing needs.
pub(super) struct StreamOutput {
    buffer: wgpu::Buffer,
    pub(super) layout: wgpu::BindGroupLayout,
    group: wgpu::BindGroup,
    /// A pass needs an attachment, even with nothing to draw.
    depth: wgpu::TextureView,
}

impl StreamOutput {
    pub(super) fn new(device: &wgpu::Device) -> Option<Self> {
        if !device.features().contains(wgpu::Features::VERTEX_WRITABLE_STORAGE) {
            return None;
        }
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shocora stream-out"),
            size: u64::from(STREAM_REGION_SIZE),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shocora stream-out"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shocora stream-out"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }],
        });
        let depth = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("shocora stream-out depth"),
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        Some(Self { buffer, layout, group, depth })
    }
}

/// Where a draw's copy of a surface goes (its own target, or a volume made as 2D).
fn feedback_texture(
    device: &wgpu::Device,
    source: &wgpu::Texture,
    size: wgpu::Extent3d,
    dimension: wgpu::TextureDimension,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shocora feedback copy"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension,
        format: source.format(),
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// What a draw samples of what it draws to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Conflict {
    /// Nothing it draws to, or other levels or slices of it.
    None,
    /// The depth it tests against but does not write: the pass attaches the depth read-only
    /// and the draw samples it directly, as Cemu's feedback loop does.
    ReadOnlyDepth,
    /// What it writes: it samples a copy made just before.
    Copy,
}

/// Whether a draw writes neither depth nor stencil, so its depth can be attached read-only.
fn writes_no_depth(state: &wiiu_gpu::render::PipelineState) -> bool {
    let keeps = |face: wiiu_gpu::render::StencilFace| {
        face.fail & 7 == 0 && face.depth_fail & 7 == 0 && face.pass & 7 == 0
    };
    let stencil_writes = state.depth_format.is_some_and(HostFormat::has_stencil)
        && state.stencil_test
        && state.stencil_write_mask != 0
        && !(keeps(state.stencil_front) && keeps(state.stencil_back));
    !state.depth_write && !stencil_writes
}

impl LatteRenderer {
    /// What `draw` samples through `bound` of what it draws to.
    fn conflict(&self, draw: &DrawCommand, bound: &BoundTexture) -> Conflict {
        let Some(surface) = self.surfaces.get(&bound.key) else { return Conflict::None };
        if let Some((key, slice)) = draw.depth_target
            && key == bound.key
            && surface.sampled_covers(bound, slice)
        {
            return if writes_no_depth(&draw.state) {
                Conflict::ReadOnlyDepth
            } else {
                Conflict::Copy
            };
        }
        let drawn = draw
            .color_targets
            .iter()
            .flatten()
            .any(|(key, slice)| *key == bound.key && surface.sampled_covers(bound, *slice));
        if drawn { Conflict::Copy } else { Conflict::None }
    }

    /// Whether `draw` samples the depth it is attached to, read-only.
    fn samples_own_depth(&self, draw: &DrawCommand) -> bool {
        draw.depth_target.is_some()
            && draw.textures.iter().any(|t| self.conflict(draw, t) == Conflict::ReadOnlyDepth)
    }

    /// A surface sampled while it is also a target: the draw reads a copy made
    /// just before it, which is what the hardware's texture cache would see.
    fn feedback_copy(
        &mut self,
        bound: &BoundTexture,
        dim: TextureDim,
        kind: TextureKind,
    ) -> Option<wgpu::TextureView> {
        let source = self.surfaces.get(&bound.key)?.texture.clone();
        if !matches!(dim, TextureDim::D2 | TextureDim::D2Array)
            || bound.key.format.kind() != kind
            || source.dimension() != wgpu::TextureDimension::D2
        {
            return None;
        }
        self.end_pass();
        // The level the draw samples, as a texture of its own: titles copy one slice of an
        // array into another this way (Breath of the Wild's terrain height maps), and a
        // dummy in its place filled their tiles with zeros.
        let level = bound.base_level.min(source.mip_level_count() - 1);
        let layers = source.depth_or_array_layers();
        let first_layer = if dim == TextureDim::D2 { bound.base_layer.min(layers - 1) } else { 0 };
        let size = wgpu::Extent3d {
            depth_or_array_layers: if dim == TextureDim::D2 { 1 } else { layers },
            ..source.size().mip_level_size(level, wgpu::TextureDimension::D2)
        };
        let device = self.device.clone();
        let copy = self
            .feedback_copies
            .entry(bound.key)
            .and_modify(|texture| {
                if texture.size() != size || texture.dimension() != wgpu::TextureDimension::D2 {
                    *texture = feedback_texture(&device, &source, size, wgpu::TextureDimension::D2);
                }
            })
            .or_insert_with(|| {
                feedback_texture(&device, &source, size, wgpu::TextureDimension::D2)
            });
        let target = copy.clone();
        self.counters.copies += 1;
        self.encoder().copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                mip_level: level,
                origin: wgpu::Origin3d { x: 0, y: 0, z: first_layer },
                ..source.as_image_copy()
            },
            target.as_image_copy(),
            size,
        );
        let aspect = if bound.key.format.has_stencil() {
            wgpu::TextureAspect::DepthOnly
        } else {
            wgpu::TextureAspect::All
        };
        let view = match dim {
            TextureDim::D2Array => {
                let base = bound.base_layer.min(layers - 1);
                target.create_view(&wgpu::TextureViewDescriptor {
                    aspect,
                    dimension: Some(wgpu::TextureViewDimension::D2Array),
                    base_array_layer: base,
                    array_layer_count: Some(bound.layer_count.min(layers - base).max(1)),
                    ..Default::default()
                })
            }
            _ => target.create_view(&wgpu::TextureViewDescriptor {
                aspect,
                dimension: Some(wgpu::TextureViewDimension::D2),
                ..Default::default()
            }),
        };
        Some(view)
    }

    /// A volume sampled as 3D whose surface was made as a 2D array (drawn slice by slice,
    /// as render targets are made): the draw reads its slices copied into a 3D texture.
    /// A dummy in its place gave Breath of the Wild's 8x8x8 `R11G11B10` volume zeros.
    fn volume_copy(&mut self, bound: &BoundTexture) -> Option<wgpu::TextureView> {
        let source = self.surfaces.get(&bound.key)?.texture.clone();
        if source.dimension() != wgpu::TextureDimension::D2 {
            return None;
        }
        self.end_pass();
        let size = wgpu::Extent3d {
            depth_or_array_layers: source.depth_or_array_layers(),
            ..source.size().mip_level_size(0, wgpu::TextureDimension::D2)
        };
        let device = self.device.clone();
        let volume = wgpu::TextureDimension::D3;
        let copy = self
            .feedback_copies
            .entry(bound.key)
            .and_modify(|texture| {
                if texture.size() != size || texture.dimension() != volume {
                    *texture = feedback_texture(&device, &source, size, volume);
                }
            })
            .or_insert_with(|| feedback_texture(&device, &source, size, volume));
        let target = copy.clone();
        self.counters.copies += 1;
        self.encoder().copy_texture_to_texture(
            source.as_image_copy(),
            target.as_image_copy(),
            size,
        );
        Some(target.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D3),
            ..Default::default()
        }))
    }

    /// Sorts a texture sampled while attached into [`Counters::feedback`].
    fn count_feedback(&mut self, draw: &DrawCommand, bound: &BoundTexture) {
        let depth = draw.depth_target.is_some_and(|(key, _)| key == bound.key);
        let kind = match (self.conflict(draw, bound), depth) {
            (Conflict::ReadOnlyDepth, _) => 0,
            (Conflict::Copy, true) => 1,
            (Conflict::Copy, false) => 2,
            (Conflict::None, _) => 3,
        };
        self.counters.feedback[kind] += 1;
        if kind == 2
            && let Some((slot, Some((key, slice)))) = draw
                .color_targets
                .iter()
                .enumerate()
                .find(|(_, t)| t.is_some_and(|(key, _)| key == bound.key))
                .map(|(slot, t)| (slot, *t))
        {
            self.log_once(format!(
                "feedback: slot {slot} {:#010x} {}x{}x{} {:?} slice {slice} mask {:#x} blend {} | sampled {:?} level {}+{} layer {}+{} | vs {:016x} ps {:016x}",
                key.address,
                key.width,
                key.height,
                key.layers,
                key.format,
                draw.state.color_masks[slot],
                draw.state.blend[slot].enabled,
                bound.dim,
                bound.base_level,
                bound.level_count,
                bound.base_layer,
                bound.layer_count,
                draw.vertex.id,
                draw.pixel.id,
            ));
        }
    }

    /// Whether `bound` is sampled as 3D but its surface was made as 2D.
    fn needs_volume_copy(&self, bound: &BoundTexture) -> bool {
        bound.dim == TextureDim::D3
            && self
                .surfaces
                .get(&bound.key)
                .is_some_and(|s| s.texture.dimension() == wgpu::TextureDimension::D2)
    }

    /// `group(1)` of a draw. Textures and samplers rarely change between draws.
    fn resource_group(
        &mut self,
        draw: &DrawCommand,
        bindings: &[(u32, BindingKind)],
    ) -> Option<wgpu::BindGroup> {
        let mut hasher = fast_hasher().build_hasher();
        bindings.hash(&mut hasher);
        for bound in &draw.textures {
            bound.hash(&mut hasher);
            self.surfaces.get(&bound.key).map(|s| s.id).hash(&mut hasher);
        }
        let key = hasher.finish();
        // A draw sampling what it draws to, or a volume made as 2D, reads a fresh copy every
        // time.
        let feedback = draw
            .textures
            .iter()
            .any(|t| self.needs_volume_copy(t) || self.conflict(draw, t) == Conflict::Copy);
        if !feedback {
            if let Some(group) = self.bind_groups.get(&key) {
                return Some(group.clone());
            }
        }
        let mut views: Vec<(u32, wgpu::TextureView)> = Vec::new();
        let mut samplers: Vec<(u32, wgpu::Sampler)> = Vec::new();
        for (binding, kind) in bindings {
            match kind {
                BindingKind::Texture { dim, kind } => {
                    let bound = draw.textures.iter().find(|t| t.binding == *binding).copied();
                    let conflict = bound.map_or(Conflict::None, |b| self.conflict(draw, &b));
                    if conflict != Conflict::None
                        && self.profile
                        && let Some(bound) = &bound
                    {
                        self.count_feedback(draw, bound);
                    }
                    let in_use = conflict == Conflict::Copy;
                    let view = match bound {
                        Some(bound) if in_use => self.feedback_copy(&bound, *dim, *kind),
                        // A surface made as 2D cannot be seen as 3D, nor the other way round.
                        Some(bound)
                            if self.surfaces.get(&bound.key).is_some_and(|s| {
                                (s.texture.dimension() == wgpu::TextureDimension::D3)
                                    == (bound.dim == TextureDim::D3)
                            }) && bound.key.format.kind() == *kind
                                && bound.dim == *dim =>
                        {
                            let key = bound.key;
                            self.surfaces.get_mut(&key).map(|s| s.sample_view(&key, &bound))
                        }
                        Some(bound)
                            if *dim == TextureDim::D3
                                && bound.key.format.kind() == *kind
                                && self.needs_volume_copy(&bound) =>
                        {
                            self.volume_copy(&bound)
                        }
                        _ => None,
                    };
                    let view = match view {
                        Some(view) => view,
                        None => {
                            if let Some(bound) = bound
                                && let Some(surface) = self.surfaces.get(&bound.key)
                            {
                                log::warn!(
                                    "texture {:#010x} {}x{}x{} {:?} sampled as {:?}/{:?} but made as {:?} ({:?}): dummy bound",
                                    bound.key.address,
                                    bound.key.width,
                                    bound.key.height,
                                    bound.key.layers,
                                    bound.key.format,
                                    bound.dim,
                                    dim,
                                    surface.texture.dimension(),
                                    bound.key.format.kind(),
                                );
                            }
                            self.dummy_texture(*dim, *kind)
                        }
                    };
                    views.push((*binding, view));
                }
                BindingKind::Sampler { comparison, filtering } => {
                    let bound = draw.textures.iter().find(|t| t.binding + 1 == *binding);
                    let mut desc = bound.and_then(|t| t.sampler).unwrap_or_default();
                    if !*filtering {
                        desc.mag_linear = false;
                        desc.min_linear = false;
                        desc.mip_linear = false;
                    }
                    if *comparison && desc.compare.is_none() {
                        desc.compare = Some(1);
                    }
                    if !*comparison {
                        desc.compare = None;
                    }
                    samplers.push((*binding, self.sampler(desc)));
                }
            }
        }
        let mut entries: Vec<wgpu::BindGroupEntry<'_>> =
            Vec::with_capacity(views.len() + samplers.len());
        for (binding, view) in &views {
            entries.push(wgpu::BindGroupEntry {
                binding: *binding,
                resource: wgpu::BindingResource::TextureView(view),
            });
        }
        for (binding, sampler) in &samplers {
            entries.push(wgpu::BindGroupEntry {
                binding: *binding,
                resource: wgpu::BindingResource::Sampler(sampler),
            });
        }
        let layout = self
            .resource_layouts
            .get(bindings)
            .cloned()
            .unwrap_or_else(|| self.resource_layout(bindings));
        self.counters.bind_groups_made += 1;
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shocora resources"),
            layout: &layout,
            entries: &entries,
        });
        if let Some(error) = pollster::block_on(scope.pop()) {
            self.log_once(format!("draw skipped, its textures do not fit: {error}"));
            return None;
        }
        if !feedback {
            self.bind_groups.insert(key, group.clone());
        }
        Some(group)
    }

    fn open_pass(&mut self, draw: &DrawCommand, depth_read_only: bool) {
        let key: PassKey = (draw.color_targets, draw.depth_target, depth_read_only);
        if let Some(open) = self.pass_key.filter(|_| self.pass.is_some()) {
            // A pass with its depth read-only also suits draws that write none.
            let suits = open == key
                || (open.2 && open.0 == key.0 && open.1 == key.1 && writes_no_depth(&draw.state));
            if suits {
                return;
            }
        }
        self.end_pass();
        if self.part_draws >= PART_DRAWS {
            self.cut();
        }
        // Latte draws to attachments of different sizes (the smaller one bounds the
        // draw); the host insists on one size, the first color target's. A misfit gets
        // a throwaway surface of the right size.
        let extent =
            draw.color_targets.iter().flatten().next().map(|(key, _)| (key.width, key.height));
        // The host also refuses one slice attached twice: Latte lets the later attachment
        // write the same memory, here it writes a throwaway surface.
        let mut attached = HashSet::default();
        let mut fitted = |index: u32, key: SurfaceKey, slice: u32| {
            let slice = slice.min(key.layers.saturating_sub(1));
            let (width, height) = extent.unwrap_or((key.width, key.height));
            if (key.width, key.height) == (width, height) && attached.insert((key, slice)) {
                return (key, slice, false);
            }
            // One stand-in per attachment, so that they never meet in a pass either.
            let address = STAND_IN_ADDRESS + index * 0x100;
            (SurfaceKey { address, width, height, layers: 1, format: key.format }, 0, true)
        };
        let mut color_views: Vec<Option<(wgpu::TextureView, bool)>> = Vec::with_capacity(8);
        for (index, target) in (0..).zip(&draw.color_targets) {
            color_views.push(target.map(|(key, slice)| {
                let (key, slice, stand_in) = fitted(index, key, slice);
                (self.target_surface(key, 1).target_view(0, slice), stand_in)
            }));
        }
        let depth_view = draw.depth_target.map(|(key, slice)| {
            let (key, slice, stand_in) = fitted(8, key, slice);
            (self.target_surface(key, 1).target_view(0, slice), stand_in)
        });
        let last = color_views.iter().rposition(Option::is_some).map_or(0, |i| i + 1);
        let attachments: Vec<Option<wgpu::RenderPassColorAttachment<'_>>> = color_views[..last]
            .iter()
            .map(|view| {
                view.as_ref().map(|(view, stand_in)| wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: if *stand_in {
                            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                        } else {
                            wgpu::LoadOp::Load
                        },
                        store: wgpu::StoreOp::Store,
                    },
                })
            })
            .collect();
        let depth_format = draw.depth_target.map(|(key, _)| key.format);
        let depth_attachment = depth_view.as_ref().map(|(view, stand_in)| {
            // Read-only, the depth may be sampled in the pass too (a stand-in never is).
            if depth_read_only && !*stand_in {
                return wgpu::RenderPassDepthStencilAttachment {
                    view,
                    depth_ops: None,
                    stencil_ops: None,
                };
            }
            wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: Some(wgpu::Operations {
                    load: if *stand_in { wgpu::LoadOp::Clear(1.0) } else { wgpu::LoadOp::Load },
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: depth_format.is_some_and(HostFormat::has_stencil).then_some(
                    wgpu::Operations {
                        load: if *stand_in { wgpu::LoadOp::Clear(0) } else { wgpu::LoadOp::Load },
                        store: wgpu::StoreOp::Store,
                    },
                ),
            }
        });
        self.counters.passes += 1;
        let encoder = self.encoder();
        let pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shocora draw"),
                color_attachments: &attachments,
                depth_stencil_attachment: depth_attachment,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            })
            .forget_lifetime();
        self.pass = Some(pass);
        self.pass_key = Some(key);
    }

    /// Runs a draw that only has its vertex shader stream out.
    fn stream_draw(&mut self, draw: &DrawCommand, pipeline: &wgpu::RenderPipeline) {
        let bindings = sorted_bindings(draw);
        let Some(group) = self.resource_group(draw, &bindings) else { return };
        let Some((stream_group, depth)) =
            self.stream.as_ref().map(|s| (s.group.clone(), s.depth.clone()))
        else {
            return;
        };
        self.end_pass();
        self.counters.passes += 1;
        let arena_group = self.arena_group.clone();
        let encoder = self.encoder();
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shocora stream-out"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &arena_group, &[draw.params_offset]);
        pass.set_bind_group(1, &group, &[]);
        pass.set_bind_group(2, &stream_group, &[]);
        let first = draw.base_vertex.cast_unsigned();
        pass.draw(first..first + draw.count, 0..draw.instances);
    }

    /// Moves stream-out results to where later draws read vertices.
    fn finish_stream_out(&mut self, draw: &DrawCommand) {
        if draw.stream_copies.is_empty() {
            return;
        }
        let Some(source) = self.stream.as_ref().map(|s| s.buffer.clone()) else { return };
        self.end_pass();
        let arena = self.arena.clone();
        let encoder = self.encoder();
        for copy in &draw.stream_copies {
            encoder.copy_buffer_to_buffer(
                &source,
                u64::from(copy.offset),
                &arena,
                u64::from(STREAM_REGION_BASE) + u64::from(copy.offset),
                u64::from(copy.size),
            );
        }
    }

    pub(super) fn draw(&mut self, draw: &DrawCommand) {
        let bindings = sorted_bindings(draw);
        let Some(pipeline) = self.pipeline(draw) else { return };
        if draw.stream_only {
            self.stream_draw(draw, &pipeline);
            self.finish_stream_out(draw);
            return;
        }
        let Some(group) = self.resource_group(draw, &bindings) else { return };
        let depth_read_only = self.samples_own_depth(draw);
        self.open_pass(draw, depth_read_only);
        let extent = draw
            .color_targets
            .iter()
            .flatten()
            .map(|(key, _)| (key.width, key.height))
            .chain(draw.depth_target.map(|(key, _)| (key.width, key.height)))
            .fold((u32::MAX, u32::MAX), |acc, size| (acc.0.min(size.0), acc.1.min(size.1)));
        let Some(pass) = self.pass.as_mut() else { return };
        // Like Cemu's draw_execute, state equal to what the pass holds is not set again: each
        // call is recorded and tracked by wgpu, which took a third of the render thread.
        let bound = &mut self.bound;
        if bound.pipeline.as_ref() != Some(&pipeline) {
            pass.set_pipeline(&pipeline);
            bound.pipeline = Some(Arc::clone(&pipeline));
        }
        if bound.params_offset != Some(draw.params_offset) {
            pass.set_bind_group(0, &self.arena_group, &[draw.params_offset]);
            bound.params_offset = Some(draw.params_offset);
        }
        if bound.resources.as_ref() != Some(&group) {
            pass.set_bind_group(1, &group, &[]);
            bound.resources = Some(group.clone());
        }
        if draw.vertex.stream_out && !bound.stream {
            if let Some(stream) = &self.stream {
                pass.set_bind_group(2, &stream.group, &[]);
                bound.stream = true;
            }
        }
        let [x, y, width, height, near, far] = draw.viewport;
        let (limit_w, limit_h) = (extent.0 as f32, extent.1 as f32);
        let vx = x.clamp(0.0, limit_w - 1.0);
        let vy = y.clamp(0.0, limit_h - 1.0);
        let vw = (width + x - vx).clamp(1.0, limit_w - vx);
        let vh = (height + y - vy).clamp(1.0, limit_h - vy);
        let scale = draw
            .color_targets
            .iter()
            .flatten()
            .next()
            .or(draw.depth_target.as_ref())
            .map_or(1, |(key, _)| self.resolution.of(key));
        let factor = scale as f32;
        let viewport =
            [vx * factor, vy * factor, vw * factor, vh * factor, near.min(far), far.max(near)];
        if bound.viewport != Some(viewport) {
            let [x, y, w, h, near, far] = viewport;
            pass.set_viewport(x, y, w, h, near, far);
            bound.viewport = Some(viewport);
        }
        let [sx, sy, sw, sh] = draw.scissor;
        let (sx, sy) = (sx.min(extent.0), sy.min(extent.1));
        let scissor =
            [sx * scale, sy * scale, sw.min(extent.0 - sx) * scale, sh.min(extent.1 - sy) * scale];
        if bound.scissor != Some(scissor) {
            let [x, y, w, h] = scissor;
            pass.set_scissor_rect(x, y, w, h);
            bound.scissor = Some(scissor);
        }
        if bound.stencil_reference != Some(draw.stencil_reference) {
            pass.set_stencil_reference(u32::from(draw.stencil_reference));
            bound.stencil_reference = Some(draw.stencil_reference);
        }
        if bound.blend_constant != Some(draw.blend_constant) {
            let [r, g, b, a] = draw.blend_constant;
            pass.set_blend_constant(wgpu::Color {
                r: f64::from(r),
                g: f64::from(g),
                b: f64::from(b),
                a: f64::from(a),
            });
            bound.blend_constant = Some(draw.blend_constant);
        }
        if let Some(offset) = draw.index_offset {
            pass.set_index_buffer(
                self.arena.slice(u64::from(offset)..u64::from(offset) + u64::from(draw.count) * 4),
                wgpu::IndexFormat::Uint32,
            );
            pass.draw_indexed(0..draw.count, draw.base_vertex, 0..draw.instances);
        } else {
            let first = draw.base_vertex.cast_unsigned();
            pass.draw(first..first + draw.count, 0..draw.instances);
        }
        self.draws_executed += 1;
        self.part_draws += 1;
        for (key, _) in draw.color_targets.iter().flatten() {
            *self.target_log.entry(*key).or_insert(0) += 1;
        }
        self.finish_stream_out(draw);
    }
}
