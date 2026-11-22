//! Part of [`super`]: see its documentation.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ compressed game    │   │ zlib125/stream.rs        │   │ plain data         │
//! │ data               │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ zlib (de)compression:    │   │                    │
//! │                    │   │ stream                   │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "stream" share of zlib125.rs (zlib (de)compression), moved here so that no file is
//! longer than 500 lines. It works exactly as if it were still written in that file. The
//! console ships the common zlib library for games; this provides the same functions using a
//! Rust implementation. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;

impl PrefixCrc {
    pub(super) fn extend(&mut self, data: &[u8]) -> u32 {
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
