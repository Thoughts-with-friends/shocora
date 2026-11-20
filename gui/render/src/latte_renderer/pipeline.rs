//! Building render pipelines, on worker threads when possible.
use super::*;

/// Identifies a host pipeline: both shaders and the fixed-function state.
pub(super) type PipelineKey = (u64, u64, u64);

pub(super) fn pipeline_key(draw: &DrawCommand) -> PipelineKey {
    (draw.vertex.id, draw.pixel.id, fast_hash(&draw.state))
}

/// The resources both shaders of a draw use, in binding order.
pub(super) fn sorted_bindings(draw: &DrawCommand) -> Vec<(u32, BindingKind)> {
    let mut bindings: Vec<(u32, BindingKind)> =
        draw.vertex.bindings.iter().chain(&draw.pixel.bindings).copied().collect();
    bindings.sort_by_key(|(binding, _)| *binding);
    bindings
}

/// How long a frame waits for its new pipelines when draws need not. What the driver has in
/// its cache arrives in time, so only pipelines it really has to compile come late.
pub(super) const PIPELINE_WAIT: Duration = Duration::from_millis(8);

/// Whether draws without a finished pipeline are left out instead of waited for, changeable
/// from any thread while the renderer runs.
#[derive(Debug, Clone, Default)]
pub struct PipelineSetting(Arc<AtomicBool>);

impl PipelineSetting {
    #[inline]
    pub fn set(&self, background: bool) {
        self.0.store(background, Ordering::Relaxed);
    }

