//! `zlib125.rpl`: zlib 1.2.5, backed by `flate2`.
//!
//! The guest `z_stream` keeps its pointers and counters in guest memory;
//! the codec state lives host-side, keyed by the stream's address.
#![allow(clippy::cast_possible_truncation, clippy::too_many_arguments)]

use std::collections::HashMap;

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};
use wiiu_hle_macros::hle_export;
use wiiu_memory::GuestPtr;

use crate::hle::{HleContext, HleError, HleExport};

mod offset {
    pub const NEXT_IN: u32 = 0x00;
    pub const AVAIL_IN: u32 = 0x04;
    pub const TOTAL_IN: u32 = 0x08;
    pub const NEXT_OUT: u32 = 0x0C;
    pub const AVAIL_OUT: u32 = 0x10;
    pub const TOTAL_OUT: u32 = 0x14;
    pub const MSG: u32 = 0x18;
    pub const ADLER: u32 = 0x30;
}

const Z_OK: i32 = 0;
const Z_STREAM_END: i32 = 1;
const Z_STREAM_ERROR: i32 = -2;
const Z_DATA_ERROR: i32 = -3;
const Z_BUF_ERROR: i32 = -5;
const Z_FINISH: u32 = 4;

/// Size of the gzip trailer (CRC-32 and input size).
const GZIP_TRAILER: u32 = 8;

/// Host codec state of one `z_stream`.
pub enum Stream {
    Inflate {
        codec: Decompress,
        /// As passed to `inflateInit2_`, so resets keep the container format.
        window_bits: i32,
        /// `windowBits` asked for gzip (or auto-detect) and the header is not consumed yet.
        gzip_pending: bool,
        auto_detect: bool,
        /// The container is gzip: the deflate data is followed by a trailer.
        is_gzip: bool,
        /// Trailer bytes still to skip once the deflate data ended.
        trailer_left: u32,
        /// The whole stream was consumed; further calls change nothing.
        finished: bool,
    },
    Deflate(Compress),
}

impl std::fmt::Debug for Stream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Inflate { .. } => "Inflate",
            Self::Deflate(_) => "Deflate",
        })
    }
}

/// All live streams.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct ZlibState {
    /// Host codecs: a stream open while a state is saved does not come back.
    #[serde(skip)]
    pub streams: HashMap<u32, Stream>,
}

/// Length of a gzip header at the start of `data`, if complete.
fn gzip_header_len(data: &[u8]) -> Option<usize> {
    if data.len() < 10 || data[0] != 0x1F || data[1] != 0x8B {
        return None;
    }
    let flags = data[3];
    let mut at = 10;
    if flags & 0x04 != 0 {
        let extra = usize::from(u16::from_le_bytes([*data.get(at)?, *data.get(at + 1)?]));
        at += 2 + extra;
    }
    for bit in [0x08, 0x10] {
        if flags & bit != 0 {
            at += data.get(at..)?.iter().position(|&b| b == 0)? + 1;
        }
    }
    if flags & 0x02 != 0 {
        at += 2;
    }
    (at <= data.len()).then_some(at)
}

fn new_inflate(window_bits: i32) -> Stream {
    let gzip = window_bits >= 16;
    let raw = window_bits < 0;
    Stream::Inflate {
        codec: Decompress::new(!raw && !gzip),
        window_bits,
        gzip_pending: gzip,
        auto_detect: window_bits >= 32,
        is_gzip: false,
        trailer_left: 0,
        finished: false,
    }
}

/// Reads `(next_in, avail_in, next_out, avail_out)`.
fn buffers(ctx: &HleContext<'_>, stream: u32) -> Result<(u32, u32, u32, u32), HleError> {
    let r = |o| ctx.memory.read_u32(stream + o);
    Ok((r(offset::NEXT_IN)?, r(offset::AVAIL_IN)?, r(offset::NEXT_OUT)?, r(offset::AVAIL_OUT)?))
}

