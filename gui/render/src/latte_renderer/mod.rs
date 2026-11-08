//! Executes Latte's [`RenderCommand`]s on wgpu.
//!
//! Guest surfaces live in host textures identified by [`SurfaceKey`]; vertex,
//! index and uniform bytes all go through one storage buffer (the *arena*)
//! that generated shaders read directly. Commands are recorded into one
//! encoder per flip; anything `queue.write_*` would reorder flushes it first.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]

use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    time::{Duration, Instant},
};

use egui_wgpu::wgpu;
use wgpu::util::DeviceExt as _;
use wiiu_gpu::{
    RenderBackend, RenderCommand, Screen,
    latte::{ARENA_SIZE, STREAM_REGION_BASE, STREAM_REGION_SIZE},
    render::{
        AddressMode, BindingKind, BorderColor, BoundTexture, DrawCommand, HostFormat,
        ResolutionScale, SamplerDesc, ShaderModule, SurfaceKey, TextureUpload, Topology,
    },
    shader::{TextureDim, TextureKind},
};

use crate::{
    framegen::{FrameGenSetting, FrameGenerator},
    stream::{StreamTap, Streamer},
    upscale::{UpscaleSetting, Upscaler},
};

mod commands;
mod convert;
mod copy;
mod device;
mod draw;
mod pipeline;
mod pipelines;
mod present;
mod readback;
mod screens;
mod surface;
mod thread;
pub use convert::*;
pub use device::*;
use draw::*;
pub use pipeline::*;
use present::*;
pub use screens::*;
use surface::*;
pub use thread::*;
/// Size the shader prelude reads through the dynamic binding.
const PARAMS_BINDING_SIZE: u64 = 340;

/// Address of the surfaces standing in for attachments of the wrong size.
const STAND_IN_ADDRESS: u32 = 0xFFFF_F000;

/// `(color attachments, depth attachment)` of the open render pass.
type PassKey = ([Option<(SurfaceKey, u32)>; 8], Option<(SurfaceKey, u32)>);

/// The wgpu renderer.
pub struct LatteRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    arena: wgpu::Buffer,
    arena_layout: wgpu::BindGroupLayout,
    arena_group: wgpu::BindGroup,
    surfaces: HashMap<SurfaceKey, HostSurface>,
    next_surface_id: u64,
    /// Filled from several threads at once when pipelines are compiled.
    modules: Arc<Mutex<HashMap<u64, wgpu::ShaderModule>>>,
    /// The driver's compiled shaders, kept between runs when the backend can.
    pipeline_cache: Option<(wgpu::PipelineCache, PathBuf)>,
    pipelines: HashMap<(u64, u64, u64), KnownPipeline>,
    /// Pipelines on their way, made by other threads.
    compiling: HashSet<PipelineKey>,
    pipeline_jobs: Sender<PipelineJob>,
    finished_pipelines: Receiver<FinishedPipeline>,
    /// Bumped on every reboot: pipelines of an earlier boot are dropped on arrival.
    pipeline_epoch: u64,
    pipeline_setting: PipelineSetting,
    resource_layouts: HashMap<Vec<(u32, BindingKind)>, wgpu::BindGroupLayout>,
    pipeline_layouts: HashMap<(Vec<(u32, BindingKind)>, bool), wgpu::PipelineLayout>,
    /// What vertex shaders stream out to, with its `group(2)`, if the adapter allows it.
    stream: Option<StreamOutput>,
    samplers: HashMap<SamplerDesc, wgpu::Sampler>,
    bind_groups: HashMap<u64, wgpu::BindGroup>,
    dummy_textures: HashMap<(TextureDim, TextureKind), wgpu::TextureView>,
    feedback_copies: HashMap<SurfaceKey, wgpu::Texture>,
    encoder: Option<wgpu::CommandEncoder>,
    pass: Option<wgpu::RenderPass<'static>>,
    pass_key: Option<PassKey>,
    blit_pipeline: wgpu::RenderPipeline,
    blit_layout: wgpu::BindGroupLayout,
    blit_sampler: wgpu::Sampler,
    /// Layout and per-target pipelines of depth to color copies, made on first use.
    depth_to_color: Option<(wgpu::BindGroupLayout, wgpu::PipelineLayout, wgpu::ShaderModule)>,
    depth_to_color_pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    pending_scan: HashMap<Screen, (SurfaceKey, u32)>,
    screen_targets: HashMap<Screen, ScreenImage>,
    screens: ScreenImages,
    generation: u64,
    upscaler: Upscaler,
    frame_generator: FrameGenerator,
    /// Pictures for a second player: the TV's, then the GamePad's.
    streamers: [Streamer; 2],
    /// How much larger than on the console the title's pictures are drawn.
    resolution: ResolutionScale,
    /// Tells whoever chooses the resolution when video memory ran out.
    memory_watch: MemoryWatch,
    /// Textures the title brought along: these never grow, whatever their size.
    uploaded: std::collections::HashSet<SurfaceKey>,
    /// Surfaces the GPU drew to, whose contents only the host has.
    drawn_to: HashSet<SurfaceKey>,
    /// How often the arena started over, for tools.
    arena_wraps: u64,
    /// Screens with a new image since the last flip.
    presented: Vec<Screen>,
    logged: std::collections::HashSet<String>,
    /// Debugging aid: only the first draws of each frame execute (SHOCORA_MAX_DRAWS).
    max_draws: Option<u32>,
    frame_draws: u32,
    target_log: HashMap<SurfaceKey, u32>,
    pub draws_executed: u64,
    pub flips: u64,
}

impl std::fmt::Debug for LatteRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LatteRenderer")
            .field("draws", &self.draws_executed)
            .field("flips", &self.flips)
            .finish_non_exhaustive()
    }
}

