//! Host textures behind guest surfaces: creation, uploads, clears, samplers.
use super::*;

pub(super) struct HostSurface {
    pub(super) texture: wgpu::Texture,
    views: HashMap<(TextureDim, u32, u32, u32, u32), wgpu::TextureView>,
    target_views: HashMap<(u32, u32), wgpu::TextureView>,
    mip_count: u32,
    /// Bumped when the texture object changes, to invalidate bind groups.
    pub(super) id: u64,
}

impl HostSurface {
    pub(super) fn sample_view(
        &mut self,
        key: &SurfaceKey,
        bound: &BoundTexture,
    ) -> wgpu::TextureView {
        let cache =
            (bound.dim, bound.base_level, bound.level_count, bound.base_layer, bound.layer_count);
        let mip_count = self.mip_count;
        let texture = &self.texture;
        self.views
            .entry(cache)
            .or_insert_with(|| {
                let aspect = if key.format.is_depth() && key.format.has_stencil() {
                    wgpu::TextureAspect::DepthOnly
                } else {
                    wgpu::TextureAspect::All
                };
                let base_mip_level = bound.base_level.min(mip_count - 1);
                let layers = if texture.dimension() == wgpu::TextureDimension::D3 {
                    1
                } else {
                    texture.depth_or_array_layers()
                };
                let base_layer = bound.base_layer.min(layers - 1);
                texture.create_view(&wgpu::TextureViewDescriptor {
                    label: None,
                    format: None,
                    dimension: Some(view_dimension(bound.dim)),
                    usage: None,
                    aspect,
                    base_mip_level,
                    mip_level_count: Some(bound.level_count.min(mip_count - base_mip_level).max(1)),
                    base_array_layer: match bound.dim {
                        TextureDim::D3 => 0,
                        TextureDim::Cube => base_layer.min(layers.saturating_sub(6)),
                        TextureDim::D2 | TextureDim::D2Array => base_layer,
                    },
                    array_layer_count: match bound.dim {
                        TextureDim::Cube => Some(6),
                        TextureDim::D2Array => {
                            Some(bound.layer_count.min(layers - base_layer).max(1))
                        }
                        // One slice of an array is a 2D texture.
                        TextureDim::D2 => Some(1),
                        TextureDim::D3 => None,
                    },
                })
            })
            .clone()
    }

