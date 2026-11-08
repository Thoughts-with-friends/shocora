//! The wgpu device the renderer wants.
use super::*;

/// Features the renderer uses when the adapter offers them.
pub fn wanted_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    adapter.features()
        & (wgpu::Features::TEXTURE_COMPRESSION_BC
            // Titles keep 3D textures (colour grading tables, volumes) in BC formats too.
            | wgpu::Features::TEXTURE_COMPRESSION_BC_SLICED_3D
            | wgpu::Features::DEPTH32FLOAT_STENCIL8
            | wgpu::Features::FLOAT32_FILTERABLE
            | wgpu::Features::TEXTURE_FORMAT_16BIT_NORM
            | wgpu::Features::RG11B10UFLOAT_RENDERABLE
            | wgpu::Features::DEPTH_CLIP_CONTROL
            | wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER
            | wgpu::Features::PIPELINE_CACHE
            // Stream-out: vertex shaders store their results in a buffer.
            | wgpu::Features::VERTEX_WRITABLE_STORAGE
            // Latte renders to formats WebGPU keeps read-only, such as RGBA8 SNORM.
            | wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES)
}

/// Requests the device the renderer wants from `adapter`.
///
/// # Errors
/// Fails if the adapter refuses.
pub async fn request_device(
    adapter: &wgpu::Adapter,
) -> Result<(wgpu::Device, wgpu::Queue), wgpu::RequestDeviceError> {
    adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("shocora"),
            required_features: wanted_features(adapter),
            required_limits: adapter.limits(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        })
        .await
}
