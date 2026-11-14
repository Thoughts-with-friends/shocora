//! H.264 bitstream helpers for `h264.rpl`: NAL unit scanning, RBSP extraction and the
//! sequence parameter set fields the decoder library reports.
//!
//! Nothing here decodes pictures; it only reads the headers titles ask about.

/// `nal_unit_type` values the library cares about.
pub mod nal_type {
    pub const NON_IDR_SLICE: u8 = 1;
    pub const IDR_SLICE: u8 = 5;
    pub const SPS: u8 = 7;
    pub const PPS: u8 = 8;
}

/// One NAL unit in a byte stream: `header` is the index of its header byte (just after
/// the `00 00 01` start code) and `end` is where the next start code begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NalUnit {
    pub header: usize,
    pub end: usize,
}

impl NalUnit {
    /// `nal_unit_type` of the unit.
    #[inline]
    pub fn kind(self, data: &[u8]) -> u8 {
        data[self.header] & 0x1F
    }
}

/// Index of the first `00 00 01` at or after `from`.
pub fn find_start_code(data: &[u8], from: usize) -> Option<usize> {
    let tail = data.get(from..)?;
    tail.windows(3).position(|w| w == [0, 0, 1]).map(|at| from + at)
}

/// Every NAL unit of an Annex B byte stream. A four-byte start code (`00 00 00 01`)
/// counts its leading zero to the unit before it.
pub fn nal_units(data: &[u8]) -> Vec<NalUnit> {
    let mut units = Vec::new();
    let mut at = find_start_code(data, 0);
    while let Some(start) = at {
        let header = start + 3;
        if header >= data.len() {
            break;
        }
        let next = find_start_code(data, header);
        let mut end = next.unwrap_or(data.len());
        if next.is_some() && end > header && data[end - 1] == 0 {
            end -= 1;
        }
        units.push(NalUnit { header, end });
        at = next;
    }
    units
}

/// The payload of a NAL unit without its emulation prevention bytes (`00 00 03`).
pub fn rbsp(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len());
    let mut zeros = 0;
    for &byte in payload {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        out.push(byte);
    }
    out
}

/// Big-endian bit reader with Exp-Golomb codes. Reads past the end yield zeros.
pub struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    #[inline]
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, bit: 0 }
    }

    pub fn bit(&mut self) -> u32 {
        let byte = self.data.get(self.bit / 8).copied().unwrap_or(0);
        let value = (byte >> (7 - self.bit % 8)) & 1;
        self.bit += 1;
        u32::from(value)
    }

    pub fn bits(&mut self, count: u32) -> u32 {
        (0..count).fold(0, |value, _| (value << 1) | self.bit())
    }

    /// `ue(v)`.
    pub fn ue(&mut self) -> u32 {
        let mut zeros = 0;
        while self.bit() == 0 {
            zeros += 1;
            if zeros > 31 {
                return u32::MAX;
            }
        }
        ((1_u64 << zeros) - 1 + u64::from(self.bits(zeros))).min(u64::from(u32::MAX)) as u32
    }

    /// `se(v)`.
    pub fn se(&mut self) -> i32 {
        let code = i64::from(self.ue());
        let magnitude = ((code + 1) / 2) as i32;
        if code % 2 == 1 { magnitude } else { -magnitude }
    }
}

/// The fields of a sequence parameter set the library reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sps {
    pub profile: u32,
    pub level: u32,
    /// Coded size in luma samples (whole macroblocks).
    pub width: u32,
    pub height: u32,
    /// Cropping in luma samples: left, right, top, bottom.
    pub crop: Option<[u32; 4]>,
}

fn skip_scaling_list(reader: &mut BitReader<'_>, size: usize) {
    let (mut last, mut next) = (8_i32, 8_i32);
    for _ in 0..size {
        if next != 0 {
            next = (last + reader.se() + 256) % 256;
        }
        if next != 0 {
            last = next;
        }
    }
}

/// Parses the RBSP of a sequence parameter set (the bytes after the NAL header).
/// Returns `None` for profiles the Wii U decoder rejects (only 66, 77 and 100 exist).
pub fn parse_sps(rbsp: &[u8]) -> Option<Sps> {
    let mut r = BitReader::new(rbsp);
    let profile = r.bits(8);
    if !matches!(profile, 66 | 77 | 100) {
        return None;
    }
    r.bits(8);
    let level = r.bits(8);
    if r.ue() >= 32 {
        return None;
    }
    if profile == 100 {
        if r.ue() > 1 {
            return None;
        }
        r.ue();
        r.ue();
        r.bit();
        if r.bit() == 1 {
            for i in 0..8 {
                if r.bit() == 1 {
                    skip_scaling_list(&mut r, if i < 6 { 16 } else { 64 });
                }
            }
        }
    }
    if r.ue() > 12 {
        return None;
    }
    match r.ue() {
        0 => {
            if r.ue() > 12 {
                return None;
            }
        }
        1 => {
            r.bit();
            r.se();
            r.se();
            let cycle = r.ue();
            if cycle > 0xFF {
                return None;
            }
            for _ in 0..cycle {
                r.se();
            }
        }
        2 => {}
        _ => return None,
    }
    if r.ue() > 16 {
        return None;
    }
    r.bit();
    let width = 16 * r.ue().checked_add(1)?;
    let map_height = 16 * r.ue().checked_add(1)?;
    let frame_mbs_only = r.bit() == 1;
    if !frame_mbs_only {
        r.bit();
    }
    r.bit();
    let crop = (r.bit() == 1).then(|| {
        // 4:2:0: one crop unit is two luma samples across, and two (or four for
        // field coding) down.
        let unit_y = if frame_mbs_only { 2 } else { 4 };
        [r.ue() * 2, r.ue() * 2, r.ue() * unit_y, r.ue() * unit_y]
    });
    let height = if frame_mbs_only { map_height } else { map_height * 2 };
    Some(Sps { profile, level, width, height, crop })
}