/// Advances the guest stream by `consumed` input and `produced` output bytes.
fn advance(
    ctx: &mut HleContext<'_>,
    stream: u32,
    consumed: u32,
    produced: u32,
) -> Result<(), HleError> {
    for (field, delta, add) in [
        (offset::NEXT_IN, consumed, true),
        (offset::AVAIL_IN, consumed, false),
        (offset::TOTAL_IN, consumed, true),
        (offset::NEXT_OUT, produced, true),
        (offset::AVAIL_OUT, produced, false),
        (offset::TOTAL_OUT, produced, true),
    ] {
        let value = ctx.memory.read_u32(stream + field)?;
        let updated = if add { value.wrapping_add(delta) } else { value.wrapping_sub(delta) };
        ctx.memory.write_u32(stream + field, updated)?;
    }
    Ok(())
}

fn reset_counters(ctx: &mut HleContext<'_>, stream: u32) -> Result<(), HleError> {
    for field in [offset::TOTAL_IN, offset::TOTAL_OUT, offset::MSG, offset::ADLER] {
        ctx.memory.write_u32(stream + field, 0)?;
    }
    Ok(())
}

#[hle_export(lib = "zlib125", name = "inflateInit2_")]
fn inflate_init2(
    ctx: &mut HleContext<'_>,
    stream: u32,
    window_bits: i32,
    _version: u32,
    _size: u32,
) -> Result<i32, HleError> {
    reset_counters(ctx, stream)?;
    ctx.os.zlib.streams.insert(stream, new_inflate(window_bits));
    Ok(Z_OK)
}

#[hle_export(lib = "zlib125", name = "inflateInit_")]
fn inflate_init(
    ctx: &mut HleContext<'_>,
    stream: u32,
    _version: u32,
    _size: u32,
) -> Result<i32, HleError> {
    reset_counters(ctx, stream)?;
    ctx.os.zlib.streams.insert(stream, new_inflate(15));
    Ok(Z_OK)
}

#[hle_export(lib = "zlib125", name = "inflateReset")]
fn inflate_reset(ctx: &mut HleContext<'_>, stream: u32) -> Result<i32, HleError> {
    reset_counters(ctx, stream)?;
    if let Some(Stream::Inflate { window_bits, .. }) = ctx.os.zlib.streams.get(&stream) {
        let window_bits = *window_bits;
        ctx.os.zlib.streams.insert(stream, new_inflate(window_bits));
    }
    Ok(Z_OK)
}

#[hle_export(lib = "zlib125", name = "inflateReset2")]
fn inflate_reset2(
    ctx: &mut HleContext<'_>,
    stream: u32,
    window_bits: i32,
) -> Result<i32, HleError> {
    reset_counters(ctx, stream)?;
    ctx.os.zlib.streams.insert(stream, new_inflate(window_bits));
    Ok(Z_OK)
}

/// Real zlib turns these calls down instead of touching memory, and titles check for it.
const fn missing_buffer(next_in: u32, avail_in: u32, next_out: u32) -> bool {
    next_out == 0 || (next_in == 0 && avail_in != 0)
}

