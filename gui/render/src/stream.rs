//! Pictures of a screen for a second player on the network.
//!
//! The GPU scales a thinned-out selection of frames to the size the host chose and hands them
//! over without stalling the renderer: a picture is copied to a buffer at one flip and picked
//! up at a later one, once the GPU got there. For video encoders the GPU also converts them to
//! NV12, which halves what crosses the bus and spares the CPU the conversion.
use std::{
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use egui_wgpu::wgpu;

use crate::{Screen, ScreenImage};

/// What pictures a screen streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// NV12 for video encoders; RGBA otherwise.
    pub nv12: bool,
}

impl StreamFormat {
    /// Half as wide as the usual 1280 x 720 TV, 20 per second, for JPEG.
    pub const TV_DEFAULT: Self = Self { width: 640, height: 360, fps: 20, nv12: false };
    /// Half as wide as the GamePad: titles mostly show little there, so 10 per second.
    pub const GAMEPAD_DEFAULT: Self = Self { width: 427, height: 240, fps: 10, nv12: false };

    /// The TV at `height` lines (16:9), for a video encoder.
    pub const fn tv(height: u32, fps: u32) -> Self {
        let width = (height * 16 / 9) & !1;
        Self { width, height: height & !1, fps, nv12: true }
    }

    fn interval(self) -> Duration {
        Duration::from_secs(1) / self.fps.max(1)
    }

    /// NV12 needs even sides.
    fn size(self) -> (u32, u32) {
        if self.nv12 {
            ((self.width & !1).max(2), (self.height & !1).max(2))
        } else {
            (self.width.max(1), self.height.max(1))
        }
    }
}

/// A picture as RGBA8, or as NV12 for video encoders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamFrame {
    pub width: u32,
    pub height: u32,
    pub nv12: bool,
    pub data: Vec<u8>,
}

/// Switches the pictures on, chooses them and takes them, from any thread.
#[derive(Debug, Clone)]
pub struct StreamTap {
    enabled: Arc<AtomicBool>,
    latest: Arc<Mutex<Option<StreamFrame>>>,
    format: Arc<Mutex<StreamFormat>>,
}

