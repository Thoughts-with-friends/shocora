//! Per-voice filters of the AX DSP: a one-pole low-pass and a biquad.
//!
//! `AXPBLPF` and `AXPBBIQUAD`: titles use them for distance and occlusion; without them
//! sound effects play unfiltered, too bright. The arithmetic follows Cemu's `ax_mix.cpp`.
//!
//! Samples are in `-1.0..=1.0` (a 16-bit sample over 32768).

/// Largest value the DSP keeps as filter history (a 16-bit sample).
const HISTORY_MAX: f32 = 32767.0 / 32768.0;

/// One-pole low-pass: `y[n] = a0 * x[n] - b0 * y[n-1]`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub struct LowPass {
    a0: f32,
    b0: f32,
    previous: f32,
}

impl LowPass {
    /// From the DSP's 0.15 fixed-point coefficients and history.
    pub fn new(a0: i16, b0: i16, yn1: i16) -> Self {
        let mut filter = Self { previous: f32::from(yn1) / 32768.0, ..Self::default() };
        filter.set_coefficients(a0, b0);
        filter
    }

    /// New coefficients, keeping the history (`AXSetVoiceLpfCoefs`).
    pub fn set_coefficients(&mut self, a0: i16, b0: i16) {
        self.a0 = f32::from(a0) / 32767.0;
        self.b0 = f32::from(b0) / 32767.0;
    }

    pub fn process(&mut self, sample: f32) -> f32 {
        let out = self.a0.mul_add(sample, -self.b0 * self.previous);
        self.previous = out;
        out
    }
}

/// Biquad: `y[n] = b0 x[n] + b1 x[n-1] + b2 x[n-2] + a1 y[n-1] + a2 y[n-2]`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    x: [f32; 2],
    y: [f32; 2],
}

impl Biquad {
    /// From the DSP's 2.14 fixed-point coefficients `[b0, b1, b2, a1, a2]` and its history
    /// `[xn1, xn2]`, `[yn1, yn2]`.
    pub fn new(coefficients: [i16; 5], x: [i16; 2], y: [i16; 2]) -> Self {
        let history = |v: [i16; 2]| v.map(|s| f32::from(s) / 32768.0);
        let mut filter = Self { x: history(x), y: history(y), ..Self::default() };
        filter.set_coefficients(coefficients);
        filter
    }

    /// New coefficients, keeping the history (`AXSetVoiceBiquadCoefs`).
    pub fn set_coefficients(&mut self, [b0, b1, b2, a1, a2]: [i16; 5]) {
        let q14 = |v: i16| f32::from(v) / 16384.0;
        self.b = [q14(b0), q14(b1), q14(b2)];
        self.a = [q14(a1), q14(a2)];
    }

    pub fn process(&mut self, sample: f32) -> f32 {
        let [b0, b1, b2] = self.b;
        let [a1, a2] = self.a;
        let feed_forward = b2.mul_add(self.x[1], b1.mul_add(self.x[0], b0 * sample));
        let out = a2.mul_add(self.y[1], a1.mul_add(self.y[0], feed_forward));
        // The output goes on as computed; the history the DSP keeps is a 16-bit sample.
        self.y = [out.clamp(-1.0, HISTORY_MAX), self.y[0]];
        self.x = [sample, self.x[0]];
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Intent: the low-pass smooths a step: it rises gradually to the steady value
    /// `a0 / (1 + b0)` instead of jumping there.
    /// Pass: with a0 = 0.5 and b0 = -0.5 the first output is 0.25 (of a step of 0.5), later
    /// outputs approach 0.5 from below.
    #[test]
    fn low_pass_rises_gradually_to_the_input() {
        let mut filter = LowPass::new(16384, -16384, 0);
        let first = filter.process(0.5);
        assert!((first - 0.25).abs() < 1e-3, "{first}");
        let later = (0..200).fold(first, |_, _| filter.process(0.5));
        assert!(later > 0.49 && later <= 0.5 + 1e-3, "{later}");
    }

    /// Intent: a biquad with b0 = 1 and the rest 0 passes the signal unchanged, and its
    /// coefficients are 2.14 fixed point.
    /// Pass: b0 = 16384 returns every input as it is; b0 = 8192 halves it.
    #[test]
    fn biquad_coefficients_are_2_14_fixed_point() {
        let mut pass = Biquad::new([16384, 0, 0, 0, 0], [0; 2], [0; 2]);
        assert!((pass.process(0.3) - 0.3).abs() < 1e-6);
        let mut half = Biquad::new([8192, 0, 0, 0, 0], [0; 2], [0; 2]);
        assert!((half.process(0.4) - 0.2).abs() < 1e-6);
    }

    /// Intent: the feedback terms use the previous outputs with the coefficients' sign as
    /// stored (the DSP adds `a1 y[n-1]`).
    /// Pass: with b0 = 1 and a1 = 0.5, an impulse decays as 1, 0.5, 0.25.
    #[test]
    fn biquad_feedback_adds_the_previous_outputs() {
        let mut filter = Biquad::new([16384, 0, 0, 8192, 0], [0; 2], [0; 2]);
        let out: Vec<f32> = [1.0, 0.0, 0.0].iter().map(|&s| filter.process(s)).collect();
        assert!(out.iter().zip([1.0, 0.5, 0.25]).all(|(a, b)| (a - b).abs() < 1e-3), "{out:?}");
    }
}
