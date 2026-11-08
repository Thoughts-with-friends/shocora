//! shocora — egui front end.
//!
//! ```text
//! shocora [--graphics wgpu|vulkan|opengl] [TITLE_DIR | FILE.rpx]
//! ```
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod fonts;
mod lang;
mod launcher;
mod netplay;
mod presenter;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
};

use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
use shocora_gui_common::config::AppConfig;
use shocora_render::{GraphicsApi, wgpu};

/// Parsed command line.
#[derive(Debug, Default)]
struct Args {
    graphics: Option<GraphicsApi>,
    title: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut iter = std::env::args_os().skip(1);
    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("--graphics" | "-g") => {
                let value = iter.next().and_then(|v| v.into_string().ok()).unwrap_or_default();
                args.graphics = Some(
                    GraphicsApi::parse(&value)
                        .ok_or_else(|| format!("unknown graphics API {value:?}"))?,
                );
            }
            Some("--help" | "-h") => {
                return Err(
                    "usage: shocora [--graphics wgpu|vulkan|opengl] [TITLE_DIR | FILE.rpx]".into(),
                );
            }
            _ => args.title = Some(PathBuf::from(arg)),
        }
    }
    Ok(args)
}

/// The adapter to use: the one named if it is there, else the fastest kind on offer. Adapters
/// come in the order of their backends, so a GPU seen through several gets its first one.
fn choose_adapter<'a>(
    adapters: &'a [wgpu::Adapter],
    surface: Option<&wgpu::Surface<'_>>,
    wanted: Option<&str>,
    found: &Mutex<Vec<String>>,
) -> Result<&'a wgpu::Adapter, String> {
    let usable: Vec<_> = adapters
        .iter()
        .filter(|adapter| adapter.get_info().device_type != wgpu::DeviceType::Cpu)
        .filter(|adapter| surface.is_none_or(|surface| adapter.is_surface_supported(surface)))
        .collect();
    let mut names: Vec<String> = usable.iter().map(|adapter| adapter.get_info().name).collect();
    names.sort();
    names.dedup();
    *found.lock().unwrap_or_else(PoisonError::into_inner) = names;

    let named = wanted.and_then(|wanted| {
        let adapter = usable.iter().find(|adapter| adapter.get_info().name == wanted);
        if adapter.is_none() {
            log::warn!("GPU {wanted:?} not found: choosing one");
        }
        adapter
    });
    let rank = |adapter: &&&wgpu::Adapter| match adapter.get_info().device_type {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        _ => 2,
    };
    named
        .or_else(|| usable.iter().min_by_key(rank))
        .copied()
        .ok_or_else(|| "no GPU can draw to this window".to_owned())
}

fn native_options(
    api: GraphicsApi,
    transparent: bool,
    gpu: Option<String>,
    gpus: Arc<Mutex<Vec<String>>>,
) -> eframe::NativeOptions {
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("shocora")
        .with_inner_size([1400.0, 640.0])
        // Only a see-through backdrop color needs the desktop behind the window: a
        // transparent window is composed by the desktop on every frame, which costs GPU time.
        .with_transparent(transparent);

    match eframe::icon_data::from_png_bytes(include_bytes!("../assets/shocora.png")) {
        Ok(icon) => viewport = viewport.with_icon(icon),
        Err(error) => log::warn!("window icon: {error}"),
    }

    let mut options = eframe::NativeOptions { viewport, ..eframe::NativeOptions::default() };
    options.renderer = eframe::Renderer::Wgpu;

    let mut setup = WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = api.wgpu_backends();
    setup.native_adapter_selector = Some(Arc::new(move |adapters, surface| {
        choose_adapter(adapters, surface, gpu.as_deref(), &gpus).cloned()
    }));

    // The renderer needs more than egui's defaults: compressed textures and big storage buffers.
    setup.device_descriptor = std::sync::Arc::new(|adapter| wgpu::DeviceDescriptor {
        label: Some("shocora"),
        required_features: shocora_render::wanted_features(adapter),
        required_limits: adapter.limits(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        ..Default::default()
    });
    options.wgpu_options =
        WgpuConfiguration { wgpu_setup: WgpuSetup::CreateNew(setup), ..Default::default() };
    options
}

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    // Read shocora.toml
    let config_path = AppConfig::default_path();
    let mut config = AppConfig::load(&config_path);

    // Override graphics API
    if let Some(api) = args.graphics {
        config.graphics_api = api;
    }
    let api = config.graphics_api;
    log::info!("graphics API: {}", api.label());
    let transparent = config.launcher_background[3] < u8::MAX;

    // Filled in while the window finds its GPU, which comes before the app is made.
    let gpus = Arc::new(Mutex::new(Vec::new()));
    let options = native_options(api, transparent, config.gpu.clone(), Arc::clone(&gpus));
    eframe::run_native(
        "shocora",
        options,
        Box::new(move |cc| {
            let gpus = std::mem::take(&mut *gpus.lock().unwrap_or_else(PoisonError::into_inner));
            Ok(Box::new(app::ShocoraApp::new(cc, config, config_path, api, gpus, args.title)?))
        }),
    )
}
