//! PM4 packet framing. Pure: `&[u32] → Packet` stream.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                                        ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ raw words          │   │ command/pm4.rs           │   │ packets            │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ packet framing           │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Cuts the stream of 32-bit words into separate commands. Terms such as HLE, JIT or shader are
//! explained in `docs/guide/wiiu_term.md`.
use crate::GpuError;

/// One packet, borrowing its payload from the command buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packet<'a> {
    /// Write `values` to consecutive registers starting at `register` (dword index).
    Type0 { register: u32, values: &'a [u32] },
    /// Filler.
    Type2,
    /// Command `opcode` with its body.
    Type3 { opcode: u8, body: &'a [u32] },
}

/// Iterator over the packets of a command buffer.
#[derive(Debug, Clone)]
pub struct Packets<'a> {
    words: &'a [u32],
    index: usize,
}

/// Splits `words` into packets.
#[inline]
pub const fn parse(words: &[u32]) -> Packets<'_> {
    Packets { words, index: 0 }
}

impl<'a> Iterator for Packets<'a> {
    type Item = Result<Packet<'a>, GpuError>;

    fn next(&mut self) -> Option<Self::Item> {
        let start = self.index;
        let header = *self.words.get(start)?;
        let count = (((header >> 16) & 0x3FFF) + 1) as usize;
        let mut body = |len: usize| {
            let range = start + 1..start + 1 + len;
            let words = self.words.get(range).ok_or(GpuError::TruncatedPacket { index: start });
            self.index = start + 1 + len;
            words
        };
        let packet = match header >> 30 {
            0 => body(count).map(|values| Packet::Type0 { register: header & 0xFFFF, values }),
            #[allow(clippy::cast_possible_truncation)]
            3 => body(count).map(|body| Packet::Type3 { opcode: (header >> 8) as u8, body }),
            // Type 2 is a single-word filler; type 1 is unused and skipped the same way.
            _ => {
                self.index = start + 1;
                Ok(Packet::Type2)
            }
        };
        if packet.is_err() {
            self.index = self.words.len();
        }
        Some(packet)
    }
}

/// Encodes a type-3 header for a body of `body_len` words.
#[allow(clippy::cast_possible_truncation)]
#[inline]
pub const fn type3_header(opcode: u8, body_len: usize) -> u32 {
    (3 << 30) | (((body_len as u32).saturating_sub(1) & 0x3FFF) << 16) | ((opcode as u32) << 8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::opcode;

    #[test]
    fn frames_packets() {
        let words = [
            type3_header(opcode::SET_CONTEXT_REG, 3),
            0x10,
            1,
            2,
            0x8000_0000,        // type 2
            (2 << 16) | 0x2100, // type 0, 3 values
            7,
            8,
            9,
        ];
        let packets: Vec<_> = parse(&words).collect::<Result<_, _>>().unwrap();
        assert_eq!(
            packets,
            [
                Packet::Type3 { opcode: opcode::SET_CONTEXT_REG, body: &[0x10, 1, 2] },
                Packet::Type2,
                Packet::Type0 { register: 0x2100, values: &[7, 8, 9] },
            ]
        );
    }

    #[test]
    fn reports_truncation() {
        let words = [type3_header(opcode::NOP, 4), 0];
        let result: Vec<_> = parse(&words).collect();
        assert!(matches!(result[..], [Err(GpuError::TruncatedPacket { index: 0 })]));
    }
}