#[hle_export(lib = "zlib125", name = "inflate")]
fn inflate(ctx: &mut HleContext<'_>, stream: u32, flush: u32) -> Result<i32, HleError> {
    let (next_in, avail_in, next_out, avail_out) = buffers(ctx, stream)?;
    if missing_buffer(next_in, avail_in, next_out) {
        return Ok(Z_STREAM_ERROR);
    }
    let mut input = vec![0; avail_in as usize];
    ctx.memory.read_bytes(next_in, &mut input)?;
    let Some(Stream::Inflate {
        codec,
        gzip_pending,
        auto_detect,
        is_gzip,
        trailer_left,
        finished,
        ..
    }) = ctx.os.zlib.streams.get_mut(&stream)
    else {
        return Ok(Z_STREAM_ERROR);
    };
    if *finished {
        return Ok(Z_STREAM_END);
    }

    let mut skipped = 0;
    if *gzip_pending || (*auto_detect && codec.total_in() == 0) {
        *is_gzip = input.starts_with(&[0x1F, 0x8B]);
        if *is_gzip {
            let Some(len) = gzip_header_len(&input) else { return Ok(Z_BUF_ERROR) };
            skipped = len;
            *codec = Decompress::new(false);
        } else if *auto_detect {
            *codec = Decompress::new(true);
        }
        *gzip_pending = false;
        *auto_detect = false;
    }

    let mut output = Vec::with_capacity(avail_out as usize);
    let before_in = codec.total_in();
    let mode = if flush == Z_FINISH { FlushDecompress::Finish } else { FlushDecompress::None };
    let status = if *trailer_left > 0 {
        Ok(Status::StreamEnd)
    } else {
        codec.decompress_vec(&input[skipped..], &mut output, mode).inspect(|status| {
            if *status == Status::StreamEnd && *is_gzip {
                *trailer_left = GZIP_TRAILER;
            }
        })
    };
    let mut consumed = (codec.total_in() - before_in) as u32 + skipped as u32;
    let ended = matches!(status, Ok(Status::StreamEnd));
    // zlib only reports the end once the trailer went by, which guests rely on
    // when they feed input until `avail_in` reaches zero.
    let skip = (*trailer_left).min(avail_in - consumed);
    consumed += skip;
    *trailer_left -= skip;
    let done = ended && *trailer_left == 0;
    *finished = done;
    let produced = output.len() as u32;
    ctx.memory.write_bytes(next_out, &output)?;
    advance(ctx, stream, consumed, produced)?;
    Ok(match status {
        Ok(_) if done => Z_STREAM_END,
        Ok(_) if consumed == 0 && produced == 0 => Z_BUF_ERROR,
        Ok(_) => Z_OK,
        Err(error) => {
            log::warn!("inflate: {error}");
            Z_DATA_ERROR
        }
    })
}

#[hle_export(lib = "zlib125", name = "inflateEnd")]
fn inflate_end(ctx: &mut HleContext<'_>, stream: u32) -> i32 {
    ctx.os.zlib.streams.remove(&stream);
    Z_OK
}

fn level(level: i32) -> Compression {
    if level < 0 {
        Compression::default()
    } else {
        Compression::new(level.clamp(0, 9).cast_unsigned())
    }
}

#[hle_export(lib = "zlib125", name = "deflateInit_")]
fn deflate_init(
    ctx: &mut HleContext<'_>,
    stream: u32,
    compression: i32,
    _version: u32,
    _size: u32,
) -> Result<i32, HleError> {
    reset_counters(ctx, stream)?;
    ctx.os.zlib.streams.insert(stream, Stream::Deflate(Compress::new(level(compression), true)));
    Ok(Z_OK)
}

#[hle_export(lib = "zlib125", name = "deflateInit2_")]
fn deflate_init2(
    ctx: &mut HleContext<'_>,
    stream: u32,
    compression: i32,
    _method: i32,
    window_bits: i32,
    _mem_level: i32,
    _strategy: i32,
    _version: u32,
    _size: u32,
) -> Result<i32, HleError> {
    reset_counters(ctx, stream)?;
    ctx.os
        .zlib
        .streams
        .insert(stream, Stream::Deflate(Compress::new(level(compression), window_bits > 0)));
    Ok(Z_OK)
}

#[hle_export(lib = "zlib125", name = "deflate")]
fn deflate(ctx: &mut HleContext<'_>, stream: u32, flush: u32) -> Result<i32, HleError> {
    let (next_in, avail_in, next_out, avail_out) = buffers(ctx, stream)?;
    if missing_buffer(next_in, avail_in, next_out) {
        return Ok(Z_STREAM_ERROR);
    }
    let mut input = vec![0; avail_in as usize];
    ctx.memory.read_bytes(next_in, &mut input)?;
    let Some(Stream::Deflate(codec)) = ctx.os.zlib.streams.get_mut(&stream) else {
        return Ok(Z_STREAM_ERROR);
    };
    let mut output = vec![0; avail_out as usize];
    let (before_in, before_out) = (codec.total_in(), codec.total_out());
    let mode = match flush {
        Z_FINISH => FlushCompress::Finish,
        2 => FlushCompress::Sync,
        3 => FlushCompress::Full,
        _ => FlushCompress::None,
    };
    let status = codec.compress(&input, &mut output, mode);
    let consumed = (codec.total_in() - before_in) as u32;
    let produced = (codec.total_out() - before_out) as u32;
    ctx.memory.write_bytes(next_out, &output[..produced as usize])?;
    advance(ctx, stream, consumed, produced)?;
    Ok(match status {
        Ok(Status::StreamEnd) => Z_STREAM_END,
        Ok(_) => Z_OK,
        Err(_) => Z_STREAM_ERROR,
    })
}