    pub(super) fn target_view(&mut self, level: u32, slice: u32) -> wgpu::TextureView {
        let texture = &self.texture;
        let slice = slice.min(texture.depth_or_array_layers() - 1);
        let level = level.min(self.mip_count - 1);
        self.target_views
            .entry((level, slice))
            .or_insert_with(|| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_mip_level: level,
                    mip_level_count: Some(1),
                    base_array_layer: slice,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .clone()
    }
}

impl LatteRenderer {
    fn surface(&mut self, key: SurfaceKey, dim: TextureDim, mip_count: u32) -> &mut HostSurface {
        let stale = self.surfaces.get(&key).is_some_and(|s| s.mip_count < mip_count);
        if stale {
            self.surfaces.remove(&key);
        }
        let device = &self.device;
        let id = self.next_surface_id;
        let scale = if self.uploaded.contains(&key) { 1 } else { self.resolution.of(&key) };
        // Large surfaces may not fit: the one who chose them gets to know.
        let scope = (scale > 1 && !self.surfaces.contains_key(&key))
            .then(|| device.push_error_scope(wgpu::ErrorFilter::OutOfMemory));
        let surface = self.surfaces.entry(key).or_insert_with(|| {
            let format = wgpu_format(key.format);
            let renderable = format.is_depth_stencil_format() || !format.is_compressed();
            let mut usage = wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC;
            if renderable && dim != TextureDim::D3 {
                usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
            }
            let (dimension, layers) = match dim {
                TextureDim::D3 => (wgpu::TextureDimension::D3, key.layers.max(1)),
                TextureDim::Cube => (wgpu::TextureDimension::D2, 6),
                // A surface drawn to first is seen as 2D, but titles draw to every
                // slice of an array: all its layers exist from the start.
                TextureDim::D2Array | TextureDim::D2 => {
                    (wgpu::TextureDimension::D2, key.layers.max(1))
                }
            };
            let (mut width, mut height) = (key.width.max(1) * scale, key.height.max(1) * scale);
            if format.is_compressed() {
                width = width.next_multiple_of(4);
                height = height.next_multiple_of(4);
            }
            let max_levels = (32 - width.max(height).leading_zeros()).max(1);
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("shocora surface"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: layers },
                mip_level_count: mip_count.clamp(1, max_levels),
                sample_count: 1,
                dimension,
                format,
                usage,
                view_formats: &[],
            });
            HostSurface {
                texture,
                views: HashMap::new(),
                target_views: HashMap::new(),
                mip_count: mip_count.clamp(1, max_levels),
                id,
            }
        });
        if let Some(error) = scope.and_then(|scope| pollster::block_on(scope.pop())) {
            log::warn!("{key:?} does not fit into video memory at {scale}x: {error}");
            self.memory_watch.report(scale);
        }
        if surface.id == id {
            self.next_surface_id += 1;
        }
        surface
    }

    /// The surface `key` to draw to. One that was a texture so far becomes a target like any
    /// other, as all attachments of a draw must grow together.
    pub(super) fn target_surface(&mut self, key: SurfaceKey, mip_count: u32) -> &mut HostSurface {
        self.drawn_to.insert(key);
        if self.resolution.applies(&key) && self.uploaded.remove(&key) {
            self.surfaces.remove(&key);
        }
        self.surface(key, TextureDim::D2, mip_count)
    }

    /// Draws at `resolution` from now on. Targets of the old size are gone: titles draw most
    /// of them again in the next frame.
    pub(super) fn set_resolution(&mut self, resolution: ResolutionScale) {
        self.submit();
        let old = self.resolution;
        let uploaded = &self.uploaded;
        self.surfaces.retain(|key, _| {
            uploaded.contains(key) || (!old.applies(key) && !resolution.applies(key))
        });
        self.bind_groups.clear();
        self.feedback_copies.clear();
        self.resolution = resolution;
    }

    /// Lets go of a texture, which the next use uploads again. What became a render target in
    /// the meantime stays: guest memory does not have its contents.
    pub(super) fn forget_texture(&mut self, key: SurfaceKey) {
        if self.uploaded.remove(&key) && !self.drawn_to.contains(&key) {
            self.surfaces.remove(&key);
            self.feedback_copies.remove(&key);
            // They would keep the texture alive.
            self.bind_groups.clear();
        }
    }

    pub(super) fn upload(&mut self, upload: &TextureUpload) {
        if self.upload_in_frame(upload) {
            return;
        }
        // `write_texture` happens before recorded commands that use the old contents.
        self.submit();
        let format = wgpu_format(upload.key.format);
        let mip_count = upload.mips.len() as u32;
        let queue = self.queue.clone();
        self.uploaded.insert(upload.key);
        let surface = self.surface(upload.key, upload.dim, mip_count);
        let texture = surface.texture.clone();
        let surface_id = surface.id;
        let block = format.block_dimensions();
        let block_bytes = format.block_copy_size(None).unwrap_or(4);
        for (level, mip) in upload.mips.iter().enumerate() {
            let level = level as u32;
            if level >= texture.mip_level_count() || mip.data.is_empty() {
                continue;
            }
            let blocks_x = mip.width.div_ceil(block.0);
            let blocks_y = mip.height.div_ceil(block.1);
            let extent_width = mip.width.next_multiple_of(block.0);
            let extent_height = mip.height.next_multiple_of(block.1);
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &mip.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(blocks_x * block_bytes),
                    rows_per_image: Some(blocks_y),
                },
                wgpu::Extent3d {
                    width: extent_width,
                    height: extent_height,
                    depth_or_array_layers: mip.layers.max(1),
                },
            );
        }
        let _ = surface_id;
    }

    /// Records the upload among the frame's commands, through the staging ring, as Cemu
    /// uploads in its command buffer: submitting all that came before for every texture
    /// took most of the render thread's time. Returns `false` when the upload does not fit
    /// (it then goes the old way).
    fn upload_in_frame(&mut self, upload: &TextureUpload) -> bool {
        const ROW_ALIGN: u64 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64;
        const OFFSET_ALIGN: u64 = 512;
        let format = wgpu_format(upload.key.format);
        if format.has_depth_aspect() || format.has_stencil_aspect() {
            return false;
        }
        let block = format.block_dimensions();
        let block_bytes = u64::from(format.block_copy_size(None).unwrap_or(4));
        // Each level: (rows of blocks, packed row bytes, padded row bytes, layers).
        let levels: Vec<(u64, u64, u64, u64)> = upload
            .mips
            .iter()
            .map(|mip| {
                let row = u64::from(mip.width.div_ceil(block.0)) * block_bytes;
                let rows = u64::from(mip.height.div_ceil(block.1));
                (rows, row, row.next_multiple_of(ROW_ALIGN), u64::from(mip.layers.max(1)))
            })
            .collect();
        let total: u64 = levels
            .iter()
            .map(|(rows, _, padded, layers)| {
                (rows * padded * layers).next_multiple_of(OFFSET_ALIGN)
            })
            .sum();
        if total == 0 || total > UPLOAD_RING_SIZE / 2 {
            return false;
        }
        // Data shorter than its size says goes the old way, which copes with it.
        if upload.mips.iter().zip(&levels).any(|(mip, (rows, row, _, layers))| {
            !mip.data.is_empty() && (mip.data.len() as u64) < rows * row * layers
        }) {
            return false;
        }
        if self.upload_cursor + total > UPLOAD_RING_SIZE {
            self.submit();
        }
        let mip_count = upload.mips.len() as u32;
        self.uploaded.insert(upload.key);
        let texture = self.surface(upload.key, upload.dim, mip_count).texture.clone();
        self.end_pass();
        let queue = self.queue.clone();
        let ring = self.upload_ring.clone();
        for (level, (mip, &(rows, row, padded, layers))) in
            upload.mips.iter().zip(&levels).enumerate()
        {
            let level = level as u32;
            if level >= texture.mip_level_count() || mip.data.is_empty() {
                continue;
            }
            let size = rows * padded * layers;
            let offset = self.upload_cursor;
            self.upload_cursor += size.next_multiple_of(OFFSET_ALIGN);
            let Some(size) = wgpu::BufferSize::new(size) else { continue };
            if let Some(mut view) = queue.write_buffer_with(&ring, offset, size) {
                if padded == row {
                    view.copy_from_slice(&mip.data[..size.get() as usize]);
                } else {
                    for (index, source) in mip
                        .data
                        .chunks_exact(row as usize)
                        .take((rows * layers) as usize)
                        .enumerate()
                    {
                        let at = index * padded as usize;
                        view.slice(at..at + row as usize).copy_from_slice(source);
                    }
                }
            } else {
                continue;
            }
            self.encoder().copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &ring,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset,
                        bytes_per_row: Some(padded as u32),
                        rows_per_image: Some(rows as u32),
                    },
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: mip.width.next_multiple_of(block.0),
                    height: mip.height.next_multiple_of(block.1),
                    depth_or_array_layers: mip.layers.max(1),
                },
            );
        }
        true
    }

    pub(super) fn clear(
        &mut self,
        color: Option<(SurfaceKey, u32, [f32; 4])>,
        depth: Option<wiiu_gpu::render::DepthClear>,
    ) {
        self.end_pass();
        let color_view = color.map(|(key, slice, _)| {
            self.target_surface(key, 1).target_view(0, slice.min(key.layers.saturating_sub(1)))
        });
        let depth_view = depth.as_ref().map(|d| {
            self.target_surface(d.surface, 1)
                .target_view(0, d.slice.min(d.surface.layers.saturating_sub(1)))
        });
        let color_attachment =
            color.as_ref().zip(color_view.as_ref()).map(|((key, _, rgba), view)| {
                wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear_color(*key, *rgba)),
                        store: wgpu::StoreOp::Store,
                    },
                }
            });
        let depth_attachment = depth.as_ref().zip(depth_view.as_ref()).map(|(d, view)| {
            wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: Some(wgpu::Operations {
                    load: d.depth.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: d.surface.format.has_stencil().then_some(wgpu::Operations {
                    load: d.stencil.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear),
                    store: wgpu::StoreOp::Store,
                }),
            }
        });
        if color_attachment.is_none() && depth_attachment.is_none() {
            return;
        }
        let encoder = self.encoder();
        drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shocora clear"),
            color_attachments: &[color_attachment],
            depth_stencil_attachment: depth_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }));
    }
}

