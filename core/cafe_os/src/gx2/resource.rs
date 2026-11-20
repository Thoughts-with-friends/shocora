//! GX2R: buffers and surfaces whose memory comes from the title's allocator
//! (`GX2RSetAllocator`). Port of Cemu's GX2_Resource.
//!
//! Creating a resource calls the guest allocator: the export jumps to it and an
//! internal stub picks up the returned pointer before going back to the caller.
use wiiu_hle_macros::hle_export;

use super::{Gx2Surface, physical};
use crate::{
    hle::{HleContext, HleError},
    process::INTERNAL_LIBRARY,
};

/// `GX2R_RESFLAG_*`.
pub mod flag {
    pub const VERTEX_BUFFER: u32 = 1 << 4;
    pub const INDEX_BUFFER: u32 = 1 << 5;
    pub const UNIFORM_BLOCK: u32 = 1 << 6;
    pub const SHADER_PROGRAM: u32 = 1 << 7;
    pub const STREAM_OUTPUT: u32 = 1 << 8;
    pub const DISPLAY_LIST: u32 = 1 << 9;
    pub const GS_RINGBUFFER: u32 = 1 << 10;
    pub const CPU_READ: u32 = 1 << 11;
    pub const CPU_WRITE: u32 = 1 << 12;
    pub const GPU_READ: u32 = 1 << 13;
    pub const GPU_WRITE: u32 = 1 << 14;
    /// Bits 19-23: passed on to the free callback of a surface.
    pub const FREE_PASSTHROUGH: u32 = 0x1F << 19;
    pub const ALLOCATED_BY_GX2R: u32 = 1 << 29;
    pub const LOCKED: u32 = 1 << 30;
}

/// `GX2RBuffer` fields.
mod buffer {
    pub const FLAGS: u32 = 0x0;
    pub const ELEMENT_SIZE: u32 = 0x4;
    pub const ELEMENT_COUNT: u32 = 0x8;
    pub const PTR: u32 = 0xC;
}

/// What the allocator's result is stored into once it returns.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingAlloc {
    Buffer(u32),
    Surface(u32),
}

/// A creation waiting for the guest allocator, with the caller's `lr`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingCreate {
    pub target: PendingAlloc,
    pub lr: u32,
}

struct RBuffer {
    flags: u32,
    element_size: u32,
    size: u32,
    ptr: u32,
}

fn read_buffer(ctx: &HleContext<'_>, address: u32) -> Result<RBuffer, HleError> {
    let element_size = ctx.memory.read_u32(address + buffer::ELEMENT_SIZE)?;
    let count = ctx.memory.read_u32(address + buffer::ELEMENT_COUNT)?;
    Ok(RBuffer {
        flags: ctx.memory.read_u32(address + buffer::FLAGS)?,
        element_size,
        size: element_size.wrapping_mul(count),
        ptr: ctx.memory.read_u32(address + buffer::PTR)?,
    })
}

const fn allocation_size(size: u32) -> u32 {
    size.wrapping_add(0x3F) & !0x3F
}

const fn alignment(flags: u32) -> u32 {
    if flags
        & (flag::STREAM_OUTPUT | flag::UNIFORM_BLOCK | flag::SHADER_PROGRAM | flag::GS_RINGBUFFER)
        != 0
    {
        return 0x100;
    }
    if flags & (flag::VERTEX_BUFFER | flag::INDEX_BUFFER | flag::DISPLAY_LIST) != 0 {
        return 0x40;
    }
    0x100
}

/// `(alloc, free)` of GX2R: the title's (`GX2RSetAllocator`) or, as GX2 sets them up
/// in `GX2Init`, the default ones that go through the GX2 default allocator.
fn gx2r_allocator(ctx: &mut HleContext<'_>) -> Result<(u32, u32), HleError> {
    let set = ctx.os.gx2.gx2r_allocator;
    if set != (0, 0) {
        return Ok(set);
    }
    let alloc = ctx.os.registry.function_stub(INTERNAL_LIBRARY, GX2R_DEFAULT_ALLOC.name);
    let free = ctx.os.registry.function_stub(INTERNAL_LIBRARY, GX2R_DEFAULT_FREE.name);
    ctx.os.registry.commit(ctx.memory)?;
    ctx.os.gx2.gx2r_allocator = (alloc, free);
    Ok((alloc, free))
}

