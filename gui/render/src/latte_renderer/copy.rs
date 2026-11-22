//! Copies between surfaces, depth into color included.
use super::*;

/// Writes a depth surface into a color one, which the host cannot copy between.
const DEPTH_TO_COLOR_SHADER: &str = r"
@group(0) @binding(0) var source: texture_depth_2d;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(textureLoad(source, vec2<i32>(position.xy), 0), 0.0, 0.0, 1.0);
}
";

impl LatteRenderer {
    pub(super) fn copy_surface(
        &mut self,
        source: SurfaceKey,
        source_slice: u32,
        target: SurfaceKey,
        target_level: u32,
        target_slice: u32,
    ) {
        self.end_pass();
        self.counters.copies += 1;
        self.feedback_sync.remove(&target);
        if !self.surfaces.contains_key(&source) {
            return;
        }
        if source.format.is_depth()
            && matches!(target.format, HostFormat::R32Float | HostFormat::R16Float)
        {
            self.copy_depth_to_color(source, source_slice, target, target_level, target_slice);
            return;
        }
        if wgpu_format(source.format).remove_srgb_suffix()
            != wgpu_format(target.format).remove_srgb_suffix()
        {
            self.log_once(format!(
                "copy between unrelated formats {:?} -> {:?}",
                source.format, target.format
            ));
            return;
        }
        let source_texture = self.surfaces.get(&source).map(|s| s.texture.clone());
        let target_texture = self.target_surface(target, target_level + 1).texture.clone();
        let Some(source_texture) = source_texture else { return };
        let (width, height) = (
            source_texture.width().min(target_texture.width() >> target_level).max(1),
            source_texture.height().min(target_texture.height() >> target_level).max(1),
        );
        // An invalid copy would invalidate everything recorded with it, the whole frame.
        let format = source_texture.format();
        let (block_w, block_h) = format.block_dimensions();
        let partial_depth = format.is_depth_stencil_format()
            && (width != source_texture.width() || height != source_texture.height());
        if source_slice >= source_texture.depth_or_array_layers()
            || target_slice >= target_texture.depth_or_array_layers()
            || target_level >= target_texture.mip_level_count()
            || width % block_w != 0
            || height % block_h != 0
            || partial_depth
        {
            self.log_once(format!("copy skipped: {source:?} slice {source_slice} -> {target:?}"));
            return;
        }
        let aspect = wgpu::TextureAspect::All;
        let encoder = self.encoder();
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &source_texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: source_slice },
                aspect,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &target_texture,
                mip_level: target_level,
                origin: wgpu::Origin3d { x: 0, y: 0, z: target_slice },
                aspect,
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
    }

    /// What `GX2ConvertDepthBufferToTextureSurface` does for titles reading depth in a shader.
    fn copy_depth_to_color(
        &mut self,
        source: SurfaceKey,
        source_slice: u32,
        target: SurfaceKey,
        target_level: u32,
        target_slice: u32,
    ) {
        let format = wgpu_format(target.format);
        let target_view =
            self.target_surface(target, target_level + 1).target_view(target_level, target_slice);
        let Some(source_texture) = self.surfaces.get(&source).map(|s| s.texture.clone()) else {
            return;
        };
        if source_slice >= source_texture.depth_or_array_layers() {
            self.log_once(format!("copy skipped: {source:?} slice {source_slice} -> {target:?}"));
            return;
        }
        let source_view = source_texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2),
            aspect: wgpu::TextureAspect::DepthOnly,
            base_array_layer: source_slice,
            array_layer_count: Some(1),
            mip_level_count: Some(1),
            ..Default::default()
        });
        let device = self.device.clone();
        let (layout, pipeline_layout, module) = self.depth_to_color.get_or_insert_with(|| {
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("shocora depth to color"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                }],
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("shocora depth to color"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("shocora depth to color"),
                source: wgpu::ShaderSource::Wgsl(DEPTH_TO_COLOR_SHADER.into()),
            });
            (layout, pipeline_layout, module)
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shocora depth to color"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&source_view),
            }],
        });
        let pipeline = self
            .depth_to_color_pipelines
            .entry(format)
            .or_insert_with(|| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("shocora depth to color"),
                    layout: Some(pipeline_layout),
                    vertex: wgpu::VertexState {
                        module,
                        entry_point: Some("vs_main"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module,
                        entry_point: Some("fs_main"),
                        compilation_options: Default::default(),
                        targets: &[Some(format.into())],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            })
            .clone();
        let target_size =
            self.surfaces.get(&target).map_or((1, 1), |s| (s.texture.width(), s.texture.height()));
        let width = source_texture.width().min(target_size.0 >> target_level).max(1);
        let height = source_texture.height().min(target_size.1 >> target_level).max(1);
        let encoder = self.encoder();
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shocora depth to color"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.set_viewport(0.0, 0.0, width as f32, height as f32, 0.0, 1.0);
        pass.draw(0..3, 0..1);
    }
}
