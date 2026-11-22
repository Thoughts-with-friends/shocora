//! Bridges the render thread's screen images to egui textures.
use std::{collections::HashMap, path::Path, sync::mpsc::SyncSender};

use eframe::{egui, egui_wgpu};
use shocora_render::{
    FrameGenSetting, LatteRenderer, MemoryWatch, PipelineSetting, RenderCommand, RenderThread,
    Screen, ScreenImages, StreamTap, UpscaleConfig, UpscaleSetting, wgpu,
};

/// Where the driver's compiled shaders stay between runs.
const PIPELINE_CACHE_DIR: &str = "shader_cache";

/// A screen ready to draw: egui texture plus its pixel size.
#[derive(Debug, Clone, Copy)]
pub struct ScreenView {
    pub texture: egui::TextureId,
    pub size: egui::Vec2,
}

/// The renderer and the textures showing its output.
pub struct Presenter {
    render_thread: RenderThread,
    screens: ScreenImages,
    upscale: UpscaleSetting,
    color_grade: shocora_render::ColorGradeSetting,
    frame_gen: FrameGenSetting,
    pipelines: PipelineSetting,
    /// Where a second player's pictures come from: the TV's, then the GamePad's.
    stream: [StreamTap; 2],
    memory_watch: MemoryWatch,
    device: wgpu::Device,
    egui_renderer: std::sync::Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>,
    /// Registered texture, the generation it shows and the image size.
    textures: HashMap<Screen, (egui::TextureId, u64, egui::Vec2)>,
    /// Filtering of the screens when drawn larger / smaller than they are.
    output_filter: wiiu_emu::OutputFilter,
}

impl Presenter {
    /// Starts the render thread on eframe's device.
    ///
    /// # Errors
    /// Fails if eframe has no wgpu context or the thread cannot start.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<Self, String> {
        let Some(state) = &cc.wgpu_render_state else {
            return Err("no wgpu device available".into());
        };
        let info = state.adapter.get_info();
        log::info!("wgpu adapter: {} ({:?})", info.name, info.backend);
        let mut renderer = LatteRenderer::new(state.device.clone(), state.queue.clone());
        renderer.use_pipeline_cache(Path::new(PIPELINE_CACHE_DIR), &info);

        let upscale = renderer.upscale_setting();
        let color_grade = renderer.color_grade_setting();
        let frame_gen = renderer.frame_gen_setting();
        let pipelines = renderer.pipeline_setting();
        let stream = Screen::ALL.map(|screen| renderer.stream_tap(screen));
        let memory_watch = renderer.memory_watch();
        let render_thread = RenderThread::spawn(renderer).map_err(|e| e.to_string())?;

        Ok(Self {
            screens: render_thread.screens(),
            upscale,
            color_grade,
            frame_gen,
            pipelines,
            stream,
            memory_watch,
            render_thread,
            device: state.device.clone(),
            egui_renderer: std::sync::Arc::clone(&state.renderer),
            textures: HashMap::new(),
            output_filter: wiiu_emu::OutputFilter::default(),
        })
    }

    /// Applies from the next flip on.
    pub fn set_upscale(&self, config: UpscaleConfig) {
        self.upscale.set(config);
    }

    /// Filtering of the screens from graphic packs' `[OutputShader]`; applies at once.
    pub fn set_output_filter(&mut self, filter: wiiu_emu::OutputFilter) {
        if self.output_filter != filter {
            self.output_filter = filter;
            // Registered again with the new sampler on the next sync.
            for (_, generation, _) in self.textures.values_mut() {
                *generation = u64::MAX;
            }
        }
    }

    fn sampler(&self) -> wgpu::SamplerDescriptor<'static> {
        let filter = |nearest| {
            if nearest { wgpu::FilterMode::Nearest } else { wgpu::FilterMode::Linear }
        };
        wgpu::SamplerDescriptor {
            label: Some("screen"),
            mag_filter: filter(self.output_filter.upscale_nearest),
            min_filter: filter(self.output_filter.downscale_nearest),
            ..Default::default()
        }
    }

    /// Applies from the next flip on.
    pub fn set_color_grade(&self, grade: shocora_render::ColorGrade) {
        self.color_grade.set(grade);
    }

    /// Applies from the next flip on.
    pub fn set_frame_generation(&self, enabled: bool) {
        self.frame_gen.set(enabled);
    }

    /// Whether draws go on without pipelines still being compiled, from the next frame on.
    pub fn set_background_shaders(&self, background: bool) {
        self.pipelines.set(background);
    }

    /// The factor that did not fit into video memory, if one did not since the last call.
    pub fn take_memory_shortage(&self) -> Option<u32> {
        self.memory_watch.take()
    }

    /// Where a second player's pictures come from: the TV's, then the GamePad's.
    pub fn stream_taps(&self) -> [StreamTap; 2] {
        self.stream.clone()
    }

    /// Where the emulator sends its work.
    pub fn sender(&self) -> Option<SyncSender<Vec<RenderCommand>>> {
        self.render_thread.sender()
    }

    /// Registers new screen images with egui.
    #[allow(clippy::cast_precision_loss)]
    pub fn sync(&mut self) {
        for screen in Screen::ALL {
            let Some(image) = self.screens.get(screen) else { continue };
            let size = egui::vec2(image.width as f32, image.height as f32);
            match self.textures.get(&screen) {
                Some((_, generation, _)) if *generation == image.generation => {}
                Some((id, _, _)) => {
                    let id = *id;
                    let sampler = self.sampler();
                    self.egui_renderer
                        .write()
                        .update_egui_texture_from_wgpu_texture_with_sampler_options(
                            &self.device,
                            &image.view,
                            sampler,
                            id,
                        );
                    self.textures.insert(screen, (id, image.generation, size));
                }
                None => {
                    let sampler = self.sampler();
                    let id =
                        self.egui_renderer.write().register_native_texture_with_sampler_options(
                            &self.device,
                            &image.view,
                            sampler,
                        );
                    self.textures.insert(screen, (id, image.generation, size));
                }
            }
        }
    }

    pub fn view(&self, screen: Screen) -> Option<ScreenView> {
        self.textures
            .get(&screen)
            .map(|(texture, _, size)| ScreenView { texture: *texture, size: *size })
    }

    pub const fn backend_name() -> &'static str {
        "wgpu"
    }
}