/// Value of a coreinit function-pointer variable (`MEMAllocFromDefaultHeapEx`, ...):
/// titles may replace them.
fn coreinit_pointer(ctx: &mut HleContext<'_>, name: &str) -> Result<u32, HleError> {
    let slot = ctx.os.registry.data_address("coreinit", name);
    ctx.os.registry.commit(ctx.memory)?;
    Ok(ctx.memory.read_u32(slot)?)
}

/// `GX2SetDefaultAllocator`: `alloc(user, size, align)` and `free(user, ptr)`.
#[hle_export(lib = "gx2", name = "GX2SetDefaultAllocator")]
fn gx2_set_default_allocator(ctx: &mut HleContext<'_>, alloc: u32, free: u32) {
    ctx.os.gx2.default_allocator = (alloc, free);
}

/// Default GX2R alloc `(flags, size, align)`: tail-calls the GX2 default allocator with
/// the flags as its user parameter.
#[hle_export(lib = "shocora", name = "__shocora_GX2RDefaultAlloc")]
fn gx2r_default_alloc(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    let (alloc, _) = ctx.os.gx2.default_allocator;
    if alloc != 0 {
        ctx.cpu.pc = alloc;
        return Ok(());
    }
    // GX2 default allocator: MEMAllocFromDefaultHeapEx(size, align).
    ctx.cpu.gpr[3] = ctx.cpu.gpr[4];
    ctx.cpu.gpr[4] = ctx.cpu.gpr[5];
    ctx.cpu.pc = coreinit_pointer(ctx, "MEMAllocFromDefaultHeapEx")?;
    Ok(())
}

/// Default GX2R free `(flags, ptr)`, through the GX2 default allocator.
#[hle_export(lib = "shocora", name = "__shocora_GX2RDefaultFree")]
fn gx2r_default_free(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    let (_, free) = ctx.os.gx2.default_allocator;
    if free != 0 {
        ctx.cpu.pc = free;
        return Ok(());
    }
    // GX2 default free: MEMFreeToDefaultHeap(ptr).
    ctx.cpu.gpr[3] = ctx.cpu.gpr[4];
    ctx.cpu.pc = coreinit_pointer(ctx, "MEMFreeToDefaultHeap")?;
    Ok(())
}

/// Jumps to the guest allocator with `(flags, size, align)`; `__shocora_GX2RAllocReturn`
/// finishes the creation. Returns false (no call) without an allocator.
fn call_allocator(
    ctx: &mut HleContext<'_>,
    target: PendingAlloc,
    args: [u32; 3],
) -> Result<bool, HleError> {
    let (alloc, _) = gx2r_allocator(ctx)?;
    if alloc == 0 {
        log::warn!("GX2R resource created without an allocator");
        return Ok(false);
    }
    let stub = ctx.os.registry.function_stub(INTERNAL_LIBRARY, GX2R_ALLOC_RETURN.name);
    ctx.os.registry.commit(ctx.memory)?;
    let me = ctx.os.coreinit.threads.current;
    ctx.os.gx2.gx2r_pending.entry(me).or_default().push(PendingCreate { target, lr: ctx.cpu.lr });
    ctx.cpu.gpr[3..6].copy_from_slice(&args);
    ctx.cpu.pc = alloc;
    ctx.cpu.lr = stub;
    Ok(true)
}

