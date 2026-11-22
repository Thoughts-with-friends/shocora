//! Sums voices into host stereo.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!                                   ▲▲▲▲▲  here
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ voices             │   │ mixer.rs                 │   │ stereo samples     │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ mixer                    │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Adds all playing sounds together. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.
use crate::{SampleMemory, Voice};

/// Mixes voices with a master volume.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Mixer {
    pub master_volume: f32,
}

impl Default for Mixer {
    fn default() -> Self {
        Self { master_volume: 1.0 }
    }
}

impl Mixer {
    /// Renders `out.len() / 2` stereo frames into `out` (interleaved L/R).
    pub fn mix(&self, voices: &mut [Voice], memory: &dyn SampleMemory, out: &mut [f32]) {
        for frame in out.as_chunks_mut::<2>().0 {
            let (mut left, mut right) = (0.0, 0.0);
            for voice in voices.iter_mut().filter(|v| v.playing) {
                let sample = voice.render(memory);
                left = f32::mul_add(sample, voice.volume_left, left);
                right = f32::mul_add(sample, voice.volume_right, right);
            }
            frame[0] = (left * self.master_volume).clamp(-1.0, 1.0);
            frame[1] = (right * self.master_volume).clamp(-1.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SampleFormat;

    struct Constant;

    impl SampleMemory for Constant {
        fn read_u8(&self, _address: u32) -> u8 {
            0x40
        }
    }

    #[test]
    fn pans_and_clamps() {
        let mut a = Voice::new(SampleFormat::Pcm8, 0, 100);
        a.volume_right = 0.0;
        let mut b = Voice::new(SampleFormat::Pcm8, 0, 100);
        b.volume_left = 4.0;
        let mut out = [0.0; 4];
        Mixer::default().mix(&mut [a, b], &Constant, &mut out);
        // The first frame is the voices' delay of one sample.
        assert_eq!(out[2], 1.0); // 0.5 + 2.0 clamped
        assert!((out[3] - 0.5).abs() < 1e-6);
    }
}
