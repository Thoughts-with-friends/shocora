//! Moving pictures for the guest: H.264 or HEVC through the system's video codecs, which
//! the graphics card runs where it can.
//!
//! A picture travels as NV12 (a full-size brightness plane, then a half-size plane of
//! interleaved colour pairs), which is what video encoders take and decoders give.
use std::{io, time::Duration};

#[cfg(windows)]
mod mf;

/// How a picture is compressed on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec {
    /// Each picture on its own: works everywhere, but large and slow at high resolutions.
    Jpeg = 0,
    H264 = 1,
    /// For pictures wider than H.264 encoders take (above 4096).
    Hevc = 2,
}

impl Codec {
    pub const ALL: [Self; 3] = [Self::Jpeg, Self::H264, Self::Hevc];

    #[inline]
    pub const fn bit(self) -> u8 {
        1 << self as u8
    }

    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Jpeg),
            1 => Some(Self::H264),
            2 => Some(Self::Hevc),
            _ => None,
        }
    }

    /// Widest picture its encoders usually take.
    pub const fn max_width(self) -> u32 {
        match self {
            Self::Jpeg | Self::H264 => 4096,
            Self::Hevc => 8192,
        }
    }
}

/// A picture in NV12, rows packed without padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nv12 {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Nv12 {
    /// Bytes of a picture of this size.
    pub const fn size(width: u32, height: u32) -> usize {
        (width as usize * height as usize) * 3 / 2
    }
}

/// Bits per second for a stream: plenty for a local network, so that fast scenes stay sharp.
pub fn bitrate(width: u32, height: u32, fps: u32) -> u32 {
    let bits = f64::from(width) * f64::from(height) * f64::from(fps) * 0.15;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let bits = bits.clamp(2e6, 400e6) as u32;
    bits
}

/// Codecs this machine can decode, as [`Codec::bit`]s. JPEG always works.
pub fn decodable() -> u8 {
    #[cfg(windows)]
    {
        Codec::Jpeg.bit() | mf::decodable()
    }
    #[cfg(not(windows))]
    {
        Codec::Jpeg.bit()
    }
}

/// Compresses a stream of pictures of one size.
pub struct VideoEncoder {
    #[cfg(windows)]
    inner: mf::Encoder,
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
}

impl std::fmt::Debug for VideoEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoEncoder")
            .field("codec", &self.codec)
            .field("size", &(self.width, self.height))
            .finish_non_exhaustive()
    }
}

impl VideoEncoder {
    /// An encoder for `codec`, which must not be [`Codec::Jpeg`].
    ///
    /// # Errors
    /// Fails if the system has no such encoder for this size.
    pub fn new(codec: Codec, width: u32, height: u32, fps: u32) -> io::Result<Self> {
        #[cfg(windows)]
        {
            let inner = mf::Encoder::new(codec, width, height, fps, bitrate(width, height, fps))?;
            Ok(Self { inner, codec, width, height })
        }
        #[cfg(not(windows))]
        {
            let _ = fps;
            Err(io::Error::new(io::ErrorKind::Unsupported, format!("no {codec:?} encoder")))
        }
    }

    /// Compresses `picture`; `keyframe` makes it decodable without the ones before. The result
    /// may be empty while the encoder holds on to the picture.
    ///
    /// # Errors
    /// Fails if the encoder does.
    pub fn encode(
        &mut self,
        picture: &Nv12,
        time: Duration,
        keyframe: bool,
    ) -> io::Result<Vec<u8>> {
        if picture.width != self.width || picture.height != self.height {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "picture size changed"));
        }
        #[cfg(windows)]
        {
            self.inner.encode(&picture.data, time, keyframe)
        }
        #[cfg(not(windows))]
        {
            let _ = (time, keyframe);
            Err(io::Error::new(io::ErrorKind::Unsupported, "no encoder"))
        }
    }
}

/// Decompresses a stream of pictures.
pub struct VideoDecoder {
    #[cfg(windows)]
    inner: mf::Decoder,
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
}

impl std::fmt::Debug for VideoDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoDecoder").field("codec", &self.codec).finish_non_exhaustive()
    }
}

impl VideoDecoder {
    /// # Errors
    /// Fails if the system has no decoder for `codec`.
    pub fn new(codec: Codec, width: u32, height: u32) -> io::Result<Self> {
        #[cfg(windows)]
        {
            Ok(Self { inner: mf::Decoder::new(codec, width, height)?, codec, width, height })
        }
        #[cfg(not(windows))]
        {
            let _ = (width, height);
            Err(io::Error::new(io::ErrorKind::Unsupported, format!("no {codec:?} decoder")))
        }
    }

    /// The picture `data` completes, if any.
    ///
    /// # Errors
    /// Fails if the data cannot be decoded: the next keyframe starts over.
    pub fn decode(&mut self, data: &[u8], time: Duration) -> io::Result<Option<Nv12>> {
        #[cfg(windows)]
        {
            self.inner.decode(data, time, self.width, self.height)
        }
        #[cfg(not(windows))]
        {
            let _ = (data, time);
            Err(io::Error::new(io::ErrorKind::Unsupported, "no decoder"))
        }
    }
}

