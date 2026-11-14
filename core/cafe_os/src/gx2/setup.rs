//! GX2 initialisation helpers with little or no GPU-visible effect.
use wiiu_hle_macros::hle_export;

use crate::hle::{HleContext, HleError};

/// GX2R resource allocator callbacks (used by GX2R buffer creation).
#[hle_export(lib = "gx2", name = "GX2RSetAllocator")]
fn gx2r_set_allocator(ctx: &mut HleContext<'_>, alloc: u32, free: u32) {
    ctx.os.gx2.gx2r_allocator = (alloc, free);
}

/// Cache invalidation: memory is always coherent here, but the GPU learns which buffers
/// the CPU wrote. Whole-memory invalidations are ignored, as Cemu does.
#[hle_export(lib = "gx2", name = "GX2Invalidate")]
fn gx2_invalidate(ctx: &mut HleContext<'_>, _flags: u32, address: u32, size: u32) {
    if address != 0 && size != u32::MAX {
        ctx.gpu.note_cpu_write(address, size);
    }
}

/// Scan buffers are recorded by `GX2CopyColorBufferToScanBuffer`; the TV/DRC
/// buffer memory itself is not needed.
#[hle_export(lib = "gx2", name = "GX2SetTVBuffer")]
fn gx2_set_tv_buffer(
    _ctx: &mut HleContext<'_>,
    _buffer: u32,
    _size: u32,
    _mode: u32,
    _format: u32,
    _buffering: u32,
) {
}

#[hle_export(lib = "gx2", name = "GX2SetDRCBuffer")]
fn gx2_set_drc_buffer(
    _ctx: &mut HleContext<'_>,
    _buffer: u32,
    _size: u32,
    _mode: u32,
    _format: u32,
    _buffering: u32,
) {
}

#[hle_export(lib = "gx2", name = "GX2SetTVScale")]
fn gx2_set_tv_scale(_ctx: &mut HleContext<'_>, _width: u32, _height: u32) {}

#[hle_export(lib = "gx2", name = "GX2SetDRCScale")]
fn gx2_set_drc_scale(_ctx: &mut HleContext<'_>, _width: u32, _height: u32) {}

/// `SQ_PGM_RESOURCES_*` is the first register of every GX2 shader structure.
fn resources(ctx: &HleContext<'_>, shader: u32) -> Result<u32, HleError> {
    Ok(ctx.memory.read_u32(shader)?)
}

#[hle_export(lib = "gx2", name = "GX2GetVertexShaderGPRs")]
fn gx2_get_vertex_shader_gprs(ctx: &mut HleContext<'_>, shader: u32) -> Result<u32, HleError> {
    Ok(resources(ctx, shader)? & 0xFF)
}

#[hle_export(lib = "gx2", name = "GX2GetVertexShaderStackEntries")]
fn gx2_get_vertex_shader_stack_entries(
    ctx: &mut HleContext<'_>,
    shader: u32,
) -> Result<u32, HleError> {
    Ok((resources(ctx, shader)? >> 8) & 0xFF)
}

#[hle_export(lib = "gx2", name = "GX2GetPixelShaderGPRs")]
fn gx2_get_pixel_shader_gprs(ctx: &mut HleContext<'_>, shader: u32) -> Result<u32, HleError> {
    Ok(resources(ctx, shader)? & 0xFF)
}

#[hle_export(lib = "gx2", name = "GX2GetPixelShaderStackEntries")]
fn gx2_get_pixel_shader_stack_entries(
    ctx: &mut HleContext<'_>,
    shader: u32,
) -> Result<u32, HleError> {
    Ok((resources(ctx, shader)? >> 8) & 0xFF)
}

#[hle_export(lib = "gx2", name = "GX2GetGeometryShaderGPRs")]
fn gx2_get_geometry_shader_gprs(ctx: &mut HleContext<'_>, shader: u32) -> Result<u32, HleError> {
    Ok(resources(ctx, shader)? & 0xFF)
}

#[hle_export(lib = "gx2", name = "GX2CalcGeometryShaderInputRingBufferSize")]
const fn gx2_calc_geometry_shader_input_ring_buffer_size(
    _ctx: &mut HleContext<'_>,
    item_size: u32,
) -> u32 {
    item_size.wrapping_mul(4).wrapping_mul(0x1000)
}

#[hle_export(lib = "gx2", name = "GX2CalcGeometryShaderOutputRingBufferSize")]
const fn gx2_calc_geometry_shader_output_ring_buffer_size(
    _ctx: &mut HleContext<'_>,
    item_size: u32,
) -> u32 {
    item_size.wrapping_mul(4).wrapping_mul(0x1000)
}

#[hle_export(lib = "gx2", name = "GX2GetGeometryShaderStackEntries")]
fn gx2_get_geometry_shader_stack_entries(
    ctx: &mut HleContext<'_>,
    shader: u32,
) -> Result<u32, HleError> {
    Ok((resources(ctx, shader)? >> 8) & 0xFF)
}

#[hle_export(lib = "gx2", name = "GX2PerfInit")]
fn gx2_perf_init(_ctx: &mut HleContext<'_>, _perf: u32, _max_tags: u32, _allocator: u32) {}

#[hle_export(lib = "gx2", name = "GX2PerfSetCollectionMethod")]
fn gx2_perf_set_collection_method(_ctx: &mut HleContext<'_>, _perf: u32, _method: u32) {}

#[hle_export(lib = "gx2", name = "GX2PerfMetricsClear")]
fn gx2_perf_metrics_clear(_ctx: &mut HleContext<'_>, _perf: u32) {}

#[hle_export(lib = "gx2", name = "GX2PerfTagEnable")]
fn gx2_perf_tag_enable(_ctx: &mut HleContext<'_>, _perf: u32, _tag: u32, _enable: bool) {}

#[cfg(test)]
mod tests {
    use crate::hle::test_env::{SCRATCH, TestEnv};

    /// Intent: a shader's resource word (`SQ_PGM_RESOURCES_*`, the first word of
    /// `GX2VertexShader`) holds the GPRs it needs in bits 0-7 and its stack size in 8-15;
    /// titles read them to size their register budget.
    /// Pass: 0x0000_0A21 gives 0x21 GPRs and 0x0A stack entries.
    #[test]
    fn shader_resources_split_into_gprs_and_stack() {
        let mut env = TestEnv::new();
        env.memory.write_u32(SCRATCH, 0x0000_0A21).unwrap();
        assert_eq!(env.call("gx2", "GX2GetVertexShaderGPRs", &[SCRATCH]), 0x21);
        assert_eq!(env.call("gx2", "GX2GetVertexShaderStackEntries", &[SCRATCH]), 0x0A);
        assert_eq!(env.call("gx2", "GX2GetPixelShaderGPRs", &[SCRATCH]), 0x21);
    }
}
