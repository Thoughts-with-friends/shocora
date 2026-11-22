//! Fits the sound of faster or slower game time into real time.
//!
//! Like a tape played faster or slower, the pitch moves with the speed.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!                                   ▲▲▲▲▲  here
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ game speed         │   │ speed.rs                 │   │ real-time audio    │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tempo fitting            │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Keeps sound in step when the game runs faster or slower than real time. Terms such as HLE,
//! JIT or shader are explained in `docs/guide/wiiu_term.md`.

/// What is left of the last piece of sound, so that pieces join without a seam.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SpeedResampler {
    /// Sum and count of the stereo samples being averaged, when speeding up.
    sum: [i32; 2],
    count: u32,
    /// Last stereo sample, when slowing down.
    last: [i16; 2],
}

impl SpeedResampler {
    /// Resamples interleaved stereo for a speed of `halves / 2`.
    pub fn process(&mut self, input: &[i16], halves: u32) -> Vec<i16> {
        match halves {
            2 => input.to_vec(),
            1 => self.stretch(input),
            _ => self.squeeze(input, halves / 2),
        }
    }

    /// Half speed: every sample, and one halfway to the next.
    fn stretch(&mut self, input: &[i16]) -> Vec<i16> {
        let mut out = Vec::with_capacity(input.len() * 2);
        for frame in input.as_chunks::<2>().0 {
            let middle = self.last.iter().zip(frame).map(|(a, b)| {
                #[allow(clippy::cast_possible_truncation)]
                let value = ((i32::from(*a) + i32::from(*b)) / 2) as i16;
                value
            });
            out.extend(middle);
            out.extend_from_slice(frame);
            self.last = *frame;
        }
        out
    }

    /// Whole times faster: the average of that many samples, so that the tones moved beyond
    /// what the output can carry fade instead of folding back as noise.
    fn squeeze(&mut self, input: &[i16], factor: u32) -> Vec<i16> {
        let mut out = Vec::with_capacity(input.len() / factor as usize + 2);
        for frame in input.as_chunks::<2>().0 {
            self.sum[0] += i32::from(frame[0]);
            self.sum[1] += i32::from(frame[1]);
            self.count += 1;
            if self.count == factor {
                #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                out.extend(self.sum.map(|sum| (sum / factor as i32) as i16));
                self.sum = [0; 2];
                self.count = 0;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_speed_leaves_the_sound_alone() {
        let mut resampler = SpeedResampler::default();
        assert_eq!(resampler.process(&[1, 2, 3, 4], 2), [1, 2, 3, 4]);
    }

    #[test]
    fn faster_speeds_keep_a_part_of_the_samples() {
        for (halves, factor) in [(4, 2), (6, 3), (8, 4)] {
            let mut resampler = SpeedResampler::default();
            let out = resampler.process(&vec![1000; 2 * 480], halves);
            assert_eq!(out.len(), 2 * 480 / factor);
            assert!(out.iter().all(|sample| *sample == 1000));
        }
    }

    #[test]
    fn half_speed_doubles_the_samples_without_a_seam() {
        let mut resampler = SpeedResampler::default();
        let mut out = resampler.process(&[0, 0, 100, 200], 1);
        out.extend(resampler.process(&[200, 400], 1));
        assert_eq!(out, [0, 0, 0, 0, 50, 100, 100, 200, 150, 300, 200, 400]);
    }

    #[test]
    fn pieces_too_short_for_a_sample_add_up() {
        let mut resampler = SpeedResampler::default();
        assert!(resampler.process(&[10, 20], 4).is_empty());
        assert_eq!(resampler.process(&[30, 40], 4), [20, 30]);
    }
}
