//! One piece of guest memory drawn in two formats of the same texel size.
//!
//! Latte has no formats in memory: Xenoblade Chronicles X accumulates light into its
//! buffer as `R11G11B10_FLOAT` and then reads the same memory as `R8G8B8A8_UNORM`, decoding
//! the bits itself. Cemu views one texture in both formats. Here each format is its own
//! host surface, so when a surface is used, the bits of a newer one at the same place are
//! brought over first: a pass reads each texel, takes it apart into its bits and writes
//! them as the other format.
use super::*;

/// Where in guest memory a surface lies, regardless of its format.
type Place = (u32, u32, u32, u32);

fn place(key: &SurfaceKey) -> Place {
    (key.address, key.width, key.height, key.layers)
}

/// The formats whose texels are 32 bits that a pass can take apart and put together.
fn bits_of(format: HostFormat) -> Option<(&'static str, &'static str)> {
    use HostFormat as H;
    Some(match format {
        H::Rgba8Unorm => ("pack4x8unorm(v)", "unpack4x8unorm(b)"),
        H::Rgba8Snorm => ("pack4x8snorm(v)", "unpack4x8snorm(b)"),
        H::Rg16Float => ("pack2x16float(v.xy)", "vec4<f32>(unpack2x16float(b), 0.0, 1.0)"),
        H::Rg16Unorm => ("pack2x16unorm(v.xy)", "vec4<f32>(unpack2x16unorm(b), 0.0, 1.0)"),
        H::R32Float => ("bitcast<u32>(v.x)", "vec4<f32>(bitcast<f32>(b), 0.0, 0.0, 1.0)"),
        H::Rgb10a2Unorm => ("pack_rgb10a2(v)", "unpack_rgb10a2(b)"),
        H::Rg11b10Float => ("pack_rg11b10(v)", "unpack_rg11b10(b)"),
        _ => return None,
    })
}

const HELPERS: &str = r"
@group(0) @binding(0) var source: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

// Unsigned floats of `mantissa` bits and a 5-bit exponent (the parts of R11G11B10).
fn pack_small(value: f32, mantissa: u32) -> u32 {
    let v = max(value, 0.0);
    if (v == 0.0 || v != v) { return 0u; }
    let bits = bitcast<u32>(v);
    let exponent = i32((bits >> 23u) & 0xFFu) - 127 + 15;
    if (exponent >= 31) { return (31u << mantissa); }
    if (exponent <= 0) {
        let full = (bits & 0x7FFFFFu) | 0x800000u;
        return full >> u32(24 - i32(mantissa) - exponent);
    }
    return (u32(exponent) << mantissa) | ((bits & 0x7FFFFFu) >> (23u - mantissa));
}
fn unpack_small(bits: u32, mantissa: u32) -> f32 {
    let exponent = bits >> mantissa;
    let fraction = f32(bits & ((1u << mantissa) - 1u)) / f32(1u << mantissa);
    if (exponent == 0u) { return fraction * exp2(-14.0); }
    if (exponent == 31u) { return 65504.0; }
    return (1.0 + fraction) * exp2(f32(exponent) - 15.0);
}
fn pack_rg11b10(v: vec4<f32>) -> u32 {
    return pack_small(v.x, 6u) | (pack_small(v.y, 6u) << 11u) | (pack_small(v.z, 5u) << 22u);
}
fn unpack_rg11b10(b: u32) -> vec4<f32> {
    return vec4<f32>(
        unpack_small(b & 0x7FFu, 6u),
        unpack_small((b >> 11u) & 0x7FFu, 6u),
        unpack_small(b >> 22u, 5u),
        1.0,
    );
}
fn pack_rgb10a2(v: vec4<f32>) -> u32 {
    let c = vec4<u32>(round(clamp(v, vec4<f32>(0.0), vec4<f32>(1.0)) * vec4<f32>(1023.0, 1023.0, 1023.0, 3.0)));
    return c.x | (c.y << 10u) | (c.z << 20u) | (c.w << 30u);
}
fn unpack_rgb10a2(b: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(b & 0x3FFu) / 1023.0,
        f32((b >> 10u) & 0x3FFu) / 1023.0,
        f32((b >> 20u) & 0x3FFu) / 1023.0,
        f32(b >> 30u) / 3.0,
    );
}
";

fn shader(source: HostFormat, target: HostFormat) -> Option<String> {
    let (pack, _) = bits_of(source)?;
    let (_, unpack) = bits_of(target)?;
    Some(format!(
        "{HELPERS}
@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {{
    let v = textureLoad(source, vec2<i32>(position.xy), 0);
    let b = {pack};
    return {unpack};
}}
"
    ))
}

/// Which surfaces were drawn last at each place, and the passes moving bits between them.
#[derive(Default)]
pub(super) struct Reinterpret {
    /// When each surface was last written: by a draw, a clear, a copy or an upload.
    stamps: HashMap<SurfaceKey, u64>,
    /// The surfaces written at each place.
    places: HashMap<Place, Vec<SurfaceKey>>,
    next: u64,
    layout: Option<(wgpu::BindGroupLayout, wgpu::PipelineLayout)>,
    pipelines: HashMap<(HostFormat, HostFormat), Option<wgpu::RenderPipeline>>,
}

