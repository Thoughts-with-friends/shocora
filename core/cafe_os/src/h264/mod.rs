//! `h264.rpl`: the hardware H.264 decoder titles play their movies with.
//!
//! Modelled on Cemu's `H264Dec.cpp`. The decoder state lives in the work memory the title
//! allocates (as on the console), so save states keep it. Pictures are not decoded: every
//! `H264DECExecute` hands one frame to the title's output callback straight away, with the
//! size and cropping of the stream's SPS, and a black picture. Titles that wait for their
//! movie frames (intro movies, attract loops) therefore keep going.
mod nal;

use std::collections::BTreeMap;

use wiiu_hle_macros::hle_export;

use crate::{
    hle::{HleContext, HleError, HleExport},
    process::INTERNAL_LIBRARY,
};

/// `H264DEC_STATUS`.
mod status {
    pub const SUCCESS: u32 = 0;
    pub const BAD_STREAM: u32 = 0x100_0000;
    pub const INVALID_PARAM: u32 = 0x101_0000;
}

/// What `H264DECExecute` returns: a frame was produced (0x80) and decoding succeeded (100).
const EXECUTE_FRAME: u32 = 0x80 | 100;
/// What it returns while waiting for an IDR slice after a restart.
const EXECUTE_RECOVERING: u32 = 0x400;
/// `H264DECFrameOutput::result` for a decoded frame.
const FRAME_RESULT: u32 = 100;

/// `H264DECSetParam` ids.
mod param {
    pub const FPTR_OUTPUT: u32 = 0x1;
    pub const OUTPUT_PER_FRAME: u32 = 0x2000_0002;
    pub const USER_MEMORY: u32 = 0x7000_0001;
}

/// Layout of the decoder state inside the title's work memory. Smaller than the 256 bytes
/// Cemu reports for its smallest requirement.
mod ctx {
    pub const BITSTREAM: u32 = 0x00;
    pub const LENGTH: u32 = 0x04;
    pub const TIMESTAMP: u32 = 0x08;
    pub const OUTPUT_FUNC: u32 = 0x10;
    pub const OUTPUT_PER_FRAME: u32 = 0x14;
    pub const USER_MEMORY: u32 = 0x18;
    pub const WIDTH: u32 = 0x1C;
    pub const HEIGHT: u32 = 0x20;
    /// Crop left, right, top, bottom; all zero without cropping.
    pub const CROP: u32 = 0x24;
    pub const BEGUN: u32 = 0x34;
    pub const RECOVERING: u32 = 0x38;
    /// `H264OutputCBStruct` (12 bytes) handed to the output callback.
    pub const CALLBACK_ARG: u32 = 0x40;
    /// The one-entry array of frame pointers it points to.
    pub const FRAME_LIST: u32 = 0x50;
    /// `H264DECFrameOutput` (0x78 bytes).
    pub const FRAME: u32 = 0x58;
    pub const SIZE: u32 = FRAME + super::frame::SIZE;
}

/// Offsets inside `H264DECFrameOutput`.
mod frame {
    pub const RESULT: u32 = 0x00;
    pub const TIMESTAMP: u32 = 0x08;
    pub const WIDTH: u32 = 0x10;
    pub const HEIGHT: u32 = 0x14;
    pub const BYTES_PER_ROW: u32 = 0x18;
    pub const CROP_ENABLE: u32 = 0x1C;
    pub const CROP: u32 = 0x20;
    pub const IMAGE: u32 = 0x44;
    pub const SIZE: u32 = 0x78;
}

/// Output callbacks running right now: the thread and the `lr` of its `H264DECExecute`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct H264State {
    returns: BTreeMap<u32, u32>,
}

/// Work memory needed for a stream of `level` (`level_idc`), as the console sizes it.
fn work_size(level: u32) -> u32 {
    let base = match level {
        0..0xB => 0x18C << 10,
        0xB => 0x384 << 10,
        0xC..=0x14 => 0x948 << 10,
        0x15 => 0x1290 << 10,
        0x16..=0x1E => 0x1FA4 << 10,
        0x1F => 0x4650 << 10,
        0x20 => 0x140_0000,
        0x21..=0x29 => 0x8000 << 10,
        0x2A => 0x220_0000,
        0x2B..=0x32 => 0x1_AF40 << 10,
        _ => 0x2_D000 << 10,
    };
    base + 0x447
}

#[hle_export(lib = "h264", name = "H264DECMemoryRequirement")]
fn h264_memory_requirement(
    ctx: &mut HleContext<'_>,
    profile: u32,
    level: u32,
    width: u32,
    height: u32,
    out: u32,
) -> Result<u32, HleError> {
    // The console rejects sizes under 64, but Pikmin 3 asks for 32 x 32 and needs success.
    let valid = (32..=2800).contains(&width)
        && (32..=1408).contains(&height)
        && level < 52
        && matches!(profile, 66 | 77 | 100);
    if !valid || out == 0 {
        return Ok(status::INVALID_PARAM);
    }
    ctx.memory.write_u32(out, work_size(level).max(ctx::SIZE))?;
    Ok(status::SUCCESS)
}