impl LatteRenderer {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        device.set_device_lost_callback(|reason, message| {
            log::error!("wgpu device lost ({reason:?}): {message}");
        });
        device.on_uncaptured_error(Arc::new(|error| log::error!("wgpu: {error}")));

        let limit = device.limits().max_storage_buffer_binding_size;
        let arena_size = u64::from(ARENA_SIZE).min(limit);
        let arena = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shocora arena"),
            size: arena_size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDEX
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let visibility = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;
        let arena_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shocora arena layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(PARAMS_BINDING_SIZE),
                    },
                    count: None,
                },
            ],
        });

        let arena_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shocora arena group"),
            layout: &arena_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: arena.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &arena,
                        offset: 0,
                        size: wgpu::BufferSize::new(PARAMS_BINDING_SIZE),
                    }),
                },
            ],
        });

        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shocora blit layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let blit_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shocora blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
        });

        let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shocora blit"),
            bind_group_layouts: &[Some(&blit_layout)],
            immediate_size: 0,
        });

        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shocora blit"),
            layout: Some(&blit_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &blit_module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &blit_module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())],
            }),
            multiview_mask: None,
            cache: None,
        });

        let blit_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let stream = StreamOutput::new(&device);

        let (pipeline_jobs, finished_pipelines) = spawn_pipeline_threads();
        Self {
            device,
            queue,
            arena,
            arena_layout,
            arena_group,
            surfaces: HashMap::new(),
            next_surface_id: 1,
            modules: Arc::default(),
            pipeline_cache: None,
            pipelines: HashMap::new(),
            compiling: HashSet::new(),
            pipeline_jobs,
            finished_pipelines,
            pipeline_epoch: 0,
            pipeline_setting: PipelineSetting::default(),
            resource_layouts: HashMap::new(),
            pipeline_layouts: HashMap::new(),
            stream,
            samplers: HashMap::new(),
            bind_groups: HashMap::new(),
            dummy_textures: HashMap::new(),
            feedback_copies: HashMap::new(),
            encoder: None,
            pass: None,
            pass_key: None,
            blit_pipeline,
            blit_layout,
            blit_sampler,
            depth_to_color: None,
            depth_to_color_pipelines: HashMap::new(),
            pending_scan: HashMap::new(),
            screen_targets: HashMap::new(),
            screens: ScreenImages::default(),
            generation: 0,
            upscaler: Upscaler::default(),
            frame_generator: FrameGenerator::default(),
            streamers: Screen::ALL.map(Streamer::new),
            resolution: ResolutionScale::default(),
            memory_watch: MemoryWatch::default(),
            uploaded: std::collections::HashSet::new(),
            drawn_to: HashSet::new(),
            arena_wraps: 0,
            presented: Vec::new(),
            logged: std::collections::HashSet::new(),
            max_draws: std::env::var("SHOCORA_MAX_DRAWS").ok().and_then(|v| v.parse().ok()),
            frame_draws: 0,
            target_log: HashMap::new(),
            draws_executed: 0,
            flips: 0,
        }
    }

    #[inline]
    pub fn screens(&self) -> ScreenImages {
        self.screens.clone()
    }

    /// Reports when video memory runs out, while the renderer runs.
    #[inline]
    pub fn memory_watch(&self) -> MemoryWatch {
        self.memory_watch.clone()
    }

    /// Where a second player's pictures come from, while the renderer runs.
    #[inline]
    pub fn stream_tap(&self, screen: Screen) -> StreamTap {
        let index = Screen::ALL.iter().position(|each| *each == screen).unwrap_or(0);
        self.streamers[index].tap()
    }

    /// Handle to switch frame generation while the renderer runs.
    #[inline]
    pub fn frame_gen_setting(&self) -> FrameGenSetting {
        self.frame_generator.setting()
    }

    /// The last generated picture of a screen as RGBA8 (tools and tests).
    #[inline]
    pub fn read_generated(&mut self, screen: Screen) -> Option<(u32, u32, Vec<u8>)> {
        self.submit();
        let image = self.frame_generator.image(screen)?.clone();
        self.read_texture(&image.texture, image.width, image.height)
    }

    /// Handle to change the screens' upscaling while the renderer runs.
    #[inline]
    pub fn upscale_setting(&self) -> UpscaleSetting {
        self.upscaler.setting()
    }

    #[inline]
    pub const fn device(&self) -> &wgpu::Device {
        &self.device
    }

    fn log_once(&mut self, message: String) {
        if self.logged.insert(message.clone()) {
            log::warn!("{message}");
        }
    }

    fn encoder(&mut self) -> &mut wgpu::CommandEncoder {
        self.encoder.get_or_insert_with(|| {
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("shocora frame"),
            })
        })
    }

    fn end_pass(&mut self) {
        self.pass = None;
        self.pass_key = None;
    }

    /// Submits everything recorded so far.
    pub fn submit(&mut self) {
        self.end_pass();
        if let Some(encoder) = self.encoder.take() {
            self.queue.submit([encoder.finish()]);
        }
    }
}

impl Drop for LatteRenderer {
    fn drop(&mut self) {
        if std::env::var_os("SHOCORA_PROFILE").is_some() {
            println!(
                "renderer: {} surfaces ({} uploaded), {} pipelines, {} parsed shaders, {} bind groups, {} arena wraps",
                self.surfaces.len(),
                self.uploaded.len(),
                self.pipelines.len(),
                self.modules.lock().unwrap_or_else(PoisonError::into_inner).len(),
                self.bind_groups.len(),
                self.arena_wraps,
            );
        }
        self.save_pipeline_cache();
    }
}
