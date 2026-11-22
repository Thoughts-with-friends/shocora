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
    hash::{BuildHasher, Hash, Hasher},
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
        AddressMode, BindingKind, BorderColor, BoundTexture, DrawCommand, HostFormat, ReadbackData,
        ReadbackReply, ReadbackTarget, ResolutionScale, SamplerDesc, ShaderModule, SurfaceKey,
        TextureUpload, Topology,
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
mod finish;
mod pipeline;
mod pipelines;
mod present;
mod readback;
mod reinterpret;
mod screens;
mod surface;
mod thread;
pub use convert::*;
pub use device::*;
use draw::*;
use finish::*;
pub use pipeline::*;
use present::*;
pub use screens::*;
use surface::*;
pub use thread::*;
/// The renderer's maps: their keys are hashed for every draw, which SipHash made a tenth of
/// the render thread's time.
type HashMap<K, V> = std::collections::HashMap<K, V, foldhash::fast::RandomState>;
type HashSet<T> = std::collections::HashSet<T, foldhash::fast::RandomState>;

/// A hash for keys that only live as long as the process.
fn fast_hash(value: &impl Hash) -> u64 {
    fast_hasher().hash_one(value)
}

fn fast_hasher() -> foldhash::fast::FixedState {
    foldhash::fast::FixedState::with_seed(0x0053_484f_434f_5241)
}

/// Size the shader prelude reads through the dynamic binding.
const PARAMS_BINDING_SIZE: u64 = wiiu_gpu::latte::PARAMS_SIZE as u64;

/// Draws in a part of a frame finished on its own (see [`finish`]): Cemu submits every few
/// hundred draws too.
const PART_DRAWS: u32 = 400;

/// Staging space for texture uploads recorded into the frame's commands.
const UPLOAD_RING_SIZE: u64 = 32 << 20;

/// Address of the surfaces standing in for attachments of the wrong size.
const STAND_IN_ADDRESS: u32 = 0xFFFF_F000;
/// How much larger than a pass an attachment may be and still be the same picture with its
/// tile padding (Latte pads surfaces to whole tiles; Xenoblade Chronicles X draws a
/// 768x620 picture over a 768x624 depth buffer).
const ALIAS_PADDING: u32 = 32;

/// `(color attachments, depth attachment, depth attachment read-only)` of the open render
/// pass.
type PassKey = ([Option<(SurfaceKey, u32)>; 8], Option<(SurfaceKey, u32)>, bool);