impl LatteRenderer {
    pub(super) fn sampler(&mut self, desc: SamplerDesc) -> wgpu::Sampler {
        let device = &self.device;
        let border = device.features().contains(wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER);
        self.samplers
            .entry(desc)
            .or_insert_with(|| {
                let filter = |linear: bool| {
                    if linear { wgpu::FilterMode::Linear } else { wgpu::FilterMode::Nearest }
                };
                // The host only filters anisotropically between mips, too.
                let anisotropy = desc.anisotropy.max(1);
                device.create_sampler(&wgpu::SamplerDescriptor {
                    label: Some("shocora sampler"),
                    address_mode_u: address_mode(desc.address[0], border),
                    address_mode_v: address_mode(desc.address[1], border),
                    address_mode_w: address_mode(desc.address[2], border),
                    mag_filter: filter(desc.mag_linear),
                    min_filter: filter(desc.min_linear),
                    mipmap_filter: if desc.mip_linear || anisotropy > 1 {
                        wgpu::MipmapFilterMode::Linear
                    } else {
                        wgpu::MipmapFilterMode::Nearest
                    },
                    lod_min_clamp: f32::from(desc.min_lod) / 64.0,
                    lod_max_clamp: (f32::from(desc.max_lod) / 64.0)
                        .max(f32::from(desc.min_lod) / 64.0),
                    compare: desc.compare.map(compare_function),
                    anisotropy_clamp: u16::from(anisotropy),
                    border_color: desc.address.contains(&AddressMode::Border).then_some(match desc
                        .border
                    {
                        BorderColor::TransparentBlack => wgpu::SamplerBorderColor::TransparentBlack,
                        BorderColor::OpaqueBlack => wgpu::SamplerBorderColor::OpaqueBlack,
                        BorderColor::OpaqueWhite => wgpu::SamplerBorderColor::OpaqueWhite,
                    }),
                })
            })
            .clone()
    }

