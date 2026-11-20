//! Frame generation: shows a title made for 30 fps at 60, without changing its speed.
//!
//! ```text
//!   flip n-1 ───────────────── flip n ─────────────── flip n+1
//!       │                        │
//!       │    two finished frames │
//!       └───────┬────────────────┘
//!               ▼
//!     flow (where things moved) ─► smooth ─► composite: the picture halfway
//!
//!   shown at flip n:              the generated picture
//!   shown half an interval later: the real frame n
//! ```
//!
//! The real frame waits half a flip interval, which is the price of this: a picture in
//! between needs the frame after it. Titles already flipping fast have no use for it, and
//! neither do xBRZ's screens, which the UI takes from elsewhere.
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use egui_wgpu::wgpu;
use wiiu_gpu::Screen;

use crate::{ScreenImage, ScreenImages};

const SHADER: &str = include_str!("framegen.wgsl");
/// Flips closer together than this need no help (about 42 fps and up).
const MIN_INTERVAL: Duration = Duration::from_millis(24);
/// Flips further apart are a pause or a loading screen, not motion to smooth.
const MAX_INTERVAL: Duration = Duration::from_millis(70);
/// Blocks of the motion search, relative to the screen.
const FLOW_DIVISOR: u32 = 8;

/// Whether frames are generated, changeable from any thread while the renderer runs.
#[derive(Debug, Clone, Default)]
pub struct FrameGenSetting {
    enabled: Arc<AtomicBool>,
    generated: Arc<AtomicU64>,
}