impl Reinterpret {
    /// Forgets every surface (they are gone).
    pub(super) fn clear(&mut self) {
        self.stamps.clear();
        self.places.clear();
    }

    /// `key` was just written.
    pub(super) fn written(&mut self, key: SurfaceKey) {
        if bits_of(key.format).is_none() {
            return;
        }
        self.next += 1;
        if self.stamps.insert(key, self.next).is_none() {
            let keys = self.places.entry(place(&key)).or_default();
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }

    /// The newest other format at `key`'s place, when it is newer than `key`.
    fn newer(&self, key: &SurfaceKey) -> Option<SurfaceKey> {
        bits_of(key.format)?;
        let own = self.stamps.get(key).copied().unwrap_or(0);
        self.places
            .get(&place(key))?
            .iter()
            .filter(|other| other.format != key.format)
            .filter_map(|other| Some((*other, *self.stamps.get(other)?)))
            .filter(|(_, stamp)| *stamp > own)
            .max_by_key(|(_, stamp)| *stamp)
            .map(|(other, _)| other)
    }
}

impl LatteRenderer {
    /// Brings `key` up to date with what was drawn at its place in another format.
    pub(super) fn sync_format(&mut self, key: SurfaceKey) {
        let Some(source) = self.reinterpret.newer(&key) else { return };
        let stamp = self.reinterpret.stamps.get(&source).copied().unwrap_or(0);
        if self.reinterpret_copy(source, key) {
            self.feedback_sync.remove(&key);
        }
        // Even when it could not be copied: asking again each draw would not help.
        if let Some(own) = self.reinterpret.stamps.get_mut(&key) {
            *own = stamp;
        } else {
            self.reinterpret.stamps.insert(key, stamp);
            let keys = self.reinterpret.places.entry(place(&key)).or_default();
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }

    /// Writes the bits of `source` into `target`, texel by texel, for every layer.
    fn reinterpret_copy(&mut self, source: SurfaceKey, target: SurfaceKey) -> bool {
        let Some(source_texture) = self.surfaces.get(&source).map(|s| s.texture.clone()) else {
            return false;
        };
        let target_texture = self.target_surface(target, 1).texture.clone();
        if source_texture.width() != target_texture.width()
            || source_texture.height() != target_texture.height()
        {
            return false;
        }
        let pair = (source.format, target.format);
        let device = self.device.clone();
        let (layout, pipeline_layout) = self.reinterpret.layout.get_or_insert_with(|| {
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("shocora reinterpret"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                }],
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("shocora reinterpret"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            (layout, pipeline_layout)
        });
        let (layout, pipeline_layout) = (layout.clone(), pipeline_layout.clone());
        let pipeline = self
            .reinterpret
            .pipelines
            .entry(pair)
            .or_insert_with(|| {
                let code = shader(pair.0, pair.1)?;
                let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("shocora reinterpret"),
                    source: wgpu::ShaderSource::Wgsl(code.into()),
                });
                Some(device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("shocora reinterpret"),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &module,
                        entry_point: Some("vs_main"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &module,
                        entry_point: Some("fs_main"),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu_format(pair.1).into())],
                    }),
                    multiview_mask: None,
                    cache: None,
                }))
            })
            .clone();
        let Some(pipeline) = pipeline else { return false };
        self.end_pass();
        self.counters.copies += 1;
        let layers =
            source_texture.depth_or_array_layers().min(target_texture.depth_or_array_layers());
        for layer in 0..layers {
            let source_view = source_texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: layer,
                array_layer_count: Some(1),
                base_mip_level: 0,
                mip_level_count: Some(1),
                ..Default::default()
            });
            let target_view = target_texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: layer,
                array_layer_count: Some(1),
                base_mip_level: 0,
                mip_level_count: Some(1),
                ..Default::default()
            });
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("shocora reinterpret"),
                layout: &layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source_view),
                }],
            });
            let encoder = self.encoder();
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shocora reinterpret"),
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
            pass.draw(0..3, 0..1);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pair_of_32_bit_formats_has_a_shader() {
        let formats = [HostFormat::Rgba8Unorm, HostFormat::Rg11b10Float, HostFormat::R32Float];
        for source in formats {
            for target in formats {
                assert!(shader(source, target).is_some());
            }
        }
    }

    #[test]
    fn a_surface_is_brought_up_to_date_with_a_newer_format() {
        let key =
            |format| SurfaceKey { address: 0x70_a000, width: 768, height: 620, layers: 1, format };
        let rgba = key(HostFormat::Rgba8Unorm);
        let light = key(HostFormat::Rg11b10Float);
        let mut r = Reinterpret::default();
        r.written(rgba);
        assert_eq!(r.newer(&light), Some(rgba));
        r.written(light);
        assert_eq!(r.newer(&rgba), Some(light));
        assert_eq!(r.newer(&light), None);
        // Another place is not affected.
        let other = SurfaceKey { address: 0x80_0000, ..rgba };
        assert_eq!(r.newer(&other), None);
    }
}