impl StreamTap {
    fn new(format: StreamFormat) -> Self {
        Self {
            enabled: Arc::default(),
            latest: Arc::default(),
            format: Arc::new(Mutex::new(format)),
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
        if !enabled {
            *self.latest.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
    }

    /// The pictures from the next one on.
    pub fn set_format(&self, format: StreamFormat) {
        *self.format.lock().unwrap_or_else(PoisonError::into_inner) = format;
    }

    pub fn format(&self) -> StreamFormat {
        *self.format.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The newest picture since the last call.
    pub fn take(&self) -> Option<StreamFrame> {
        self.latest.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

/// Planes of one picture on the GPU: RGBA, or NV12's brightness and colour.
struct Plane {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Bytes of a row in the buffer, padded as copies want.
    row: u32,
    /// Where the plane starts in the buffer.
    offset: u64,
}

/// The GPU's side of a picture.
struct Target {
    format: StreamFormat,
    planes: Vec<Plane>,
    buffer: wgpu::Buffer,
}

/// Conversion to NV12 from the blit's texture and sampler.
const NV12_SHADER: &str = r"
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;

struct Varyings {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> Varyings {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: Varyings;
    out.position = vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(corner.x, 1.0 - corner.y);
    return out;
}

// BT.709 in limited range, which decoders assume for HD pictures.
fn luma(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@fragment
fn fs_luma(in: Varyings) -> @location(0) vec4<f32> {
    let color = textureSample(source, source_sampler, in.uv).rgb;
    return vec4<f32>(luma(color) * (219.0 / 255.0) + 16.0 / 255.0, 0.0, 0.0, 1.0);
}

@fragment
fn fs_chroma(in: Varyings) -> @location(0) vec4<f32> {
    // Half size: the sample between four pixels is their average.
    let color = textureSample(source, source_sampler, in.uv).rgb;
    let y = luma(color);
    let u = (color.b - y) / 1.8556;
    let v = (color.r - y) / 1.5748;
    return vec4<f32>(u * (224.0 / 255.0) + 0.5, v * (224.0 / 255.0) + 0.5, 0.0, 1.0);
}
";

/// Lives on the render thread.
pub struct Streamer {
    tap: StreamTap,
    target: Option<Target>,
    last: Option<Instant>,
    /// A copy is on its way into the buffer.
    in_flight: bool,
    /// Set once the buffer can be read.
    mapped: Arc<AtomicBool>,
    /// Brightness and colour pipelines, made on first use.
    nv12: Option<[wgpu::RenderPipeline; 2]>,
}

impl std::fmt::Debug for Streamer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Streamer").field("in_flight", &self.in_flight).finish_non_exhaustive()
    }
}

impl Streamer {
    /// Pictures of `screen`.
    pub fn new(screen: Screen) -> Self {
        let format = match screen {
            Screen::Tv => StreamFormat::TV_DEFAULT,
            Screen::Drc => StreamFormat::GAMEPAD_DEFAULT,
        };
        Self {
            tap: StreamTap::new(format),
            target: None,
            last: None,
            in_flight: false,
            mapped: Arc::default(),
            nv12: None,
        }
    }

    #[inline]
    pub fn tap(&self) -> StreamTap {
        self.tap.clone()
    }

    /// Whether [`Self::update`] has work, and needs the flip's commands submitted first.
    pub(crate) fn active(&self) -> bool {
        self.in_flight || self.tap.enabled.load(Ordering::Relaxed)
    }

    /// After a flip: hands over a finished picture and starts the next one when it is due.
    /// `source` is the screen's new image, if there is one. `pipeline` must render
    /// to RGBA8 from the texture and sampler of `layout`.
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
        let format = self.tap.format();
        // A flip a little early still counts: at 60 per second each flip comes about one
        // interval after the last, and holding to the interval exactly dropped every other one.
        let interval = format.interval();
        let due = self.last.is_none_or(|last| last.elapsed() + interval / 4 >= interval);
        if !due || !self.tap.enabled.load(Ordering::Relaxed) {
            return;
        }
        let Some(source) = source else { return };
        self.last = Some(Instant::now());
        if self.target.as_ref().is_some_and(|target| target.format != format) {
            self.target = None;
        }
        let target = self.target.get_or_insert_with(|| create_target(device, format));
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
        let pipelines: Vec<&wgpu::RenderPipeline> = if format.nv12 {
            let [luma, chroma] = self.nv12.get_or_insert_with(|| nv12_pipelines(device, layout));
            vec![luma, chroma]
        } else {
            vec![pipeline]
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("shocora stream"),
        });
        for (plane, pipeline) in target.planes.iter().zip(pipelines) {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shocora stream"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &plane.view,
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
        for plane in &target.planes {
            encoder.copy_texture_to_buffer(
                plane.texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &target.buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: plane.offset,
                        bytes_per_row: Some(plane.row),
                        rows_per_image: None,
                    },
                },
                plane.texture.size(),
            );
        }
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

    /// Packs the planes' rows together and hands the picture over.
    fn deliver(&self) {
        let Some(target) = &self.target else { return };
        let picture = target.buffer.slice(..).get_mapped_range().ok().map(|data| {
            let mut packed = Vec::new();
            for plane in &target.planes {
                let size = plane.texture.size();
                let bytes = plane.texture.format().block_copy_size(None).unwrap_or(4);
                let used = (size.width * bytes) as usize;
                let start = plane.offset as usize;
                let end = start + plane.row as usize * size.height as usize;
                for line in data[start..end].chunks_exact(plane.row as usize) {
                    packed.extend_from_slice(&line[..used]);
                }
            }
            packed
        });
        target.buffer.unmap();
        let Some(data) = picture else { return };
        if self.tap.enabled.load(Ordering::Relaxed) {
            let (width, height) = target.format.size();
            let frame = StreamFrame { width, height, nv12: target.format.nv12, data };
            *self.tap.latest.lock().unwrap_or_else(PoisonError::into_inner) = Some(frame);
        }
    }
}

const fn row_bytes(bytes: u32) -> u32 {
    bytes.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
}

fn create_target(device: &wgpu::Device, format: StreamFormat) -> Target {
    let (width, height) = format.size();
    let planes: Vec<(wgpu::TextureFormat, u32, u32)> = if format.nv12 {
        vec![
            (wgpu::TextureFormat::R8Unorm, width, height),
            (wgpu::TextureFormat::Rg8Unorm, width / 2, height / 2),
        ]
    } else {
        vec![(wgpu::TextureFormat::Rgba8Unorm, width, height)]
    };
    let mut offset = 0;
    let planes: Vec<Plane> = planes
        .into_iter()
        .map(|(texel, width, height)| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("shocora stream"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: texel,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let row = row_bytes(width * texel.block_copy_size(None).unwrap_or(4));
            let plane = Plane { texture, view, row, offset };
            offset += u64::from(row) * u64::from(height);
            plane
        })
        .collect();
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("shocora stream"),
        size: offset,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    Target { format, planes, buffer }
}

fn nv12_pipelines(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
) -> [wgpu::RenderPipeline; 2] {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("shocora stream nv12"),
        source: wgpu::ShaderSource::Wgsl(NV12_SHADER.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("shocora stream nv12"),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    [("fs_luma", wgpu::TextureFormat::R8Unorm), ("fs_chroma", wgpu::TextureFormat::Rg8Unorm)].map(
        |(entry, texel)| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("shocora stream nv12"),
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
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(texel.into())],
                }),
                multiview_mask: None,
                cache: None,
            })
        },
    )
}
