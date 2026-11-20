//! xBRZ post-processing of the finished screen images, on the GPU.
//!
//! ```text
//!  screen target ──xBRZ fragment shader──► upscaled texture ──► screen shown by the UI
//! ```
//!
//! A shader runs within the frame it belongs to, whatever the factor: on the CPU,
//! xBRZ cannot keep up with 60 frames of 720p and takes the cores from the guest.
#![allow(clippy::cast_possible_truncation)]

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
};

use egui_wgpu::wgpu;
use wiiu_gpu::Screen;

use crate::{ScreenImage, ScreenImages};

/// Largest factor offered.
pub const MAX_FACTOR: u8 = 6;

/// Which algorithm upscales the screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpscaleMethod {
    #[default]
    None,
    Xbrz,
}

/// Persisted upscaling settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct UpscaleConfig {
    pub method: UpscaleMethod,
    /// Output size relative to the guest's (2..=6).
    pub factor: u8,
}

impl Default for UpscaleConfig {
    fn default() -> Self {
        Self { method: UpscaleMethod::None, factor: 2 }
    }
}

impl UpscaleConfig {
    /// `1` when the screens stay as they are.
    pub const fn effective_factor(self) -> u8 {
        if !available() {
            return 1;
        }
        match self.method {
            UpscaleMethod::None => 1,
            UpscaleMethod::Xbrz => {
                if self.factor < 2 {
                    1
                } else if self.factor > MAX_FACTOR {
                    MAX_FACTOR
                } else {
                    self.factor
                }
            }
        }
    }
}

/// Whether this build contains xBRZ (GPL-3.0-only, behind the `xbrz` feature).
#[inline]
pub const fn available() -> bool {
    cfg!(feature = "xbrz")
}

/// The factor in effect, changeable from any thread while the renderer runs.
#[derive(Debug, Clone)]
pub struct UpscaleSetting {
    factor: Arc<AtomicU8>,
    shown: Arc<AtomicU64>,
}

impl Default for UpscaleSetting {
    fn default() -> Self {
        Self { factor: Arc::new(AtomicU8::new(1)), shown: Arc::new(AtomicU64::new(0)) }
    }
}

impl UpscaleSetting {
    #[inline]
    pub fn set(&self, config: UpscaleConfig) {
        self.factor.store(config.effective_factor(), Ordering::Relaxed);
    }

    fn factor(&self) -> u8 {
        self.factor.load(Ordering::Relaxed)
    }

    /// Upscaled images made so far.
    #[inline]
    pub fn images_shown(&self) -> u64 {
        self.shown.load(Ordering::Relaxed)
    }
}

#[cfg(feature = "xbrz")]
const SHADER: &str = include_str!("xbrz.wgsl");
// The feature decides whether this GPL code is part of the build at all.
#[cfg(not(feature = "xbrz"))]
const SHADER: &str = "";

/// xBRZ for the screens of one renderer.
#[derive(Default)]
pub struct Upscaler {
    setting: UpscaleSetting,
    /// Made on first use: most sessions never upscale.
    pipeline: Option<(wgpu::RenderPipeline, wgpu::BindGroupLayout)>,
    images: HashMap<Screen, ScreenImage>,
}

impl std::fmt::Debug for Upscaler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upscaler").field("setting", &self.setting).finish_non_exhaustive()
    }
}

fn create_pipeline(device: &wgpu::Device) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("shocora xBRZ"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        }],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("shocora xBRZ"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("shocora xBRZ"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("shocora xBRZ"),
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
            targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())],
        }),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, layout)
}

impl Upscaler {
    #[inline]
    pub fn setting(&self) -> UpscaleSetting {
        self.setting.clone()
    }

    /// `true` while the UI should see the upscaled image of `screen`, not the native one.
    #[inline]
    pub fn shows(&self, screen: Screen) -> bool {
        self.active() && self.images.contains_key(&screen)
    }

    /// Whether [`Self::update`] has work, and needs the flip's commands submitted first.
    pub(crate) fn active(&self) -> bool {
        self.setting.factor() > 1
    }