/// The first SPS in an Annex B byte stream.
pub fn find_sps(data: &[u8]) -> Option<Sps> {
    nal_units(data)
        .into_iter()
        .find(|unit| unit.kind(data) == nal_type::SPS)
        .and_then(|unit| parse_sps(&rbsp(&data[unit.header + 1..unit.end])))
}

/// Whether the first slice of the stream is an IDR slice.
pub fn first_slice_is_idr(data: &[u8]) -> bool {
    for unit in nal_units(data) {
        if data[unit.header] & 0x80 != 0 {
            return false;
        }
        match unit.kind(data) {
            nal_type::NON_IDR_SLICE => return false,
            nal_type::IDR_SLICE => return true,
            _ => {}
        }
    }
    false
}

/// Offset of the first start code whose NAL type is in `kinds`, as Cemu and the console
/// report it: one byte before `00 00 01`, so a four-byte start code is included.
pub fn find_point(data: &[u8], kinds: &[u8]) -> Option<usize> {
    let mut at = 0;
    while let Some(start) = find_start_code(data, at) {
        let kind = *data.get(start + 3)? & 0x9F;
        if kinds.contains(&kind) {
            return Some(start.saturating_sub(1));
        }
        at = start + 1;
    }
    None
}

/// Length of the decoding unit at the start of `data`: everything up to the end of its
/// first slice. Assumes one slice per picture, as Cemu does.
pub fn decunit_length(data: &[u8]) -> Option<usize> {
    nal_units(data)
        .into_iter()
        .find(|unit| {
            matches!(unit.kind(data), nal_type::NON_IDR_SLICE | nal_type::IDR_SLICE)
                && data[unit.header] & 0x80 == 0
        })
        .map(|unit| unit.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes bits and Exp-Golomb codes, to build parameter sets by hand.
    #[derive(Default)]
    struct BitWriter(Vec<bool>);

    impl BitWriter {
        fn bits(&mut self, value: u32, count: u32) -> &mut Self {
            self.0.extend((0..count).rev().map(|i| value >> i & 1 == 1));
            self
        }

        fn ue(&mut self, value: u32) -> &mut Self {
            let code = value + 1;
            let len = 32 - code.leading_zeros();
            self.bits(0, len - 1).bits(code, len)
        }

        fn bytes(&self) -> Vec<u8> {
            self.0
                .chunks(8)
                .map(|c| c.iter().enumerate().fold(0, |b, (i, &v)| b | u8::from(v) << (7 - i)))
                .collect()
        }
    }

    /// A 1280 x 720 baseline SPS, level 3.1, cropping off.
    fn sps_720p() -> Vec<u8> {
        let mut w = BitWriter::default();
        w.bits(66, 8).bits(0xC0, 8).bits(31, 8);
        w.ue(0).ue(0).ue(2).ue(1).bits(0, 1).ue(79).ue(44);
        w.bits(1, 1).bits(1, 1).bits(0, 1).bits(0, 1).bits(1, 1);
        w.bytes()
    }

    fn stream(units: &[&[u8]]) -> Vec<u8> {
        units.iter().flat_map(|unit| [&[0_u8, 0, 0, 1][..], unit].concat()).collect()
    }

    /// Intent: Exp-Golomb codes decode as in the H.264 spec (9.1).
    /// Pass: `1` → 0, `010` → 1, `011` → 2, `00100` → 3; signed `011` → -1.
    #[test]
    fn reads_exp_golomb_codes() {
        let mut r = BitReader::new(&[0b1010_0110, 0b0100_0000]);
        assert_eq!([r.ue(), r.ue(), r.ue(), r.ue()], [0, 1, 2, 3]);
        let mut r = BitReader::new(&[0b0110_0000]);
        assert_eq!(r.se(), -1);
    }

    /// Intent: emulation prevention bytes are removed, other bytes stay.
    /// Pass: `00 00 03 01` becomes `00 00 01`.
    #[test]
    fn strips_emulation_prevention() {
        assert_eq!(rbsp(&[0, 0, 3, 1, 5]), [0, 0, 1, 5]);
    }

    /// Intent: the SPS parser reports the coded size titles allocate frames for.
    /// Pass: the 720p SPS gives 1280 x 720, profile 66, level 31.
    #[test]
    fn parses_a_720p_sps() {
        let sps = parse_sps(&sps_720p()).unwrap();
        assert_eq!((sps.profile, sps.level, sps.width, sps.height), (66, 31, 1280, 720));
        assert_eq!(sps.crop, None);
    }

    /// Intent: NAL scanning finds SPS, IDR points and decoding unit ends in a stream.
    /// Pass: offsets match the hand-built stream (SPS, PPS, IDR slice, non-IDR slice).
    #[test]
    fn scans_a_byte_stream() {
        let data = stream(&[&[0x67, 0x42], &[0x68, 0xCE], &[0x65, 0x88, 0x84], &[0x41, 0x9A]]);
        let units = nal_units(&data);
        let kinds: Vec<u8> = units.iter().map(|u| u.kind(&data)).collect();
        assert_eq!(kinds, [7, 8, 5, 1]);
        assert_eq!(find_point(&data, &[nal_type::SPS]), Some(0));
        assert_eq!(find_point(&data, &[nal_type::IDR_SLICE]), Some(12));
        assert!(first_slice_is_idr(&data));
        // The unit ends where the next start code's leading zero begins.
        assert_eq!(decunit_length(&data), Some(19));
    }
}
