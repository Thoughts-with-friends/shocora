//! Fetch shaders: the guest's description of its vertex attributes.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                                        ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ vertex layout      │   │ shader/fetch.rs          │   │ attribute formats  │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ fetch shaders            │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Understands how the game describes the layout of its vertex data. Terms such as HLE, JIT or
//! shader are explained in `docs/guide/wiiu_term.md`.

/// Attribute formats (`E_HWFMT`) the vertex pulling code decodes.
pub mod format {
    pub const F8: u32 = 0x01;
    pub const F16: u32 = 0x05;
    pub const F16_FLOAT: u32 = 0x06;
    pub const F8_8: u32 = 0x07;
    pub const F32: u32 = 0x0D;
    pub const F32_FLOAT: u32 = 0x0E;
    pub const F16_16: u32 = 0x0F;
    pub const F16_16_FLOAT: u32 = 0x10;
    pub const F10_11_11_FLOAT: u32 = 0x16;
    pub const F2_10_10_10: u32 = 0x19;
    pub const F8_8_8_8: u32 = 0x1A;
    pub const F32_32: u32 = 0x1D;
    pub const F32_32_FLOAT: u32 = 0x1E;
    pub const F16_16_16_16: u32 = 0x1F;
    pub const F16_16_16_16_FLOAT: u32 = 0x20;
    pub const F32_32_32_32: u32 = 0x22;
    pub const F32_32_32_32_FLOAT: u32 = 0x23;
    pub const F32_32_32: u32 = 0x2F;
    pub const F32_32_32_FLOAT: u32 = 0x30;
}

/// One `VTX_INST_SEMANTIC` of a fetch shader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Attribute {
    pub semantic: u32,
    /// Attribute buffer index (0..16).
    pub buffer: u32,
    pub offset: u32,
    pub format: u32,
    /// `NUM_FORMAT_ALL`: 0 normalized, 1 integer, 2 scaled.
    pub num_format: u32,
    pub signed: bool,
    /// 0: no swap, 1: bytes in 16-bit units, 2: bytes in 32-bit units.
    pub endian: u32,
    /// 0..=3: components, 4: 0, 5: 1, 7: masked.
    pub dst_sel: [u8; 4],
    /// 0: per vertex; 1/2: instance / divisor 0/1; 3: per instance.
    pub rate: u32,
}

const CF_VTX: u32 = 0x02;
const CF_VTX_TC: u32 = 0x03;
const CF_RETURN: u32 = 0x14;
/// `VTX_INST_SEMANTIC`.
const VTX_SEMANTIC: u32 = 1;
/// First attribute buffer id (`BUFFER_ID` field).
const ATTRIBUTE_BUFFER_BASE: u32 = 0xA0;

fn word(code: &[u8], index: usize) -> Option<u32> {
    let bytes = code.get(index * 4..index * 4 + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Parses a fetch shader program (little-endian microcode).
pub fn parse_fetch_shader(code: &[u8]) -> Vec<Attribute> {
    let mut attributes = Vec::new();
    let mut cf = 0;
    while let (Some(word0), Some(word1)) = (word(code, cf), word(code, cf + 1)) {
        cf += 2;
        let kind = (word1 >> 23) & 0x7F;
        if kind == CF_RETURN || kind == 0 && word0 == 0 && word1 == 0 {
            break;
        }
        if kind != CF_VTX && kind != CF_VTX_TC {
            continue;
        }
        let count = (((word1 >> 10) & 7) | ((word1 >> 16) & 8)) + 1;
        let start = (word0 as usize) * 2;
        for n in 0..count as usize {
            let first = start + n * 4;
            let (Some(v0), Some(v1), Some(v2)) =
                (word(code, first), word(code, first + 1), word(code, first + 2))
            else {
                break;
            };
            if v0 & 0x1F != VTX_SEMANTIC {
                continue;
            }
            let dst_sel = [0, 1, 2, 3].map(|c: u32| ((v1 >> (9 + c * 3)) & 7) as u8);
            let fetch_type = (v0 >> 5) & 3;
            let src_sel = (v0 >> 24) & 3;
            attributes.push(Attribute {
                semantic: v1 & 0xFF,
                buffer: ((v0 >> 8) & 0xFF).wrapping_sub(ATTRIBUTE_BUFFER_BASE) & 0xF,
                offset: v2 & 0xFFFF,
                format: (v1 >> 22) & 0x3F,
                num_format: (v1 >> 28) & 3,
                signed: (v1 >> 30) & 1 != 0,
                endian: (v2 >> 16) & 3,
                dst_sel,
                rate: if fetch_type == 0 { 0 } else { src_sel.max(1) },
            });
        }
    }
    attributes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_one_attribute() {
        let (buffer, offset, fmt) = (1_u32, 8_u32, format::F32_32_32_FLOAT);
        let words = [
            2,
            CF_VTX_TC << 23,
            0,
            (CF_RETURN << 23) | (1 << 31),
            VTX_SEMANTIC | ((ATTRIBUTE_BUFFER_BASE + buffer) << 8),
            (3 | (fmt << 22) | (2 << 28)) | (1 << 12) | (2 << 15) | (5 << 18),
            offset | (2 << 16),
            0,
        ];
        let code: Vec<u8> = words.iter().flat_map(|w: &u32| w.to_le_bytes()).collect();
        let parsed = parse_fetch_shader(&code);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].semantic, 3);
        assert_eq!(parsed[0].buffer, 1);
        assert_eq!(parsed[0].offset, 8);
        assert_eq!(parsed[0].format, fmt);
        assert_eq!(parsed[0].endian, 2);
        assert_eq!(parsed[0].dst_sel, [0, 1, 2, 5]);
    }
}
