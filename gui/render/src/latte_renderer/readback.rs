//! Reading surfaces back to the CPU (screenshots, debugging).
use wiiu_gpu::render::MipData;

use super::*;

impl LatteRenderer {
    /// Render targets drawn since the last call, with their draw counts (tools).
    pub fn take_target_log(&mut self) -> Vec<(SurfaceKey, u32)> {
        let mut list: Vec<_> = std::mem::take(&mut self.target_log).into_iter().collect();
        list.sort_by_key(|(key, _)| key.address);
        list
    }

    /// Reads a surface back as RGBA8 when its format has a simple 8-bit layout (tools).
    pub fn read_surface(&mut self, key: SurfaceKey) -> Option<(u32, u32, Vec<u8>)> {
        self.submit();
        let format = wgpu_format(key.format);
        let texture = self.surfaces.get(&key)?.texture.clone();
        let key = SurfaceKey { width: texture.width(), height: texture.height(), ..key };
        match format {
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => {
                self.read_texture(&texture, key.width, key.height)
            }
            wgpu::TextureFormat::Rgb10a2Unorm
            | wgpu::TextureFormat::Rg11b10Ufloat
            | wgpu::TextureFormat::Rgba16Float
            | wgpu::TextureFormat::Rgba8Snorm
            | wgpu::TextureFormat::R16Float
            | wgpu::TextureFormat::R8Unorm
            | wgpu::TextureFormat::Rg8Unorm
            | wgpu::TextureFormat::Depth16Unorm => {
                if format.is_depth_stencil_format() {
                    return None;
                }
                let (width, height, raw) =
                    self.read_raw(&texture, key.width, key.height, format)?;
                let eight = |v: f32| (v.clamp(0.0, 1.0) * 255.0) as u8;
                let half = |v: u16| half_to_f32(v);
                let rgba = match format {
                    wgpu::TextureFormat::Rgba8Snorm => {
                        raw.iter().map(|v| v.wrapping_add(128)).collect()
                    }
                    wgpu::TextureFormat::R8Unorm => {
                        raw.iter().flat_map(|v| [*v, *v, *v, 255]).collect()
                    }
                    wgpu::TextureFormat::Rg8Unorm => {
                        raw.as_chunks::<2>().0.iter().flat_map(|p| [p[0], p[1], 0, 255]).collect()
                    }
                    wgpu::TextureFormat::R16Float => raw
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .flat_map(|p| {
                            // Linear depth and the like: the fraction shows the structure.
                            let value = half(u16::from_le_bytes(*p)).abs();
                            let v = eight(value.fract());
                            [v, v, eight(value / 256.0), 255]
                        })
                        .collect(),
                    wgpu::TextureFormat::Rgba16Float => raw
                        .as_chunks::<8>()
                        .0
                        .iter()
                        .flat_map(|p| {
                            let c = |i: usize| half(u16::from_le_bytes([p[i], p[i + 1]]));
                            [eight(c(0)), eight(c(2)), eight(c(4)), eight(c(6))]
                        })
                        .collect(),
                    wgpu::TextureFormat::Rgb10a2Unorm => raw
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .flat_map(|p| {
                            let v = u32::from_le_bytes(*p);
                            let c = |shift: u32| ((v >> shift) & 0x3FF) as f32 / 1023.0;
                            [eight(c(0)), eight(c(10)), eight(c(20)), (v >> 30) as u8 * 85]
                        })
                        .collect(),
                    _ => raw
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .flat_map(|p| {
                            let v = u32::from_le_bytes(*p);
                            let c = |shift: u32, bits: u32| {
                                let part = (v >> shift) & ((1 << bits) - 1);
                                half(((part << (15 - bits)) & 0xFFFF) as u16)
                            };
                            [eight(c(0, 11)), eight(c(11, 11)), eight(c(22, 10)), 255]
                        })
                        .collect(),
                };
                Some((width, height, rgba))
            }
            _ => None,
        }
    }

    /// One slice of a surface as the host holds it, without conversion (tools).
    pub fn read_surface_slice(
        &mut self,
        key: SurfaceKey,
        slice: u32,
    ) -> Option<(u32, u32, Vec<u8>)> {
        self.submit();
        let texture = self.surfaces.get(&key)?.texture.clone();
        let bytes = texture.format().block_copy_size(None)?;
        if texture.format().is_compressed() || slice >= texture.depth_or_array_layers() {
            return None;
        }
        self.read_texels_at(&texture, texture.width(), texture.height(), bytes, slice)
    }