/// The wgpu renderer.
pub struct LatteRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    arena: wgpu::Buffer,
    arena_layout: wgpu::BindGroupLayout,
    arena_group: wgpu::BindGroup,
    /// Texels on their way to textures: written with the queue, copied by the encoder, so an
    /// upload lands between the draws around it without submitting what came before.
    upload_ring: wgpu::Buffer,
    /// Where the next upload goes in `upload_ring`; back to 0 on every submit.
    upload_cursor: u64,
    /// Arena writes that follow each other, gathered into one queue write: a frame has
    /// thousands of small ones, and each queue write has a cost of its own.
    arena_pending: (u64, Vec<u8>),
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
    /// What each feedback copy holds, so that only what draws changed since is copied.
    feedback_sync: HashMap<SurfaceKey, draw::FeedbackSync>,
    /// Attachments larger than the pass (a depth buffer 768x624 under a 768x620 picture,
    /// as Xenoblade Chronicles X has) are drawn into a surface of the pass's size, which
    /// clears and copies of the large one reach too.
    aliases: HashMap<SurfaceKey, SurfaceKey>,
    /// Readbacks recorded in the open encoder: mapped once it is submitted.
    readbacks_recorded: Vec<readback::PendingReadback>,
    /// Readbacks submitted, waiting for the GPU.
    readbacks_in_flight: Vec<readback::PendingReadback>,
    encoder: Option<wgpu::CommandEncoder>,
    /// Draws recorded into `encoder`: past [`PART_DRAWS`], the next pass starts a new part.
    part_draws: u32,
    /// Finishes and submits the parts of frames, see [`finish`].
    finisher: Finisher,
    /// Readbacks in parts handed over, by part: they are mapped once it is submitted.
    readbacks_submitting: Vec<(u64, readback::PendingReadback)>,
    /// Screen images drawn by the commands being recorded, and by parts handed over: they
    /// are shown once their commands are submitted.
    shows_recorded: Vec<(Screen, ScreenImage)>,
    shows_submitting: Vec<(u64, Screen, ScreenImage)>,
    pass: Option<wgpu::RenderPass<'static>>,
    pass_key: Option<PassKey>,
    /// What the open pass has set, so equal state is not set again.
    bound: BoundState,
    blit_pipeline: wgpu::RenderPipeline,
    blit_layout: wgpu::BindGroupLayout,
    blit_sampler: wgpu::Sampler,
    /// Scan buffer presentation with the user's colour adjustments.
    present_grade: present::PresentGrade,
    /// Layout and per-target pipelines of depth to color copies, made on first use.
    depth_to_color: Option<(wgpu::BindGroupLayout, wgpu::PipelineLayout, wgpu::ShaderModule)>,
    depth_to_color_pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    /// Surfaces drawn at one place in two formats of the same texel size.
    reinterpret: reinterpret::Reinterpret,
    /// Scan buffer copies of this frame: surface, slice, sRGB scan buffer.
    pending_scan: HashMap<Screen, (SurfaceKey, u32, bool)>,
    screen_targets: std::collections::HashMap<Screen, ScreenImage>,
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
    uploaded: HashSet<SurfaceKey>,
    /// Surfaces the GPU drew to, whose contents only the host has.
    drawn_to: HashSet<SurfaceKey>,
    /// How often the arena started over, for tools.
    arena_wraps: u64,
    /// Screens with a new image since the last flip.
    presented: Vec<Screen>,
    logged: HashSet<String>,
    /// Debugging aid: only the first draws of each frame execute (SHOCORA_MAX_DRAWS).
    max_draws: Option<u32>,
    frame_draws: u32,
    target_log: HashMap<SurfaceKey, u32>,
    /// What frames cost the host, reported with `SHOCORA_PROFILE`.
    counters: Counters,
    profile: bool,
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

        let upload_ring = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shocora uploads"),
            size: UPLOAD_RING_SIZE,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
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

        let blit_with = |fragment: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
                    entry_point: Some(fragment),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let blit_pipeline = blit_with("fs_main");

        let blit_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let present_grade = present::PresentGrade::new(&device);
        let stream = StreamOutput::new(&device);

        let (pipeline_jobs, finished_pipelines) = spawn_pipeline_threads();
        Self {
            device,
            queue,
            arena,
            arena_layout,
            arena_group,
            upload_ring,
            upload_cursor: 0,
            arena_pending: (0, Vec::new()),
            surfaces: HashMap::default(),
            next_surface_id: 1,
            modules: Arc::default(),
            pipeline_cache: None,
            pipelines: HashMap::default(),
            compiling: HashSet::default(),
            pipeline_jobs,
            finished_pipelines,
            pipeline_epoch: 0,
            pipeline_setting: PipelineSetting::default(),
            resource_layouts: HashMap::default(),
            pipeline_layouts: HashMap::default(),
            stream,
            samplers: HashMap::default(),
            bind_groups: HashMap::default(),
            dummy_textures: HashMap::default(),
            feedback_copies: HashMap::default(),
            feedback_sync: HashMap::default(),
            aliases: HashMap::default(),
            readbacks_recorded: Vec::new(),
            readbacks_in_flight: Vec::new(),
            encoder: None,
            part_draws: 0,
            finisher: Finisher::new(),
            readbacks_submitting: Vec::new(),
            shows_recorded: Vec::new(),
            shows_submitting: Vec::new(),
            pass: None,
            pass_key: None,
            bound: BoundState::default(),
            blit_pipeline,
            blit_layout,
            blit_sampler,
            present_grade,
            depth_to_color: None,
            depth_to_color_pipelines: HashMap::default(),
            reinterpret: reinterpret::Reinterpret::default(),
            pending_scan: HashMap::default(),
            screen_targets: std::collections::HashMap::new(),
            screens: ScreenImages::default(),
            generation: 0,
            upscaler: Upscaler::default(),
            frame_generator: FrameGenerator::default(),
            streamers: Screen::ALL.map(Streamer::new),
            resolution: ResolutionScale::default(),
            memory_watch: MemoryWatch::default(),
            uploaded: HashSet::default(),
            drawn_to: HashSet::default(),
            arena_wraps: 0,
            presented: Vec::new(),
            logged: HashSet::default(),
            max_draws: std::env::var("SHOCORA_MAX_DRAWS").ok().and_then(|v| v.parse().ok()),
            frame_draws: 0,
            target_log: HashMap::default(),
            counters: Counters::default(),
            profile: std::env::var_os("SHOCORA_PROFILE").is_some(),
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

    /// Handle to change the colour adjustments of the screens while the renderer runs.
    #[inline]
    pub fn color_grade_setting(&self) -> crate::ColorGradeSetting {
        self.present_grade.setting.clone()
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

    /// Writes `bytes` to the arena at `offset`, after the writes gathered so far if it
    /// continues them. The queue applies its writes before the next submitted commands, so
    /// gathering them until then changes nothing for the draws.
    fn write_arena(&mut self, offset: u64, bytes: &[u8]) {
        /// Gaps of padding between entries are filled in rather than starting over.
        const MAX_GAP: u64 = 256;
        const MAX_PENDING: usize = 4 << 20;
        let (start, pending) = &mut self.arena_pending;
        let end = *start + pending.len() as u64;
        if pending.is_empty() {
            *start = offset;
        } else if offset < end || offset - end > MAX_GAP || pending.len() > MAX_PENDING {
            self.flush_arena();
            self.arena_pending.0 = offset;
        } else {
            pending.resize((offset - *start) as usize, 0);
        }
        self.arena_pending.1.extend_from_slice(bytes);
    }

    fn flush_arena(&mut self) {
        let (start, pending) = &mut self.arena_pending;
        if pending.is_empty() {
            return;
        }
        // Writes must cover whole words: vertex data and indices often end mid-word. The
        // padding lands in the gap `arena_reserve` leaves before the next entry.
        let padded = (pending.len() as u64).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
        pending.resize(padded as usize, 0);
        if *start + padded <= self.arena.size() {
            self.queue.write_buffer(&self.arena, *start, pending);
        }
        pending.clear();
    }

    fn end_pass(&mut self) {
        self.pass = None;
        self.pass_key = None;
        self.bound = BoundState::default();
    }

    /// Hands what is recorded so far to the finishing threads as a part of its own, which is
    /// submitted once they are done with it.
    fn cut(&mut self) {
        self.end_pass();
        self.flush_arena();
        if let Some(encoder) = self.encoder.take() {
            self.counters.submits += 1;
            self.part_draws = 0;
            self.finisher.hand_over(&self.queue, encoder);
            let part = self.finisher.handed_over();
            self.readbacks_submitting
                .extend(self.readbacks_recorded.drain(..).map(|readback| (part, readback)));
            self.shows_submitting
                .extend(self.shows_recorded.drain(..).map(|(screen, image)| (part, screen, image)));
        }
        self.after_submits();
    }

    /// Submits everything recorded so far.
    pub fn submit(&mut self) {
        self.cut();
        self.finisher.wait_all(&self.queue);
        self.after_submits();
        // The queue's writes of the next commands come after these in the queue's order.
        self.upload_cursor = 0;
    }

    /// Submits the parts finished meanwhile, without waiting for the others.
    pub(super) fn submit_finished(&mut self) {
        self.finisher.submit_ready(&self.queue);
        self.after_submits();
    }

    /// Submits every part handed over, as there is nothing else to do: what a frame drew
    /// must not wait for the next frame's commands to reach the screen.
    pub fn settle(&mut self) {
        self.finisher.wait_all(&self.queue);
        self.after_submits();
        if self.encoder.is_none() {
            // Every upload recorded so far is submitted: the ring can start over.
            self.upload_cursor = 0;
        }
    }

    /// What waits for its part to be submitted: readbacks can be mapped and screen images
    /// shown once their commands are in the queue.
    fn after_submits(&mut self) {
        let submitted = self.finisher.submitted();
        if self.readbacks_submitting.iter().any(|(part, _)| *part <= submitted) {
            let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.readbacks_submitting)
                .into_iter()
                .partition(|(part, _)| *part <= submitted);
            self.readbacks_submitting = waiting;
            self.map_readbacks(ready.into_iter().map(|(_, readback)| readback));
        }
        if self.shows_submitting.iter().any(|(part, ..)| *part <= submitted) {
            let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.shows_submitting)
                .into_iter()
                .partition(|(part, ..)| *part <= submitted);
            self.shows_submitting = waiting;
            for (_, screen, image) in ready {
                self.screens.set(screen, image);
            }
        }
    }
}