impl FrameGenSetting {
    #[inline]
    pub fn set(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Pictures generated so far.
    #[inline]
    pub fn images_generated(&self) -> u64 {
        self.generated.load(Ordering::Relaxed)
    }
}

/// What one screen keeps between flips.
struct ScreenState {
    prev: wgpu::Texture,
    cur: wgpu::Texture,
    flow: wgpu::Texture,
    smooth: wgpu::Texture,
    output: ScreenImage,
    /// Time of the last flip; `None` right after creation.
    last_flip: Option<Instant>,
}

struct Pipelines {
    layout: wgpu::BindGroupLayout,
    flow: wgpu::RenderPipeline,
    smooth: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
}

impl Pipelines {
    fn new(device: &wgpu::Device) -> Self {
        let visibility = wgpu::ShaderStages::FRAGMENT;
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shocora frame generation"),
            entries: &[
                texture(0),
                texture(1),
                texture(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shocora frame generation"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shocora frame generation"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = |entry: &str, format: wgpu::TextureFormat| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("shocora frame generation"),
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
                    targets: &[Some(format.into())],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let flow_format = wgpu::TextureFormat::Rg16Float;
        Self {
            flow: pipeline("fs_flow", flow_format),
            smooth: pipeline("fs_smooth", flow_format),
            composite: pipeline("fs_composite", wgpu::TextureFormat::Rgba8Unorm),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            layout,
        }
    }
}

fn create_state(device: &wgpu::Device, width: u32, height: u32, generation: u64) -> ScreenState {
    let texture = |label, width, height, format, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let frame = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
    let flow = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT;
    let (flow_width, flow_height) = ((width / FLOW_DIVISOR).max(1), (height / FLOW_DIVISOR).max(1));
    let flow_format = wgpu::TextureFormat::Rg16Float;
    let output = texture(
        "shocora generated frame",
        width,
        height,
        wgpu::TextureFormat::Rgba8Unorm,
        flow | wgpu::TextureUsages::COPY_SRC,
    );
    ScreenState {
        prev: texture(
            "shocora previous frame",
            width,
            height,
            wgpu::TextureFormat::Rgba8Unorm,
            frame,
        ),
        cur: texture(
            "shocora current frame",
            width,
            height,
            wgpu::TextureFormat::Rgba8Unorm,
            frame,
        ),
        flow: texture("shocora motion", flow_width, flow_height, flow_format, flow),
        smooth: texture("shocora smooth motion", flow_width, flow_height, flow_format, flow),
        output: ScreenImage {
            view: output.create_view(&wgpu::TextureViewDescriptor::default()),
            texture: output,
            width,
            height,
            generation,
        },
        last_flip: None,
    }
}

/// Frame generation for the screens of one renderer.
#[derive(Default)]
pub struct FrameGenerator {
    setting: FrameGenSetting,
    /// Made on first use: most sessions never generate frames.
    pipelines: Option<Pipelines>,
    states: HashMap<Screen, ScreenState>,
}

impl FrameGenerator {
    #[inline]
    pub fn setting(&self) -> FrameGenSetting {
        self.setting.clone()
    }

    /// Whether [`Self::update`] has work, and needs the flip's commands submitted first.
    pub(crate) fn active(&self) -> bool {
        self.setting.enabled()
    }

    /// The last picture generated for `screen`.
    #[inline]
    pub fn image(&self, screen: Screen) -> Option<&ScreenImage> {
        self.states.get(&screen).map(|state| &state.output)
    }

    /// Forgets the frames of the previous boot.
    #[inline]
    pub fn reset(&mut self) {
        self.states.clear();
    }

    /// After a flip: generates the picture before each new image and queues the image itself.
    /// `upscaled` screens are left alone.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        presented: &[Screen],
        targets: &HashMap<Screen, ScreenImage>,
        screens: &ScreenImages,
        generation: &mut u64,
        upscaled: bool,
    ) {
        if !self.setting.enabled() || upscaled {
            self.states.clear();
            return;
        }
        let now = Instant::now();
        let pipelines = self.pipelines.get_or_insert_with(|| Pipelines::new(device));
        for screen in presented {
            let Some(target) = targets.get(screen) else { continue };
            let stale = self.states.get(screen).is_none_or(|state| {
                state.output.width != target.width || state.output.height != target.height
            });
            if stale {
                *generation += 1;
                self.states.insert(
                    *screen,
                    create_state(device, target.width, target.height, *generation),
                );
            }
            let Some(state) = self.states.get_mut(screen) else { continue };
            std::mem::swap(&mut state.prev, &mut state.cur);
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("shocora frame generation"),
            });
            encoder.copy_texture_to_texture(
                target.texture.as_image_copy(),
                state.cur.as_image_copy(),
                state.cur.size(),
            );
            let interval = state.last_flip.map(|last| now.duration_since(last));
            state.last_flip = Some(now);
            // Without two frames of steady motion the real image simply stays, as `present` set it.
            let Some(interval) = interval.filter(|i| (MIN_INTERVAL..=MAX_INTERVAL).contains(i))
            else {
                queue.submit([encoder.finish()]);
                continue;
            };
            record_passes(device, &mut encoder, pipelines, state);
            queue.submit([encoder.finish()]);
            *generation += 1;
            state.output.generation = *generation;
            screens.set(*screen, state.output.clone());
            screens.set_after(*screen, target.clone(), now + interval / 2);
            self.setting.generated.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Motion search, its smoothing, and the picture in between.
fn record_passes(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pipelines: &Pipelines,
    state: &ScreenState,
) {
    let view =
        |texture: &wgpu::Texture| texture.create_view(&wgpu::TextureViewDescriptor::default());
    let (prev, cur, flow, smooth) =
        (view(&state.prev), view(&state.cur), view(&state.flow), view(&state.smooth));
    let group = |flow_in: &wgpu::TextureView| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shocora frame generation"),
            layout: &pipelines.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&prev),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&cur),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(flow_in),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&pipelines.sampler),
                },
            ],
        })
    };
    // The flow pass does not read its input: the other motion texture stands in for it.
    let passes: [(&wgpu::RenderPipeline, wgpu::BindGroup, &wgpu::TextureView); 3] = [
        (&pipelines.flow, group(&smooth), &flow),
        (&pipelines.smooth, group(&flow), &smooth),
        (&pipelines.composite, group(&smooth), &state.output.view),
    ];
    for (pipeline, bind_group, target) in &passes {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shocora frame generation"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
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
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (u32, u32) = (256, 144);
    const BOX: u32 = 32;

    /// A white box on black, with its left edge at `x`.
    fn picture(x: u32) -> Vec<u8> {
        let mut rgba = vec![0_u8; (SIZE.0 * SIZE.1 * 4) as usize];
        for row in 56..56 + BOX {
            for column in x..x + BOX {
                let at = ((row * SIZE.0 + column) * 4) as usize;
                rgba[at..at + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        for alpha in rgba.iter_mut().skip(3).step_by(4) {
            *alpha = 255;
        }
        rgba
    }

    #[test]
    fn a_moving_box_appears_halfway() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("no GPU here: skipped");
            return;
        };
        let (device, queue) =
            pollster::block_on(adapter.request_device(&Default::default())).unwrap();
        let mut state = create_state(&device, SIZE.0, SIZE.1, 1);
        let write = |texture: &wgpu::Texture, rgba: &[u8]| {
            queue.write_texture(
                texture.as_image_copy(),
                rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(SIZE.0 * 4),
                    rows_per_image: None,
                },
                texture.size(),
            );
        };
        write(&state.prev, &picture(80));
        write(&state.cur, &picture(104));
        state.last_flip = Some(Instant::now());

        let pipelines = Pipelines::new(&device);
        let mut encoder = device.create_command_encoder(&Default::default());
        record_passes(&device, &mut encoder, &pipelines, &state);
        let row = (SIZE.0 * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(row * SIZE.1),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            state.output.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            state.output.texture.size(),
        );
        queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let data = buffer.slice(..).get_mapped_range().unwrap();
        let red = |x: u32, y: u32| data[(y * row + x * 4) as usize];

        // The middle row: the box at 92..124, solid white, black around it.
        assert!(red(100, 72) > 200, "inside the box halfway: {}", red(100, 72));
        assert!(red(115, 72) > 200, "inside the box halfway: {}", red(115, 72));
        assert!(red(60, 72) < 40, "left of everything: {}", red(60, 72));
        assert!(red(160, 72) < 40, "right of everything: {}", red(160, 72));
    }
}