/// Any range of memory will do.
#[hle_export(lib = "h264", name = "H264DECCheckMemSegmentation")]
fn h264_check_mem_segmentation(_ctx: &mut HleContext<'_>, _memory: u32, _size: u32) -> u32 {
    0
}

/// Copies `length` guest bytes at `address`; empty if they are not mapped.
fn guest_bytes(ctx: &HleContext<'_>, address: u32, length: u32) -> Vec<u8> {
    ctx.memory.slice(address, length).map(<[u8]>::to_vec).unwrap_or_default()
}

fn find_point(
    ctx: &mut HleContext<'_>,
    data: u32,
    length: u32,
    out: u32,
    kinds: &[u8],
) -> Result<u32, HleError> {
    if data == 0 || length < 4 || out == 0 {
        return Ok(status::INVALID_PARAM);
    }
    let bytes = guest_bytes(ctx, data, length);
    let Some(at) = nal::find_point(&bytes, kinds) else { return Ok(status::BAD_STREAM) };
    ctx.memory.write_u32(out, at as u32)?;
    Ok(status::SUCCESS)
}

/// Where decoding can start: the first SPS.
#[hle_export(lib = "h264", name = "H264DECFindDecstartpoint")]
fn h264_find_decstartpoint(
    ctx: &mut HleContext<'_>,
    data: u32,
    length: u32,
    out: u32,
) -> Result<u32, HleError> {
    find_point(ctx, data, length, out, &[nal::nal_type::SPS])
}

/// Where decoding can resume: an IDR slice, or the SPS/PPS before one.
#[hle_export(lib = "h264", name = "H264DECFindIdrpoint")]
fn h264_find_idrpoint(
    ctx: &mut HleContext<'_>,
    data: u32,
    length: u32,
    out: u32,
) -> Result<u32, HleError> {
    use nal::nal_type::{IDR_SLICE, PPS, SPS};
    find_point(ctx, data, length, out, &[IDR_SLICE, SPS, PPS])
}

/// The coded picture size of the first SPS at or after `offset`.
#[hle_export(lib = "h264", name = "H264DECGetImageSize")]
fn h264_get_image_size(
    ctx: &mut HleContext<'_>,
    data: u32,
    length: i32,
    offset: i32,
    width_out: u32,
    height_out: u32,
) -> Result<u32, HleError> {
    if data == 0 || length < 4 || offset < 0 || offset + 4 >= length {
        return Ok(status::INVALID_PARAM);
    }
    if width_out == 0 || height_out == 0 {
        return Ok(status::INVALID_PARAM);
    }
    let bytes = guest_bytes(ctx, data, length.cast_unsigned());
    let Some(sps) = nal::find_sps(&bytes[offset.cast_unsigned() as usize..]) else {
        return Ok(status::BAD_STREAM);
    };
    ctx.memory.write_u32(width_out, sps.width)?;
    ctx.memory.write_u32(height_out, sps.height)?;
    Ok(status::SUCCESS)
}

/// Length of the decoding unit (one picture) starting at `offset`.
#[hle_export(lib = "h264", name = "H264DECCheckDecunitLength")]
fn h264_check_decunit_length(
    ctx: &mut HleContext<'_>,
    _work: u32,
    data: u32,
    length: u32,
    offset: u32,
    out: u32,
) -> Result<u32, HleError> {
    if offset >= length || length < 4 || out == 0 {
        return Ok(status::INVALID_PARAM);
    }
    let bytes = guest_bytes(ctx, data + offset, length - offset);
    let Some(unit) = nal::decunit_length(&bytes) else { return Ok(status::BAD_STREAM) };
    ctx.memory.write_u32(out, unit as u32)?;
    Ok(status::SUCCESS)
}

#[hle_export(lib = "h264", name = "H264DECInitParam")]
fn h264_init_param(ctx: &mut HleContext<'_>, size: u32, work: u32) -> Result<u32, HleError> {
    if work == 0 || size < ctx::SIZE {
        return Ok(status::INVALID_PARAM);
    }
    ctx.memory.fill(work, ctx::SIZE, 0)?;
    Ok(status::SUCCESS)
}