/// Host work since the last report, for finding where frames lose their time.
#[derive(Debug, Default)]
struct Counters {
    flips: u64,
    passes: u64,
    submits: u64,
    uploads: u64,
    copies: u64,
    bind_groups_made: u64,
    /// Textures sampled while attached: depth without and with depth or stencil writes, color
    /// in the attached slice and level, and color elsewhere in the surface.
    feedback: [u64; 4],
}

impl Counters {
    /// Flips between reports.
    const EVERY: u64 = 300;

    fn report(&mut self) {
        let flips = self.flips.max(1) as f64;
        println!(
            "renderer per flip: {:.0} passes, {:.1} submits, {:.1} uploads, {:.1} surface copies, {:.1} bind groups made, feedback {:?}",
            self.passes as f64 / flips,
            self.submits as f64 / flips,
            self.uploads as f64 / flips,
            self.copies as f64 / flips,
            self.bind_groups_made as f64 / flips,
            self.feedback.map(|count| (count as f64 / flips).round() as u64),
        );
        *self = Self::default();
    }
}

/// State set on the open render pass.
#[derive(Default)]
struct BoundState {
    pipeline: Option<Arc<wgpu::RenderPipeline>>,
    params_offset: Option<u32>,
    resources: Option<wgpu::BindGroup>,
    stream: bool,
    viewport: Option<[f32; 6]>,
    scissor: Option<[u32; 4]>,
    stencil_reference: Option<u8>,
    blend_constant: Option<[f32; 4]>,
}