/// BT.709 limited range, as video decoders expect HD pictures to be.
mod bt709 {
    pub const KR: f32 = 0.2126;
    pub const KB: f32 = 0.0722;
}

/// Converts RGBA into NV12. Width and height must be even.
pub fn rgba_to_nv12(width: u32, height: u32, rgba: &[u8]) -> Nv12 {
    use bt709::{KB, KR};
    let (w, h) = (width as usize, height as usize);
    let kg = 1.0 - KR - KB;
    let mut data = vec![0; Nv12::size(width, height)];
    let luma = |p: &[u8]| {
        let y = f32::from(p[0]).mul_add(KR, f32::from(p[1]).mul_add(kg, f32::from(p[2]) * KB));
        y.mul_add(219.0 / 255.0, 16.0)
    };
    for (row, out) in data[..w * h].chunks_exact_mut(w).enumerate() {
        for (x, value) in out.iter_mut().enumerate() {
            let p = &rgba[(row * w + x) * 4..];
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                *value = luma(p).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    let chroma = &mut data[w * h..];
    for y in 0..h / 2 {
        for x in 0..w / 2 {
            let mut sum = [0.0_f32; 3];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = &rgba[((y * 2 + dy) * w + x * 2 + dx) * 4..];
                for c in 0..3 {
                    sum[c] += f32::from(p[c]) / 4.0;
                }
            }
            let y_full = sum[0].mul_add(KR, sum[1].mul_add(kg, sum[2] * KB));
            let u = (sum[2] - y_full) / (2.0 * (1.0 - KB));
            let v = (sum[0] - y_full) / (2.0 * (1.0 - KR));
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                chroma[(y * w / 2 + x) * 2] =
                    u.mul_add(224.0 / 255.0, 128.0).round().clamp(0.0, 255.0) as u8;
                chroma[(y * w / 2 + x) * 2 + 1] =
                    v.mul_add(224.0 / 255.0, 128.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Nv12 { width, height, data }
}

/// Converts NV12 into opaque RGBA.
pub fn nv12_to_rgba(picture: &Nv12) -> Vec<u8> {
    use bt709::{KB, KR};
    let (w, h) = (picture.width as usize, picture.height as usize);
    let (luma, chroma) = picture.data.split_at(w * h);
    let kg = 1.0 - KR - KB;
    let mut rgba = vec![255; w * h * 4];
    for (row, out) in rgba.chunks_exact_mut(w * 4).enumerate() {
        let pairs = &chroma[(row / 2) * w..];
        for (x, pixel) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let y = (f32::from(luma[row * w + x]) - 16.0) * (255.0 / 219.0);
            let u = (f32::from(pairs[(x / 2) * 2]) - 128.0) * (255.0 / 224.0);
            let v = (f32::from(pairs[(x / 2) * 2 + 1]) - 128.0) * (255.0 / 224.0);
            let r = (2.0 * (1.0 - KR)).mul_add(v, y);
            let b = (2.0 * (1.0 - KB)).mul_add(u, y);
            let g = KB.mul_add(-b, KR.mul_add(-r, y)) / kg;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            for (slot, value) in pixel.iter_mut().zip([r, g, b]) {
                *slot = value.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(width: u32, height: u32) -> Vec<u8> {
        (0..height)
            .flat_map(|y| (0..width).flat_map(move |x| [(x * 4) as u8, (y * 4) as u8, 128, 255]))
            .collect()
    }

    #[test]
    fn colours_survive_nv12() {
        let rgba = gradient(16, 8);
        let back = nv12_to_rgba(&rgba_to_nv12(16, 8, &rgba));
        let worst = rgba.iter().zip(&back).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        assert!(worst <= 12, "{worst}");
    }

    /// Intent: on Windows, a picture comes back through the system's H.264 codecs at the
    /// same size and roughly the same colour, as the guest sees it.
    #[cfg(windows)]
    #[test]
    fn h264_round_trip() {
        let (width, height) = (320, 240);
        let rgba = gradient(width, height);
        let picture = rgba_to_nv12(width, height, &rgba);
        let mut encoder = VideoEncoder::new(Codec::H264, width, height, 60).unwrap();
        let mut decoder = VideoDecoder::new(Codec::H264, width, height).unwrap();
        let mut decoded = None;
        for frame in 0..30_u64 {
            let time = Duration::from_millis(frame * 16);
            let data = encoder.encode(&picture, time, frame == 0).unwrap();
            if !data.is_empty() {
                if let Some(picture) = decoder.decode(&data, time).unwrap() {
                    decoded = Some(picture);
                }
            }
        }
        let decoded = decoded.expect("no picture came out");
        assert_eq!((decoded.width, decoded.height), (width, height));
        let back = nv12_to_rgba(&decoded);
        let centre = ((height / 2 * width + width / 2) * 4) as usize;
        assert!(back[centre].abs_diff(rgba[centre]) < 20, "{} {}", back[centre], rgba[centre]);
    }
}
