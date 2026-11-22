//! Wii U audio: AX-style voices mixed to host-rate stereo.
//!
//! ```text
//!  guest sample memory ──► Voice (PCM8/PCM16/DSP-ADPCM decode, looping)
//!                              │ resample (ratio)
//!                              ▼
//!                           Mixer ──► interleaved f32 stereo for the host
//! ```
//! Voice parameters are written by the `sndcore2` HLE library; this crate only
//! does the signal processing.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!                                   ▲▲▲▲▲  here
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ voices from AX     │   │ lib.rs                   │   │ stereo for         │
//! │                    │──►│ ──────────────────────── │──►│ speakers           │
//! │                    │   │ audio mixing             │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Mixes the game's sounds into the stereo signal your PC plays. Terms such as HLE, JIT or
//! shader are explained in `docs/guide/wiiu_term.md`.
pub mod adpcm;
pub mod filter;
pub mod mixer;
pub mod speed;
pub mod voice;

pub use mixer::Mixer;
pub use voice::{SampleFormat, SampleMemory, Voice};

/// Output sample rate of the mixer.
pub const OUTPUT_RATE: u32 = 48_000;

#[cfg(test)]
mod tests {
    use super::*;

    /// Intent: the mixer's output rate is the console's 48 kHz, which the front end's audio
    /// device opens with.
    /// Pass: `OUTPUT_RATE` is 48 000.
    #[test]
    fn output_is_48_khz() {
        assert_eq!(OUTPUT_RATE, 48_000);
    }
}