    /// Upscales the new images. Call after a flip's work was submitted; `presented`
    /// lists the screens that have one.
    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        presented: &[Screen],
        targets: &HashMap<Screen, ScreenImage>,
        shown: &ScreenImages,
        generation: &mut u64,
    ) {
        let factor = self.setting.factor();
        if factor <= 1 {
            self.switch_off(targets, shown);
            return;
        }
        if presented.is_empty() {
            return;
        }
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("shocora xBRZ"),
        });
        for screen in presented {
            if let Some(target) = targets.get(screen) {
                self.upscale(device, &mut encoder, *screen, target, factor, shown, generation);
            }
        }
        queue.submit([encoder.finish()]);
    }

    /// Back to the native images as soon as the setting says so.
    fn switch_off(&mut self, targets: &HashMap<Screen, ScreenImage>, shown: &ScreenImages) {
        for (screen, _) in self.images.drain() {
            if let Some(target) = targets.get(&screen) {
                shown.set(screen, target.clone());
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn upscale(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        screen: Screen,
        target: &ScreenImage,
        factor: u8,
        shown: &ScreenImages,
        generation: &mut u64,
    ) {
        let largest = device.limits().max_texture_dimension_2d;
        let factor = u32::from(factor).min(largest / target.width.max(target.height).max(1)).max(1);
        let (width, height) = (target.width * factor, target.height * factor);
        let stale = self.images.get(&screen).is_none_or(|i| i.width != width || i.height != height);
        if stale {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("shocora upscaled screen"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            *generation += 1;
            let image = ScreenImage { view, texture, width, height, generation: *generation };
            shown.set(screen, image.clone());
            self.images.insert(screen, image);
        }
        let Some(image) = self.images.get(&screen) else { return };
        let (pipeline, layout) = self.pipeline.get_or_insert_with(|| create_pipeline(device));
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shocora xBRZ"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&target.view),
            }],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shocora xBRZ"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &image.view,
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
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);
        self.setting.shown.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factors_stay_in_the_range_offered() {
        let config = |method, factor| UpscaleConfig { method, factor }.effective_factor();
        assert_eq!(config(UpscaleMethod::None, 4), 1);
        if available() {
            assert_eq!(config(UpscaleMethod::Xbrz, 0), 1);
            assert_eq!(config(UpscaleMethod::Xbrz, 3), 3);
            assert_eq!(config(UpscaleMethod::Xbrz, 200), MAX_FACTOR);
        }
    }

    /// `None` on a machine without a usable GPU.
    #[cfg(feature = "xbrz")]
    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    }

    #[cfg(feature = "xbrz")]
    fn image(device: &wgpu::Device, queue: &wgpu::Queue, size: u32, rgba: &[u8]) -> ScreenImage {
        let extent = wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            texture.as_image_copy(),
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size * 4),
                rows_per_image: None,
            },
            extent,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        ScreenImage { view, texture, width: size, height: size, generation: 1 }
    }

    #[cfg(feature = "xbrz")]
    fn read(device: &wgpu::Device, queue: &wgpu::Queue, image: &ScreenImage) -> Vec<u8> {
        let row = (image.width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(row) * u64::from(image.height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            image.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d { width: image.width, height: image.height, depth_or_array_layers: 1 },
        );
        queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let data = buffer.slice(..).get_mapped_range().unwrap();
        data.chunks_exact(row as usize)
            .flat_map(|line| line[..(image.width * 4) as usize].iter().copied())
            .collect()
    }

    #[cfg(feature = "xbrz")]
    #[test]
    fn smooths_a_diagonal_and_leaves_flat_areas_alone() {
        const SIZE: u32 = 16;
        const FACTOR: u32 = 4;
        let Some((device, queue)) = device() else {
            eprintln!("no GPU: test skipped");
            return;
        };
        // Black below the diagonal, white above it.
        let source: Vec<u8> = (0..SIZE * SIZE)
            .flat_map(|at| {
                let value = if at % SIZE > at / SIZE { 255 } else { 0 };
                [value, value, value, 255]
            })
            .collect();
        let target = image(&device, &queue, SIZE, &source);
        let targets = HashMap::from([(Screen::Tv, target)]);
        let shown = ScreenImages::default();
        let mut upscaler = Upscaler::default();
        upscaler.setting().set(UpscaleConfig { method: UpscaleMethod::Xbrz, factor: FACTOR as u8 });
        let mut generation = 1;
        upscaler.update(&device, &queue, &[Screen::Tv], &targets, &shown, &mut generation);

        let result = shown.get(Screen::Tv).unwrap();
        assert_eq!((result.width, result.height), (SIZE * FACTOR, SIZE * FACTOR));
        let pixels = read(&device, &queue, &result);
        let value = |x: u32, y: u32| pixels[((y * SIZE * FACTOR + x) * 4) as usize];
        // Flat areas keep their exact colours, and the image is opaque.
        assert_eq!(value(SIZE * FACTOR - 2, 1), 255);
        assert_eq!(value(1, SIZE * FACTOR - 2), 0);
        assert!(pixels.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255));
        // A plain enlargement would be a staircase of whole source pixels: here,
        // inside the source pixels on the edge, some output pixels differ from E.
        let source_at = |x: u32, y: u32| source[(((y / FACTOR) * SIZE + x / FACTOR) * 4) as usize];
        let smoothed = (0..SIZE * FACTOR)
            .flat_map(|y| (0..SIZE * FACTOR).map(move |x| (x, y)))
            .filter(|&(x, y)| value(x, y) != source_at(x, y))
            .count();
        assert!(smoothed > (SIZE * FACTOR) as usize, "only {smoothed} output pixels changed");
    }

    #[cfg(feature = "xbrz")]
    #[test]
    fn switching_off_brings_back_the_native_image() {
        let Some((device, queue)) = device() else {
            eprintln!("no GPU: test skipped");
            return;
        };
        let target = image(&device, &queue, 8, &[128; 8 * 8 * 4]);
        let targets = HashMap::from([(Screen::Drc, target)]);
        let shown = ScreenImages::default();
        let mut upscaler = Upscaler::default();
        let mut generation = 1;
        let config = UpscaleConfig { method: UpscaleMethod::Xbrz, factor: 2 };
        upscaler.setting().set(config);
        upscaler.update(&device, &queue, &[Screen::Drc], &targets, &shown, &mut generation);
        assert!(upscaler.shows(Screen::Drc));
        assert_eq!(shown.get(Screen::Drc).unwrap().width, 16);

        upscaler.setting().set(UpscaleConfig { method: UpscaleMethod::None, factor: 2 });
        upscaler.update(&device, &queue, &[], &targets, &shown, &mut generation);
        assert!(!upscaler.shows(Screen::Drc));
        assert_eq!(shown.get(Screen::Drc).unwrap().width, 8);
    }
}