#[hle_export(lib = "h264", name = "H264DECSetParam")]
fn h264_set_param(
    ctx: &mut HleContext<'_>,
    work: u32,
    id: u32,
    value: u32,
) -> Result<u32, HleError> {
    match id {
        param::FPTR_OUTPUT => ctx.memory.write_u32(work + ctx::OUTPUT_FUNC, value)?,
        param::USER_MEMORY => ctx.memory.write_u32(work + ctx::USER_MEMORY, value)?,
        param::OUTPUT_PER_FRAME => {
            let on = value != 0 && ctx.memory.read_u8(value)? != 0;
            ctx.memory.write_u32(work + ctx::OUTPUT_PER_FRAME, u32::from(on))?;
        }
        _ => log::debug!("H264DECSetParam: parameter {id:#x} ignored"),
    }
    Ok(status::SUCCESS)
}

#[hle_export(lib = "h264", name = "H264DECSetParam_FPTR_OUTPUT")]
fn h264_set_param_output(ctx: &mut HleContext<'_>, work: u32, func: u32) -> Result<u32, HleError> {
    ctx.memory.write_u32(work + ctx::OUTPUT_FUNC, func)?;
    Ok(status::SUCCESS)
}

#[hle_export(lib = "h264", name = "H264DECSetParam_OUTPUT_PER_FRAME")]
fn h264_set_param_per_frame(ctx: &mut HleContext<'_>, work: u32, on: u32) -> Result<u32, HleError> {
    ctx.memory.write_u32(work + ctx::OUTPUT_PER_FRAME, u32::from(on != 0))?;
    Ok(status::SUCCESS)
}

/// Takes a pointer to the user value, as Cemu reads it.
#[hle_export(lib = "h264", name = "H264DECSetParam_USER_MEMORY")]
fn h264_set_param_user(ctx: &mut HleContext<'_>, work: u32, value: u32) -> Result<u32, HleError> {
    let user = if value == 0 { 0 } else { ctx.memory.read_u32(value)? };
    ctx.memory.write_u32(work + ctx::USER_MEMORY, user)?;
    Ok(status::SUCCESS)
}

#[hle_export(lib = "h264", name = "H264DECOpen")]
fn h264_open(_ctx: &mut HleContext<'_>, _work: u32) -> u32 {
    status::SUCCESS
}

#[hle_export(lib = "h264", name = "H264DECClose")]
fn h264_close(_ctx: &mut HleContext<'_>, _work: u32) -> u32 {
    status::SUCCESS
}

/// A second `Begin` restarts the stream somewhere else: frames are dropped until an IDR
/// slice comes.
#[hle_export(lib = "h264", name = "H264DECBegin")]
fn h264_begin(ctx: &mut HleContext<'_>, work: u32) -> Result<u32, HleError> {
    if ctx.memory.read_u32(work + ctx::BEGUN)? != 0 {
        ctx.memory.write_u32(work + ctx::RECOVERING, 1)?;
    }
    ctx.memory.write_u32(work + ctx::BEGUN, 1)?;
    Ok(status::SUCCESS)
}

/// Every frame went out with its `Execute`: nothing is left to flush.
#[hle_export(lib = "h264", name = "H264DECEnd")]
fn h264_end(_ctx: &mut HleContext<'_>, _work: u32) -> u32 {
    status::SUCCESS
}

#[hle_export(lib = "h264", name = "H264DECSetBitstream")]
fn h264_set_bitstream(
    ctx: &mut HleContext<'_>,
    work: u32,
    data: u32,
    length: u32,
    timestamp: f64,
) -> Result<u32, HleError> {
    ctx.memory.write_u32(work + ctx::BITSTREAM, data)?;
    ctx.memory.write_u32(work + ctx::LENGTH, length)?;
    ctx.memory.write_f64(work + ctx::TIMESTAMP, timestamp)?;
    Ok(status::SUCCESS)
}

/// Fills the NV12 picture at `image` with black, so titles show a black movie instead of
/// whatever the buffer held: luma 16, chroma 128, rows of the 256-aligned pitch the
/// decoder writes with.
fn write_black_picture(
    ctx: &mut HleContext<'_>,
    image: u32,
    width: u32,
    height: u32,
) -> Result<(), HleError> {
    let pitch = width.next_multiple_of(256);
    let luma = pitch * height;
    if image == 0 || luma == 0 || !ctx.memory.is_mapped(image, luma + luma / 2) {
        return Ok(());
    }
    ctx.memory.fill(image, luma, 16)?;
    ctx.memory.fill(image + luma, luma / 2, 128)?;
    Ok(())
}