#[hle_export(lib = "zlib125", name = "deflateEnd")]
fn deflate_end(ctx: &mut HleContext<'_>, stream: u32) -> i32 {
    ctx.os.zlib.streams.remove(&stream);
    Z_OK
}

const fn bound(len: u32) -> u32 {
    len + (len >> 12) + (len >> 14) + (len >> 25) + 13
}

#[hle_export(lib = "zlib125", name = "deflateBound")]
fn deflate_bound(_ctx: &mut HleContext<'_>, _stream: u32, len: u32) -> u32 {
    bound(len)
}

#[hle_export(lib = "zlib125", name = "compressBound")]
fn compress_bound(_ctx: &mut HleContext<'_>, len: u32) -> u32 {
    bound(len)
}

#[hle_export(lib = "zlib125", name = "uncompress")]
fn uncompress(
    ctx: &mut HleContext<'_>,
    dst: u32,
    dst_len: GuestPtr<u32>,
    src: u32,
    src_len: u32,
) -> Result<i32, HleError> {
    let capacity = dst_len.read(ctx.memory)?;
    let mut input = vec![0; src_len as usize];
    ctx.memory.read_bytes(src, &mut input)?;
    let mut output = vec![0; capacity as usize];
    let mut codec = Decompress::new(true);
    let status = codec.decompress(&input, &mut output, FlushDecompress::Finish);
    let produced = codec.total_out() as u32;
    ctx.memory.write_bytes(dst, &output[..produced as usize])?;
    dst_len.write(ctx.memory, produced)?;
    Ok(match status {
        Ok(Status::StreamEnd) => Z_OK,
        Ok(_) => Z_BUF_ERROR,
        Err(_) => Z_DATA_ERROR,
    })
}

#[hle_export(lib = "zlib125", name = "compress")]
fn compress(
    ctx: &mut HleContext<'_>,
    dst: u32,
    dst_len: GuestPtr<u32>,
    src: u32,
    src_len: u32,
) -> Result<i32, HleError> {
    let capacity = dst_len.read(ctx.memory)?;
    let mut input = vec![0; src_len as usize];
    ctx.memory.read_bytes(src, &mut input)?;
    let mut output = vec![0; capacity as usize];
    let mut codec = Compress::new(Compression::default(), true);
    let status = codec.compress(&input, &mut output, FlushCompress::Finish);
    let produced = codec.total_out() as u32;
    ctx.memory.write_bytes(dst, &output[..produced as usize])?;
    dst_len.write(ctx.memory, produced)?;
    Ok(if matches!(status, Ok(Status::StreamEnd)) { Z_OK } else { Z_BUF_ERROR })
}

#[hle_export(lib = "zlib125", name = "crc32")]
fn crc32(ctx: &mut HleContext<'_>, crc: u32, buffer: u32, len: u32) -> Result<u32, HleError> {
    if buffer == 0 {
        return Ok(0);
    }
    let mut data = vec![0; len as usize];
    ctx.memory.read_bytes(buffer, &mut data)?;
    Ok(PrefixCrc(crc).extend(&data))
}

/// CRC-32 continued from an arbitrary previous value (zlib semantics).
struct PrefixCrc(u32);

impl PrefixCrc {
    fn extend(&mut self, data: &[u8]) -> u32 {
        let mut crc = !self.0;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
        self.0 = !crc;
        self.0
    }
}

