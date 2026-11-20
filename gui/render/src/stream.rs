//! Small pictures of a screen for a second player on the network.
//!
//! The GPU shrinks a thinned-out selection of frames and hands them over without stalling the
//! renderer: a picture is copied to a buffer at one flip and picked up at a later one, once
//! the GPU got there.
use std::{
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use egui_wgpu::wgpu;

use crate::{Screen, ScreenImage};

/// Size of the TV's pictures, half as wide as the usual 1280 x 720 TV.
const TV_SIZE: (u32, u32) = (640, 360);
/// Time between the TV's pictures: 20 per second, whatever the title's frame rate is.
const TV_INTERVAL: Duration = Duration::from_millis(50);
/// Size of the GamePad's pictures, half as wide as its screen.
const GAMEPAD_SIZE: (u32, u32) = (427, 240);
/// Time between the GamePad's pictures: titles mostly show little there, so 10 per second.
const GAMEPAD_INTERVAL: Duration = Duration::from_millis(100);

/// A picture as RGBA8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Switches the pictures on and takes them, from any thread.
#[derive(Debug, Clone, Default)]
pub struct StreamTap {
    enabled: Arc<AtomicBool>,
    latest: Arc<Mutex<Option<StreamFrame>>>,
}

impl StreamTap {
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
        if !enabled {
            *self.latest.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
    }

    /// The newest picture since the last call.
    pub fn take(&self) -> Option<StreamFrame> {
        self.latest.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

/// The GPU's side of a picture.
struct Target {
    view: wgpu::TextureView,
    texture: wgpu::Texture,
    buffer: wgpu::Buffer,
}

/// Lives on the render thread.
pub struct Streamer {
    size: (u32, u32),
    interval: Duration,
    tap: StreamTap,
    target: Option<Target>,
    last: Option<Instant>,
    /// A copy is on its way into the buffer.
    in_flight: bool,
    /// Set once the buffer can be read.
    mapped: Arc<AtomicBool>,
}

impl std::fmt::Debug for Streamer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Streamer").field("in_flight", &self.in_flight).finish_non_exhaustive()
    }
}

impl Streamer {
    /// Pictures of `screen`.
    pub fn new(screen: Screen) -> Self {
        let (size, interval) = match screen {
            Screen::Tv => (TV_SIZE, TV_INTERVAL),
            Screen::Drc => (GAMEPAD_SIZE, GAMEPAD_INTERVAL),
        };
        Self {
            size,
            interval,
            tap: StreamTap::default(),
            target: None,
            last: None,
            in_flight: false,
            mapped: Arc::default(),
        }
    }

    #[inline]
    pub fn tap(&self) -> StreamTap {
        self.tap.clone()
    }

    /// After a flip: hands over a finished picture and starts the next one when it is due.
    /// `source` is the screen's new image, if there is one. `pipeline` must render
    /// to RGBA8 from the texture and sampler of `layout`.
    /// Whether [`Self::update`] has work, and needs the flip's commands submitted first.
    pub(crate) fn active(&self) -> bool {
        self.in_flight || self.tap.enabled.load(Ordering::Relaxed)
    }

    pub(crate) fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: Option<&ScreenImage>,
        pipeline: &wgpu::RenderPipeline,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
    ) {
        if !self.tap.enabled.load(Ordering::Relaxed) && !self.in_flight {
            return;
        }
        if self.in_flight {
            let _ = device.poll(wgpu::PollType::Poll);
            if !self.mapped.swap(false, Ordering::Relaxed) {
                return;
            }
            self.deliver();
            self.in_flight = false;
        }
        let due = self.last.is_none_or(|last| last.elapsed() >= self.interval);
        if !due || !self.tap.enabled.load(Ordering::Relaxed) {
            return;
        }
        let Some(source) = source else { return };
        self.last = Some(Instant::now());
        let size = self.size;
        let target = self.target.get_or_insert_with(|| create_target(device, size));
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shocora stream"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("shocora stream"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shocora stream"),
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
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.copy_texture_to_buffer(
            target.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &target.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row_bytes(size.0)),
                    rows_per_image: None,
                },
            },
            target.texture.size(),
        );
        queue.submit([encoder.finish()]);
        let mapped = Arc::clone(&self.mapped);
        target.buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            // A failed mapping leaves the slot empty: `deliver` then finds nothing to read.
            if result.is_err() {
                log::warn!("netplay: a picture could not be read back");
            }
            mapped.store(true, Ordering::Relaxed);
        });
        self.in_flight = true;
    }

    fn deliver(&self) {
        let Some(target) = &self.target else { return };
        let picture = target.buffer.slice(..).get_mapped_range().ok().map(|data| {
            let row = row_bytes(self.size.0) as usize;
            let mut rgba = Vec::with_capacity((self.size.0 * self.size.1 * 4) as usize);
            for line in data.chunks_exact(row) {
                rgba.extend_from_slice(&line[..(self.size.0 * 4) as usize]);
            }
            rgba
        });
        target.buffer.unmap();
        let Some(rgba) = picture else { return };
        if self.tap.enabled.load(Ordering::Relaxed) {
            let frame = StreamFrame { width: self.size.0, height: self.size.1, rgba };
            *self.tap.latest.lock().unwrap_or_else(PoisonError::into_inner) = Some(frame);
        }
    }
}

const fn row_bytes(width: u32) -> u32 {
    (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
}

fn create_target(device: &wgpu::Device, size: (u32, u32)) -> Target {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shocora stream"),
        size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("shocora stream"),
        size: u64::from(row_bytes(size.0)) * u64::from(size.1),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    Target { view, texture, buffer }
}
