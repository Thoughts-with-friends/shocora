//! Nintendo DSP-ADPCM.
//!
//! A frame is 8 bytes: one header byte (predictor index in the high nibble,
//! scale exponent in the low nibble) followed by 14 signed 4-bit samples.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!                                   ▲▲▲▲▲  here
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ compressed samples │   │ adpcm.rs                 │   │ plain samples      │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ DSP-ADPCM decoding       │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unpacks Nintendo's compressed sound format. Terms such as HLE, JIT or shader are explained
//! in `docs/guide/wiiu_term.md`.

/// Samples per 8-byte frame.
pub const SAMPLES_PER_FRAME: usize = 14;
/// Nibbles per frame, including the two header nibbles.
pub const NIBBLES_PER_FRAME: u32 = 16;

/// Decoder history carried between frames.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AdpcmState {
    pub hist1: i16,
    pub hist2: i16,
}

/// Decodes one nibble given the frame header and predictor coefficients.
#[allow(clippy::cast_possible_truncation)]
pub fn decode_sample(
    nibble: u8,
    header: u8,
    coefficients: &[i16; 16],
    state: &mut AdpcmState,
) -> i16 {
    let predictor = usize::from(header >> 4) & 7;
    let scale = 1_i32 << (header & 0xF);
    let (coef1, coef2) =
        (i32::from(coefficients[predictor * 2]), i32::from(coefficients[predictor * 2 + 1]));
    // Sign-extend the 4-bit sample.
    let signed = i32::from((nibble << 4) as i8 >> 4);
    let value = ((signed * scale) << 11)
        + 1024
        + coef1 * i32::from(state.hist1)
        + coef2 * i32::from(state.hist2);
    let sample = (value >> 11).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    state.hist2 = state.hist1;
    state.hist1 = sample;
    sample
}

/// Decodes a whole frame.
pub fn decode_frame(
    frame: &[u8; 8],
    coefficients: &[i16; 16],
    state: &mut AdpcmState,
) -> [i16; SAMPLES_PER_FRAME] {
    let mut out = [0; SAMPLES_PER_FRAME];
    for (i, sample) in out.iter_mut().enumerate() {
        let byte = frame[1 + i / 2];
        let nibble = if i % 2 == 0 { byte >> 4 } else { byte & 0xF };
        *sample = decode_sample(nibble, frame[0], coefficients, state);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_without_prediction() {
        let mut state = AdpcmState::default();
        // Scale 2^0, predictor 0 with zero coefficients: output = nibble (rounded).
        let frame = [0x00, 0x17, 0xF8, 0, 0, 0, 0, 0];
        let out = decode_frame(&frame, &[0; 16], &mut state);
        assert_eq!(&out[..4], &[1, 7, -1, -8]);
    }

    #[test]
    fn prediction_uses_history() {
        let mut coefficients = [0_i16; 16];
        coefficients[0] = 2048; // coef1 = 1.0 in 4.11 fixed point
        let mut state = AdpcmState { hist1: 100, hist2: 0 };
        assert_eq!(decode_sample(0, 0x00, &coefficients, &mut state), 100);
        assert_eq!(decode_sample(1, 0x02, &coefficients, &mut state), 104);
        assert_eq!(state, AdpcmState { hist1: 104, hist2: 100 });
    }
}