    pub(super) fn background(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Flips without use after which a pipeline is dropped. The driver's cache makes one quick to
/// get back, while keeping thousands of them takes gigabytes of the driver's memory.
pub(super) const PIPELINE_EXPIRE_FLIPS: u64 = 18_000;
/// How often unused pipelines are looked for, and parsed shaders thrown away.
pub(super) const CLEANUP_FLIPS: u64 = 1800;
/// Bind groups kept. They keep their textures alive, and are rebuilt cheaply.
pub(super) const MAX_BIND_GROUPS: usize = 2048;

/// A compiled pipeline, `None` if the host refused it, and the flip that last used it.
pub(super) struct KnownPipeline {
    pub(super) pipeline: Option<Arc<wgpu::RenderPipeline>>,
    pub(super) last_used: u64,
}

/// What compiling a pipeline needs.
#[derive(Clone)]
pub(super) struct PipelineContext {
    pub(super) device: wgpu::Device,
    pub(super) modules: Arc<Mutex<HashMap<u64, wgpu::ShaderModule>>>,
    pub(super) cache: Option<wgpu::PipelineCache>,
}

/// A pipeline for a compiling thread to make.
pub(super) struct PipelineJob {
    /// The boot it belongs to: shader ids start over with the next.
    pub(super) epoch: u64,
    pub(super) key: PipelineKey,
    pub(super) draw: Box<DrawCommand>,
    pub(super) layout: wgpu::PipelineLayout,
    pub(super) context: PipelineContext,
}

/// A compiled pipeline: its boot, its key and the pipeline, if the host accepted it.
pub(super) type FinishedPipeline = (u64, PipelineKey, Option<Arc<wgpu::RenderPipeline>>);

/// Starts the threads compiling pipelines: creating one can take the driver a long time, and
/// they only stop when the sender is dropped.
pub(super) fn spawn_pipeline_threads() -> (Sender<PipelineJob>, Receiver<FinishedPipeline>) {
    let (jobs, job_receiver) = mpsc::channel::<PipelineJob>();
    let (finished_sender, finished) = mpsc::channel();
    let job_receiver = Arc::new(Mutex::new(job_receiver));
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get() / 2).clamp(1, 8);
    for _ in 0..threads {
        let (job_receiver, finished_sender) = (Arc::clone(&job_receiver), finished_sender.clone());
        let spawned =
            std::thread::Builder::new().name("shocora-pipeline".into()).spawn(move || {
                loop {
                    let job = job_receiver.lock().unwrap_or_else(PoisonError::into_inner).recv();
                    let Ok(job) = job else { break };
                    // A panic in the driver leaves that pipeline's draws out, and nobody waiting.
                    let pipeline = catch_unwind(AssertUnwindSafe(|| {
                        job.context.build(&job.draw, &job.layout).map(Arc::new)
                    }))
                    .unwrap_or(None);
                    if finished_sender.send((job.epoch, job.key, pipeline)).is_err() {
                        break;
                    }
                }
            });
        if let Err(error) = spawned {
            log::error!("cannot start a pipeline thread: {error}");
        }
    }
    (jobs, finished)
}

impl PipelineContext {
    fn module(&self, shader: &ShaderModule) -> wgpu::ShaderModule {
        let known =
            self.modules.lock().unwrap_or_else(PoisonError::into_inner).get(&shader.id).cloned();
        if let Some(module) = known {
            return module;
        }
        // Parsed outside the lock: this is the slow part. Another thread doing the
        // same module at the same time only wastes a little work.
        let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shocora shader"),
            source: wgpu::ShaderSource::Wgsl(if std::env::var_os("SHOCORA_DEBUG_RED").is_some() {
                shader
                    .wgsl
                    .replace(
                        "return out;",
                        "if (true) { out.c0 = vec4<f32>(1.0, 0.0, 0.0, 1.0); }\nreturn out;",
                    )
                    .into()
            } else {
                shader.wgsl.as_str().into()
            }),
        });
        self.modules
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(shader.id, module.clone());
        module
    }

    /// A pipeline the host refuses is `None`: using it would lose the whole frame, not one draw.
    pub(super) fn build(
        &self,
        draw: &DrawCommand,
        layout: &wgpu::PipelineLayout,
    ) -> Option<wgpu::RenderPipeline> {
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pipeline = self.create(draw, layout);
        match pollster::block_on(scope.pop()) {
            None => Some(pipeline),
            Some(error) => {
                log::error!(
                    "shaders vs{} ps{} rejected, draws using them are skipped: {error}",
                    draw.vertex.id,
                    draw.pixel.id
                );
                None
            }
        }
    }

    fn create(&self, draw: &DrawCommand, layout: &wgpu::PipelineLayout) -> wgpu::RenderPipeline {
        let vs = self.module(&draw.vertex);
        let fs = self.module(&draw.pixel);
        let state = &draw.state;
        let targets: Vec<Option<wgpu::ColorTargetState>> = (0..8)
            .map(|i| {
                state.color_formats[i].map(|format| {
                    let wgpu_format = wgpu_format(format);
                    let integer = matches!(format.kind(), TextureKind::Uint | TextureKind::Sint);
                    let blend = state.blend[i];
                    let blendable = wgpu_format
                        .guaranteed_format_features(self.device.features())
                        .flags
                        .contains(wgpu::TextureFormatFeatureFlags::BLENDABLE);
                    wgpu::ColorTargetState {
                        format: wgpu_format,
                        blend: (blend.enabled && !integer && blendable).then_some(
                            wgpu::BlendState {
                                color: blend_component(
                                    blend.color_src,
                                    blend.color_dst,
                                    blend.color_op,
                                ),
                                alpha: blend_component(
                                    blend.alpha_src,
                                    blend.alpha_dst,
                                    blend.alpha_op,
                                ),
                            },
                        ),
                        write_mask: wgpu::ColorWrites::from_bits_truncate(u32::from(
                            state.color_masks[i] & 0xF,
                        )),
                    }
                })
            })
            .collect();
        let last = targets.iter().rposition(Option::is_some).map_or(0, |i| i + 1);
        let fragment = (!draw.stream_only).then_some(wgpu::FragmentState {
            module: &fs,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            targets: &targets[..last],
        });
        let triangles = matches!(
            state.topology,
            Topology::Triangles | Topology::Rects | Topology::TriangleStrip
        );
        let depth_stencil = state.depth_format.map(|format| {
            let face = |f: wiiu_gpu::render::StencilFace| wgpu::StencilFaceState {
                compare: compare_function(f.compare),
                fail_op: stencil_op(f.fail),
                depth_fail_op: stencil_op(f.depth_fail),
                pass_op: stencil_op(f.pass),
            };
            let has_stencil = format.has_stencil() && state.stencil_test;
            wgpu::DepthStencilState {
                format: wgpu_format(format),
                depth_write_enabled: Some(state.depth_write),
                depth_compare: Some(if state.depth_test {
                    compare_function(state.depth_compare)
                } else {
                    wgpu::CompareFunction::Always
                }),
                stencil: if has_stencil {
                    wgpu::StencilState {
                        front: face(state.stencil_front),
                        back: face(state.stencil_back),
                        read_mask: u32::from(state.stencil_read_mask),
                        write_mask: u32::from(state.stencil_write_mask),
                    }
                } else {
                    wgpu::StencilState::default()
                },
                // The host biases triangles only.
                bias: if triangles {
                    wgpu::DepthBiasState {
                        constant: f32::from_bits(state.depth_bias[1]) as i32,
                        slope_scale: f32::from_bits(state.depth_bias[0]) / 16.0,
                        clamp: f32::from_bits(state.depth_bias[2]),
                    }
                } else {
                    wgpu::DepthBiasState::default()
                },
            }
        });
        let topology = match state.topology {
            Topology::Points => wgpu::PrimitiveTopology::PointList,
            Topology::Lines => wgpu::PrimitiveTopology::LineList,
            Topology::LineStrip => wgpu::PrimitiveTopology::LineStrip,
            Topology::Triangles | Topology::Rects => wgpu::PrimitiveTopology::TriangleList,
            Topology::TriangleStrip => wgpu::PrimitiveTopology::TriangleStrip,
        };
        let strip = matches!(state.topology, Topology::LineStrip | Topology::TriangleStrip);
        let cull_mode = match (state.cull_front, state.cull_back) {
            (true, false) => Some(wgpu::Face::Front),
            (false, true) => Some(wgpu::Face::Back),
            _ => None,
        };
        self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shocora pipeline"),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: &vs,
                entry_point: Some(if state.topology == Topology::Rects {
                    wiiu_gpu::shader::RECT_ENTRY_POINT
                } else {
                    "main"
                }),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology,
                strip_index_format: strip.then_some(wgpu::IndexFormat::Uint32),
                front_face: if state.front_face_cw {
                    wgpu::FrontFace::Cw
                } else {
                    wgpu::FrontFace::Ccw
                },
                cull_mode,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil,
            multisample: wgpu::MultisampleState::default(),
            fragment,
            multiview_mask: None,
            cache: self.cache.as_ref(),
        })
    }
}