/// The guest allocator returned `r3`: store it and return success to the creator.
#[hle_export(lib = "shocora", name = "__shocora_GX2RAllocReturn")]
fn gx2r_alloc_return(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    let me = ctx.os.coreinit.threads.current;
    let Some(pending) = ctx.os.gx2.gx2r_pending.get_mut(&me).and_then(Vec::pop) else {
        return Err(HleError::Fatal("GX2R allocator returned without a creation".into()));
    };
    let ptr = ctx.cpu.gpr[3];
    match pending.target {
        PendingAlloc::Buffer(address) => ctx.memory.write_u32(address + buffer::PTR, ptr)?,
        PendingAlloc::Surface(address) => {
            let mut s = Gx2Surface::read(ctx.memory, address)?;
            s.image = ptr;
            s.mip_ptr = if ptr != 0 && s.mip_size > 0 { ptr + s.image_size } else { 0 };
            s.write(ctx.memory, address)?;
        }
    }
    ctx.cpu.lr = pending.lr;
    ctx.cpu.gpr[3] = u32::from(ptr != 0);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2RGetBufferAllocationSize")]
fn gx2r_get_buffer_allocation_size(
    ctx: &mut HleContext<'_>,
    address: u32,
) -> Result<u32, HleError> {
    Ok(allocation_size(read_buffer(ctx, address)?.size))
}

#[hle_export(lib = "gx2", name = "GX2RGetBufferAlignment")]
const fn gx2r_get_buffer_alignment(_ctx: &mut HleContext<'_>, flags: u32) -> u32 {
    alignment(flags)
}

#[hle_export(lib = "gx2", name = "GX2RCreateBuffer")]
fn gx2r_create_buffer(ctx: &mut HleContext<'_>, address: u32) -> Result<(), HleError> {
    let b = read_buffer(ctx, address)?;
    let flags = (b.flags & !flag::LOCKED) | flag::ALLOCATED_BY_GX2R;
    ctx.memory.write_u32(address + buffer::FLAGS, flags)?;
    let args = [b.flags, allocation_size(b.size), alignment(b.flags)];
    if !call_allocator(ctx, PendingAlloc::Buffer(address), args)? {
        ctx.memory.write_u32(address + buffer::PTR, 0)?;
        ctx.cpu.gpr[3] = 0;
    }
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2RCreateBufferUserMemory")]
fn gx2r_create_buffer_user_memory(
    ctx: &mut HleContext<'_>,
    address: u32,
    ptr: u32,
    _size: u32,
) -> Result<bool, HleError> {
    let flags = ctx.memory.read_u32(address + buffer::FLAGS)?;
    ctx.memory.write_u32(address + buffer::PTR, ptr)?;
    ctx.memory
        .write_u32(address + buffer::FLAGS, flags & !(flag::LOCKED | flag::ALLOCATED_BY_GX2R))?;
    Ok(true)
}

/// Clears the pointer and tail-calls the free callback with `(flags, ptr)` for buffers
/// GX2R allocated.
#[hle_export(lib = "gx2", name = "GX2RDestroyBufferEx")]
fn gx2r_destroy_buffer_ex(
    ctx: &mut HleContext<'_>,
    address: u32,
    _flags: u32,
) -> Result<(), HleError> {
    let b = read_buffer(ctx, address)?;
    ctx.memory.write_u32(address + buffer::PTR, 0)?;
    let (_, free) = gx2r_allocator(ctx)?;
    if b.flags & flag::ALLOCATED_BY_GX2R != 0 && free != 0 {
        ctx.cpu.gpr[3] = b.flags;
        ctx.cpu.gpr[4] = b.ptr;
        ctx.cpu.pc = free;
    }
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2RBufferExists")]
fn gx2r_buffer_exists(ctx: &mut HleContext<'_>, address: u32) -> Result<bool, HleError> {
    Ok(address != 0 && read_buffer(ctx, address)?.ptr != 0)
}

#[hle_export(lib = "gx2", name = "GX2RSetBufferName")]
fn gx2r_set_buffer_name(_ctx: &mut HleContext<'_>, _address: u32, _name: u32) {}

#[hle_export(lib = "gx2", name = "GX2RLockBufferEx")]
fn gx2r_lock_buffer_ex(
    ctx: &mut HleContext<'_>,
    address: u32,
    _flags: u32,
) -> Result<u32, HleError> {
    Ok(read_buffer(ctx, address)?.ptr)
}

/// The CPU may have written the buffer: the GPU must see the new contents.
fn note_written(ctx: &mut HleContext<'_>, address: u32) -> Result<(), HleError> {
    let b = read_buffer(ctx, address)?;
    if b.ptr != 0 && b.size != 0 {
        ctx.gpu.note_cpu_write(b.ptr, b.size);
    }
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2RUnlockBufferEx")]
fn gx2r_unlock_buffer_ex(
    ctx: &mut HleContext<'_>,
    address: u32,
    _flags: u32,
) -> Result<(), HleError> {
    note_written(ctx, address)
}

#[hle_export(lib = "gx2", name = "GX2RInvalidateBuffer")]
fn gx2r_invalidate_buffer(
    ctx: &mut HleContext<'_>,
    address: u32,
    _flags: u32,
) -> Result<(), HleError> {
    note_written(ctx, address)
}

#[hle_export(lib = "gx2", name = "GX2RCreateSurface")]
fn gx2r_create_surface(ctx: &mut HleContext<'_>, address: u32, flags: u32) -> Result<(), HleError> {
    let mut s = Gx2Surface::read(ctx.memory, address)?;
    s.resource_flags = flags;
    s.calc_size_and_alignment();
    s.resource_flags = (s.resource_flags & !flag::LOCKED) | flag::ALLOCATED_BY_GX2R;
    s.image = 0;
    s.mip_ptr = 0;
    s.write(ctx.memory, address)?;
    let args = [s.resource_flags, s.image_size.wrapping_add(s.mip_size), s.alignment];
    if !call_allocator(ctx, PendingAlloc::Surface(address), args)? {
        ctx.cpu.gpr[3] = 0;
    }
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2RCreateSurfaceUserMemory")]
fn gx2r_create_surface_user_memory(
    ctx: &mut HleContext<'_>,
    address: u32,
    image: u32,
    mip: u32,
    flags: u32,
) -> Result<bool, HleError> {
    let mut s = Gx2Surface::read(ctx.memory, address)?;
    s.resource_flags = flags & !(flag::LOCKED | flag::ALLOCATED_BY_GX2R);
    s.calc_size_and_alignment();
    s.image = image;
    s.mip_ptr = mip;
    s.write(ctx.memory, address)?;
    Ok(true)
}

#[hle_export(lib = "gx2", name = "GX2RDestroySurfaceEx")]
fn gx2r_destroy_surface_ex(
    ctx: &mut HleContext<'_>,
    address: u32,
    flags: u32,
) -> Result<(), HleError> {
    let mut s = Gx2Surface::read(ctx.memory, address)?;
    let image = s.image;
    s.image = 0;
    s.write(ctx.memory, address)?;
    let (_, free) = gx2r_allocator(ctx)?;
    if s.resource_flags & flag::ALLOCATED_BY_GX2R != 0 && free != 0 {
        ctx.cpu.gpr[3] = s.resource_flags | (flags & flag::FREE_PASSTHROUGH);
        ctx.cpu.gpr[4] = image;
        ctx.cpu.pc = free;
    }
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2RSurfaceExists")]
fn gx2r_surface_exists(ctx: &mut HleContext<'_>, address: u32) -> Result<bool, HleError> {
    if address == 0 {
        return Ok(false);
    }
    let s = Gx2Surface::read(ctx.memory, address)?;
    let usage = flag::CPU_READ | flag::CPU_WRITE | flag::GPU_READ | flag::GPU_WRITE;
    Ok(s.image != 0 && s.resource_flags & usage != 0)
}

#[hle_export(lib = "gx2", name = "GX2RLockSurfaceEx")]
fn gx2r_lock_surface_ex(
    ctx: &mut HleContext<'_>,
    address: u32,
    _level: u32,
    _flags: u32,
) -> Result<u32, HleError> {
    let mut s = Gx2Surface::read(ctx.memory, address)?;
    s.resource_flags |= flag::LOCKED;
    s.write(ctx.memory, address)?;
    Ok(s.image)
}

#[hle_export(lib = "gx2", name = "GX2RUnlockSurfaceEx")]
fn gx2r_unlock_surface_ex(
    ctx: &mut HleContext<'_>,
    address: u32,
    _level: u32,
    _flags: u32,
) -> Result<(), HleError> {
    let mut s = Gx2Surface::read(ctx.memory, address)?;
    s.resource_flags &= !flag::LOCKED;
    s.write(ctx.memory, address)?;
    if s.image != 0 {
        ctx.gpu.note_cpu_write(s.image, s.image_size);
    }
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2RBeginDisplayListEx")]
fn gx2r_begin_display_list_ex(
    ctx: &mut HleContext<'_>,
    address: u32,
    _profiling: u32,
    _flags: u32,
) -> Result<(), HleError> {
    let b = read_buffer(ctx, address)?;
    ctx.os.gx2.begin_display_list(b.ptr, b.size);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2REndDisplayList")]
fn gx2r_end_display_list(ctx: &mut HleContext<'_>, address: u32) -> Result<u32, HleError> {
    let ptr = read_buffer(ctx, address)?.ptr;
    super::command::gx2_end_display_list(ctx, ptr)
}

#[hle_export(lib = "gx2", name = "GX2RCallDisplayList")]
fn gx2r_call_display_list(
    ctx: &mut HleContext<'_>,
    address: u32,
    size: u32,
) -> Result<(), HleError> {
    let ptr = read_buffer(ctx, address)?.ptr;
    ctx.os.gx2.emit(wiiu_gpu::command::opcode::INDIRECT_BUFFER, &[physical(ptr), 0, size / 4]);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2RDirectCallDisplayList")]
fn gx2r_direct_call_display_list(
    ctx: &mut HleContext<'_>,
    address: u32,
    size: u32,
) -> Result<(), HleError> {
    let ptr = read_buffer(ctx, address)?.ptr;
    super::command::gx2_direct_call_display_list(ctx, ptr, size)
}

#[hle_export(lib = "gx2", name = "GX2RDrawIndexed")]
#[allow(clippy::too_many_arguments)]
fn gx2r_draw_indexed(
    ctx: &mut HleContext<'_>,
    mode: u32,
    index_buffer: u32,
    index_type: u32,
    count: u32,
    base_index: u32,
    base_vertex: u32,
    instances: u32,
) -> Result<(), HleError> {
    let b = read_buffer(ctx, index_buffer)?;
    let indices = b.ptr.wrapping_add(base_index.wrapping_mul(b.element_size));
    super::draw::gx2_draw_indexed_ex(ctx, mode, count, index_type, indices, base_vertex, instances)
}

pub const EXPORTS: &[crate::hle::HleExport] = &[
    GX2R_ALLOC_RETURN,
    GX2_SET_DEFAULT_ALLOCATOR,
    GX2R_DEFAULT_ALLOC,
    GX2R_DEFAULT_FREE,
    GX2R_GET_BUFFER_ALLOCATION_SIZE,
    GX2R_GET_BUFFER_ALIGNMENT,
    GX2R_CREATE_BUFFER,
    GX2R_CREATE_BUFFER_USER_MEMORY,
    GX2R_DESTROY_BUFFER_EX,
    GX2R_BUFFER_EXISTS,
    GX2R_SET_BUFFER_NAME,
    GX2R_LOCK_BUFFER_EX,
    GX2R_UNLOCK_BUFFER_EX,
    GX2R_INVALIDATE_BUFFER,
    GX2R_CREATE_SURFACE,
    GX2R_CREATE_SURFACE_USER_MEMORY,
    GX2R_DESTROY_SURFACE_EX,
    GX2R_SURFACE_EXISTS,
    GX2R_LOCK_SURFACE_EX,
    GX2R_UNLOCK_SURFACE_EX,
    GX2R_BEGIN_DISPLAY_LIST_EX,
    GX2R_END_DISPLAY_LIST,
    GX2R_CALL_DISPLAY_LIST,
    GX2R_DIRECT_CALL_DISPLAY_LIST,
    GX2R_DRAW_INDEXED,
];

#[cfg(test)]
mod tests {
    use super::flag;
    use crate::hle::test_env::{SCRATCH, TestEnv};

    /// Intent: `GX2RCreateBuffer` gets its memory from the title's allocator, called with
    /// (flags, size rounded to 64 bytes, alignment), and stores the result in the buffer
    /// before returning success to the caller (Xenoblade Chronicles X crashed on a null
    /// buffer while the export was missing).
    /// Pass: the call jumps to the allocator with those arguments, and the return stub
    /// writes the pointer, returns 1 and goes back to the original `lr`.
    #[test]
    fn create_buffer_calls_the_title_allocator() {
        let mut env = TestEnv::new();
        let (alloc, free) = (0x0200_0000, 0x0200_0100);
        env.call("gx2", "GX2RSetAllocator", &[alloc, free]);
        let buffer = SCRATCH;
        let flags = flag::VERTEX_BUFFER | flag::CPU_WRITE;
        for (offset, value) in [(0, flags), (4, 12), (8, 10), (12, 0)] {
            env.memory.write_u32(buffer + offset, value).unwrap();
        }
        env.cpu.lr = 0x0300_0000;
        env.call("gx2", "GX2RCreateBuffer", &[buffer]);
        assert_eq!(env.cpu.pc, alloc);
        assert_eq!(env.cpu.gpr[3..6], [flags, 0x80, 0x40]);

        let ok = env.call("shocora", "__shocora_GX2RAllocReturn", &[SCRATCH + 0x1000]);
        assert_eq!(ok, 1);
        assert_eq!(env.cpu.lr, 0x0300_0000);
        assert_eq!(env.memory.read_u32(buffer + 12).unwrap(), SCRATCH + 0x1000);
        assert_ne!(env.memory.read_u32(buffer).unwrap() & flag::ALLOCATED_BY_GX2R, 0);
        assert_eq!(env.call("gx2", "GX2RLockBufferEx", &[buffer, 0]), SCRATCH + 0x1000);

        env.call("gx2", "GX2RDestroyBufferEx", &[buffer, 0]);
        assert_eq!(env.cpu.pc, free);
        assert_eq!(env.cpu.gpr[4], SCRATCH + 0x1000);
        assert_eq!(env.call("gx2", "GX2RBufferExists", &[buffer]), 0);
    }

    /// Intent: without `GX2RSetAllocator`, GX2R allocates from the default heap through
    /// `MEMAllocFromDefaultHeapEx`, as GX2 sets up in `GX2Init` (Xenoblade Chronicles X
    /// creates buffers without setting an allocator).
    /// Pass: creation jumps to the default GX2R stub, which moves (size, align) into r3/r4
    /// and jumps to the default heap's allocation function.
    #[test]
    fn create_buffer_defaults_to_the_default_heap() {
        let mut env = TestEnv::new();
        let buffer = SCRATCH;
        let flags = flag::UNIFORM_BLOCK;
        for (offset, value) in [(0, flags), (4, 4), (8, 16), (12, 0)] {
            env.memory.write_u32(buffer + offset, value).unwrap();
        }
        env.call("gx2", "GX2RCreateBuffer", &[buffer]);
        let default_alloc = env.os.registry.function_stub("shocora", "__shocora_GX2RDefaultAlloc");
        assert_eq!(env.cpu.pc, default_alloc);
        env.call("shocora", "__shocora_GX2RDefaultAlloc", &[flags, 0x40, 0x100]);
        assert_eq!(env.cpu.gpr[3..5], [0x40, 0x100]);
        let heap_alloc = env.os.registry.function_stub("coreinit", "__shocora_DefaultHeapAllocEx");
        assert_eq!(env.cpu.pc, heap_alloc);
    }
}