pub const EXPORTS: &[HleExport] = &[
    INFLATE_INIT2,
    INFLATE_INIT,
    INFLATE_RESET,
    INFLATE_RESET2,
    INFLATE,
    INFLATE_END,
    DEFLATE_INIT,
    DEFLATE_INIT2,
    DEFLATE,
    DEFLATE_END,
    DEFLATE_BOUND,
    COMPRESS_BOUND,
    UNCOMPRESS,
    COMPRESS,
    CRC32,
];

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn missing_buffers_are_an_error_not_a_crash() {
        let mut env = TestEnv::new();
        let stream = SCRATCH;
        env.call("zlib125", "inflateInit_", &[stream, 0, 0x38]);
        // No input although some is promised, as when a title's allocation failed.
        for (field, value) in
            [(0x00, 0), (0x04, 0x480_2000), (0x0C, SCRATCH + 0x400), (0x10, 0x100)]
        {
            env.memory.write_u32(stream + field, value).unwrap();
        }
        assert_eq!(
            env.call("zlib125", "inflate", &[stream, Z_FINISH]),
            Z_STREAM_ERROR.cast_unsigned()
        );
    }

    #[test]
    fn inflate_streams_into_guest_memory() {
        let mut env = TestEnv::new();
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"hello hello hello shocora").unwrap();
        let packed = encoder.finish().unwrap();
        let (stream, input, output) = (SCRATCH, SCRATCH + 0x100, SCRATCH + 0x400);
        env.memory.write_bytes(input, &packed).unwrap();
        env.call("zlib125", "inflateInit_", &[stream, 0, 0x38]);
        for (field, value) in
            [(0x00, input), (0x04, packed.len() as u32), (0x0C, output), (0x10, 0x100)]
        {
            env.memory.write_u32(stream + field, value).unwrap();
        }
        assert_eq!(
            env.call("zlib125", "inflate", &[stream, Z_FINISH]),
            Z_STREAM_END.cast_unsigned()
        );
        assert_eq!(env.memory.read_u32(stream + 0x14).unwrap(), 25);
        assert_eq!(env.memory.read_c_string(output, 25).unwrap(), "hello hello hello shocora");
        env.memory.write_bytes(SCRATCH + 0x800, b"123456789").unwrap();
        assert_eq!(env.call("zlib125", "crc32", &[0, SCRATCH + 0x800, 9]), 0xCBF4_3926);
        assert_eq!(env.call("zlib125", "crc32", &[0xCBF4_3926, SCRATCH + 0x800, 0]), 0xCBF4_3926);
    }

    #[test]
    fn gzip_trailer_is_consumed_even_across_calls() {
        let mut env = TestEnv::new();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"gzip data for shocora").unwrap();
        let packed = encoder.finish().unwrap();
        let (stream, input, output) = (SCRATCH, SCRATCH + 0x100, SCRATCH + 0x400);
        env.memory.write_bytes(input, &packed).unwrap();
        env.call("zlib125", "inflateInit2_", &[stream, 31, 0, 0x38]);
        // Withhold the last three bytes: the end is only reported once they arrive.
        let first = packed.len() as u32 - 3;
        for (field, value) in [(0x00, input), (0x04, first), (0x0C, output), (0x10, 0x100)] {
            env.memory.write_u32(stream + field, value).unwrap();
        }
        assert_eq!(env.call("zlib125", "inflate", &[stream, 0]), Z_OK.cast_unsigned());
        assert_eq!(env.memory.read_u32(stream + 0x04).unwrap(), 0);
        env.memory.write_u32(stream + 0x04, 3).unwrap();
        assert_eq!(env.call("zlib125", "inflate", &[stream, 0]), Z_STREAM_END.cast_unsigned());
        assert_eq!(env.memory.read_u32(stream + 0x04).unwrap(), 0);
        assert_eq!(env.memory.read_c_string(output, 21).unwrap(), "gzip data for shocora");
    }
}