/// Fills the frame record and the callback argument in the work memory.
fn write_frame(ctx: &mut HleContext<'_>, work: u32, image: u32) -> Result<(), HleError> {
    let out = work + ctx::FRAME;
    let width = ctx.memory.read_u32(work + ctx::WIDTH)?;
    ctx.memory.fill(out, frame::SIZE, 0)?;
    ctx.memory.write_u32(out + frame::RESULT, FRAME_RESULT)?;
    let timestamp = ctx.memory.read_f64(work + ctx::TIMESTAMP)?;
    ctx.memory.write_f64(out + frame::TIMESTAMP, timestamp)?;
    ctx.memory.write_u32(out + frame::WIDTH, width)?;
    ctx.memory.write_u32(out + frame::HEIGHT, ctx.memory.read_u32(work + ctx::HEIGHT)?)?;
    // NV12 rows of the decoder are 256-byte aligned.
    ctx.memory.write_u32(out + frame::BYTES_PER_ROW, width.next_multiple_of(256))?;
    let mut cropped = false;
    for side in 0..4 {
        let value = ctx.memory.read_u32(work + ctx::CROP + side * 4)?;
        cropped |= value != 0;
        ctx.memory.write_u32(out + frame::CROP + side * 4, value)?;
    }
    ctx.memory.write_u32(out + frame::CROP_ENABLE, u32::from(cropped))?;
    ctx.memory.write_u32(out + frame::IMAGE, image)?;
    write_black_picture(ctx, image, width, ctx.memory.read_u32(work + ctx::HEIGHT)?)?;

    ctx.memory.write_u32(work + ctx::FRAME_LIST, out)?;
    let arg = work + ctx::CALLBACK_ARG;
    ctx.memory.write_u32(arg, 1)?;
    ctx.memory.write_u32(arg + 4, work + ctx::FRAME_LIST)?;
    ctx.memory.write_u32(arg + 8, ctx.memory.read_u32(work + ctx::USER_MEMORY)?)?;
    Ok(())
}

/// Takes the bitstream set last and hands one frame to the output callback, which runs
/// before this call returns (the title's thread calls it, as on the console).
#[hle_export(lib = "h264", name = "H264DECExecute")]
fn h264_execute(ctx: &mut HleContext<'_>, work: u32, image: u32) -> Result<(), HleError> {
    let data = ctx.memory.read_u32(work + ctx::BITSTREAM)?;
    let length = ctx.memory.read_u32(work + ctx::LENGTH)?;
    let bytes = guest_bytes(ctx, data, length);
    if ctx.memory.read_u32(work + ctx::RECOVERING)? != 0 {
        if !nal::first_slice_is_idr(&bytes) {
            ctx.cpu.gpr[3] = EXECUTE_RECOVERING;
            return Ok(());
        }
        ctx.memory.write_u32(work + ctx::RECOVERING, 0)?;
    }
    if let Some(sps) = nal::find_sps(&bytes) {
        ctx.memory.write_u32(work + ctx::WIDTH, sps.width)?;
        ctx.memory.write_u32(work + ctx::HEIGHT, sps.height)?;
        for (side, value) in sps.crop.unwrap_or_default().into_iter().enumerate() {
            ctx.memory.write_u32(work + ctx::CROP + side as u32 * 4, value)?;
        }
    }
    write_frame(ctx, work, image)?;
    ctx.cpu.gpr[3] = EXECUTE_FRAME;
    let callback = ctx.memory.read_u32(work + ctx::OUTPUT_FUNC)?;
    if callback == 0 {
        return Ok(());
    }
    let stub = ctx.os.registry.function_stub(INTERNAL_LIBRARY, H264_OUTPUT_RETURN.name);
    ctx.os.registry.commit(ctx.memory)?;
    let me = ctx.os.coreinit.threads.current;
    ctx.os.h264.returns.insert(me, ctx.cpu.lr);
    ctx.cpu.pc = callback;
    ctx.cpu.lr = stub;
    ctx.cpu.gpr[3] = work + ctx::CALLBACK_ARG;
    Ok(())
}

/// The output callback returned: `H264DECExecute` returns to its caller.
#[hle_export(lib = "shocora", name = "__shocora_H264OutputReturn")]
fn h264_output_return(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    let me = ctx.os.coreinit.threads.current;
    let Some(lr) = ctx.os.h264.returns.remove(&me) else {
        return Err(HleError::Fatal("H.264 output callback returned without a decode".into()));
    };
    ctx.cpu.lr = lr;
    ctx.cpu.gpr[3] = EXECUTE_FRAME;
    Ok(())
}

pub const EXPORTS: &[HleExport] = &[
    H264_MEMORY_REQUIREMENT,
    H264_CHECK_MEM_SEGMENTATION,
    H264_FIND_DECSTARTPOINT,
    H264_FIND_IDRPOINT,
    H264_GET_IMAGE_SIZE,
    H264_CHECK_DECUNIT_LENGTH,
    H264_INIT_PARAM,
    H264_SET_PARAM,
    H264_SET_PARAM_OUTPUT,
    H264_SET_PARAM_PER_FRAME,
    H264_SET_PARAM_USER,
    H264_OPEN,
    H264_CLOSE,
    H264_BEGIN,
    H264_END,
    H264_SET_BITSTREAM,
    H264_EXECUTE,
    H264_OUTPUT_RETURN,
];

#[cfg(test)]
mod tests;
