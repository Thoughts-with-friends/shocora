//! Host renderer for Latte's [`RenderCommand`]s.
//!
//! | [`GraphicsApi`] | Host API through wgpu                       |
//! | --------------- | ------------------------------------------- |
//! | `Wgpu`          | best available: Vulkan / DX12 / Metal / GL  |
//! | `Vulkan`        | Vulkan only                                 |
//! | `OpenGl`        | OpenGL only (wgpu's GL backend)             |
pub mod color_grade;
pub mod framegen;
mod latte_renderer;
pub mod stream;
pub mod upscale;

pub use color_grade::{ColorGrade, ColorGradeSetting};
pub use egui_wgpu::wgpu;
pub use framegen::FrameGenSetting;
pub use latte_renderer::{
    LatteRenderer, MemoryWatch, PipelineSetting, RenderThread, ScreenImage, ScreenImages,
    request_device, wanted_features, wgpu_format,
};
pub use stream::{StreamFrame, StreamTap};
pub use upscale::{UpscaleConfig, UpscaleMethod, UpscaleSetting};
pub use wiiu_gpu::{RenderBackend, RenderCommand, Screen, render::ResolutionScale};

/// Graphics API selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphicsApi {
    /// wgpu with automatic backend choice.
    #[default]
    Wgpu,
    /// wgpu restricted to Vulkan.
    Vulkan,
    /// wgpu restricted to OpenGL.
    #[serde(rename = "opengl")]
    OpenGl,
}

impl GraphicsApi {
    pub const ALL: [Self; 3] = [Self::Wgpu, Self::Vulkan, Self::OpenGl];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Wgpu => "wgpu (auto)",
            Self::Vulkan => "Vulkan",
            Self::OpenGl => "OpenGL",
        }
    }

    /// Parses a command-line value (`wgpu`, `vulkan`, `opengl`/`gl`).
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "wgpu" | "auto" => Some(Self::Wgpu),
            "vulkan" | "vk" => Some(Self::Vulkan),
            "opengl" | "gl" | "glow" => Some(Self::OpenGl),
            _ => None,
        }
    }

    /// wgpu backends to enable.
    pub fn wgpu_backends(self) -> wgpu::Backends {
        match self {
            Self::Wgpu => {
                wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY | wgpu::Backends::GL)
            }
            Self::Vulkan => wgpu::Backends::VULKAN,
            Self::OpenGl => wgpu::Backends::GL,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_api_names() {
        assert_eq!(GraphicsApi::parse("GL"), Some(GraphicsApi::OpenGl));
        assert_eq!(GraphicsApi::parse("vulkan"), Some(GraphicsApi::Vulkan));
        assert_eq!(GraphicsApi::parse("dx9"), None);
        assert_eq!(GraphicsApi::Vulkan.wgpu_backends(), wgpu::Backends::VULKAN);
        assert_eq!(GraphicsApi::OpenGl.wgpu_backends(), wgpu::Backends::GL);
    }
}