    pub(super) fn dummy_texture(
        &mut self,
        dim: TextureDim,
        kind: TextureKind,
    ) -> wgpu::TextureView {
        let device = self.device.clone();
        let queue = self.queue.clone();
        self.dummy_textures
            .entry((dim, kind))
            .or_insert_with(|| {
                let format = match kind {
                    TextureKind::Float => wgpu::TextureFormat::Rgba8Unorm,
                    TextureKind::Depth => wgpu::TextureFormat::Depth32Float,
                    TextureKind::Uint => wgpu::TextureFormat::Rgba8Uint,
                    TextureKind::Sint => wgpu::TextureFormat::Rgba8Sint,
                };
                let layers = if dim == TextureDim::Cube { 6 } else { 1 };
                let texture = device.create_texture_with_data(
                    &queue,
                    &wgpu::TextureDescriptor {
                        label: Some("shocora dummy"),
                        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: layers },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: if dim == TextureDim::D3 {
                            wgpu::TextureDimension::D3
                        } else {
                            wgpu::TextureDimension::D2
                        },
                        format,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::COPY_DST
                            | wgpu::TextureUsages::RENDER_ATTACHMENT,
                        view_formats: &[],
                    },
                    wgpu::util::TextureDataOrder::LayerMajor,
                    &vec![
                        0;
                        (format.block_copy_size(Some(wgpu::TextureAspect::All)).unwrap_or(4)
                            * layers) as usize
                    ],
                );
                texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(view_dimension(dim)),
                    array_layer_count: if dim == TextureDim::Cube {
                        Some(6)
                    } else if dim == TextureDim::D2Array {
                        Some(1)
                    } else {
                        None
                    },
                    ..Default::default()
                })
            })
            .clone()
    }
}

fn clear_color(key: SurfaceKey, rgba: [f32; 4]) -> wgpu::Color {
    let _ = key;
    wgpu::Color {
        r: f64::from(rgba[0]),
        g: f64::from(rgba[1]),
        b: f64::from(rgba[2]),
        a: f64::from(rgba[3]),
    }
}