    fn read_raw(
        &self,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Option<(u32, u32, Vec<u8>)> {
        let bytes = format.block_copy_size(None)?;
        let (_, _, rows) = self.read_texels(texture, width, height, bytes)?;
        Some((width, height, rows))
    }

    pub(super) fn read_texture(
        &self,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Option<(u32, u32, Vec<u8>)> {
        self.read_texels(texture, width, height, 4)
    }

    fn read_texels(
        &self,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
        bytes: u32,
    ) -> Option<(u32, u32, Vec<u8>)> {
        self.read_texels_at(texture, width, height, bytes, 0)
    }

    fn read_texels_at(
        &self,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
        bytes: u32,
        slice: u32,
    ) -> Option<(u32, u32, Vec<u8>)> {
        self.read_level(texture, (width, height), bytes, 0, slice)
    }

    /// Surfaces as an upload would bring them back, for a saved state. Those drawn larger than
    /// the title thinks are left out: their size means nothing at another scale.
    pub(super) fn read_surfaces(&mut self, keys: &[SurfaceKey]) -> Vec<TextureUpload> {
        self.submit();
        let mut surfaces = Vec::new();
        for key in keys {
            let Some(texture) = self.surfaces.get(key).map(|s| s.texture.clone()) else { continue };
            let format = texture.format();
            let Some(bytes) = format.block_copy_size(None) else { continue };
            if format.is_depth_stencil_format()
                || format.is_compressed()
                || texture.width() != key.width
                || texture.height() != key.height
            {
                continue;
            }
            let three_d = texture.dimension() == wgpu::TextureDimension::D3;
            let dim = match (three_d, texture.depth_or_array_layers()) {
                (true, _) => TextureDim::D3,
                (false, 1) => TextureDim::D2,
                _ => TextureDim::D2Array,
            };
            let mut mips = Vec::new();
            for level in 0..texture.mip_level_count() {
                let size = texture.size().mip_level_size(level, texture.dimension());
                let mut data = Vec::new();
                for slice in 0..size.depth_or_array_layers {
                    let read = (size.width, size.height);
                    let Some((_, _, part)) = self.read_level(&texture, read, bytes, level, slice)
                    else {
                        continue;
                    };
                    data.extend_from_slice(&part);
                }
                mips.push(MipData {
                    width: size.width,
                    height: size.height,
                    layers: size.depth_or_array_layers,
                    data,
                });
            }
            surfaces.push(TextureUpload { key: *key, dim, mips });
        }
        surfaces
    }

    fn read_level(
        &self,
        texture: &wgpu::Texture,
        (width, height): (u32, u32),
        bytes: u32,
        level: u32,
        slice: u32,
    ) -> Option<(u32, u32, Vec<u8>)> {
        let row = (width * bytes).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shocora readback"),
            size: u64::from(row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("shocora readback"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                mip_level: level,
                origin: wgpu::Origin3d { x: 0, y: 0, z: slice },
                ..texture.as_image_copy()
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let data = slice.get_mapped_range().ok()?;
        let mut out = Vec::with_capacity((width * height * bytes) as usize);
        for line in data.chunks_exact(row as usize) {
            out.extend_from_slice(&line[..(width * bytes) as usize]);
        }
        drop(data);
        buffer.unmap();
        Some((width, height, out))
    }

    /// Reads a screen back as RGBA8 (tools and tests).
    #[inline]
    pub fn read_screen(&mut self, screen: Screen) -> Option<(u32, u32, Vec<u8>)> {
        self.submit();
        let image = self.screens.get(screen)?;
        self.read_texture(&image.texture, image.width, image.height)
    }
}

/// Decodes a 16-bit float; small floats of packed formats are shifted up to this layout.
fn half_to_f32(value: u16) -> f32 {
    let sign = if value & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from((value >> 10) & 0x1F);
    let mantissa = f32::from(value & 0x3FF) / 1024.0;
    sign * match exponent {
        0 => mantissa * 2.0_f32.powi(-14),
        0x1F => f32::INFINITY,
        _ => (1.0 + mantissa) * 2.0_f32.powi(exponent - 15),
    }
}