impl LatteRenderer {
    /// What the renderer holds, for finding where memory goes (`SHOCORA_PROFILE`).
    fn report_memory(&self) {
        let bytes = |texture: &wgpu::Texture| -> u64 {
            let size = texture.size();
            let format = texture.format();
            let block = format.block_dimensions();
            let per_block = u64::from(format.block_copy_size(None).unwrap_or(4));
            let base = u64::from(size.width.div_ceil(block.0))
                * u64::from(size.height.div_ceil(block.1))
                * u64::from(size.depth_or_array_layers)
                * per_block;
            if texture.mip_level_count() > 1 { base * 4 / 3 } else { base }
        };
        let surface_bytes: u64 = self.surfaces.values().map(|s| bytes(&s.texture)).sum();
        let feedback_bytes: u64 = self.feedback_copies.values().map(bytes).sum();
        println!(
            "renderer at flip {}: {} surfaces {} MB, {} feedback copies {} MB, {} pipelines, {} shader modules, {} bind groups",
            self.flips,
            self.surfaces.len(),
            surface_bytes >> 20,
            self.feedback_copies.len(),
            feedback_bytes >> 20,
            self.pipelines.len(),
            self.modules.lock().unwrap_or_else(PoisonError::into_inner).len(),
            self.bind_groups.len(),
        );
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
