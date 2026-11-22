//! One playing voice.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!                                   ▲▲▲▲▲  here
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ sample memory      │   │ voice.rs                 │   │ samples at         │
//! │                    │──►│ ──────────────────────── │──►│ pitch/volume       │
//! │                    │   │ one voice                │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! One playing sound: where its samples are, its pitch and volume. Terms such as HLE, JIT or
//! shader are explained in `docs/guide/wiiu_term.md`.
use crate::adpcm::{self, AdpcmState, NIBBLES_PER_FRAME};

/// Read access to guest sample data.
pub trait SampleMemory {
    fn read_u8(&self, address: u32) -> u8;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SampleFormat {
    Pcm8,
    Pcm16,
    Adpcm,
}

/// Voice parameters and playback state.
///
/// Offsets are in format units: samples for PCM, nibbles for ADPCM (as AX
/// stores them).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Voice {
    pub format: SampleFormat,
    pub base_address: u32,
    pub loop_offset: u32,
    pub end_offset: u32,
    pub current_offset: u32,
    pub looping: bool,
    pub playing: bool,
    /// Source samples advanced per output sample.
    pub ratio: f32,
    pub volume_left: f32,
    pub volume_right: f32,
    pub adpcm_coefficients: [i16; 16],
    pub adpcm_state: AdpcmState,
    /// Decoder history at the loop start (`AXSetVoiceAdpcmLoop`).
    pub adpcm_loop_state: Option<AdpcmState>,
    /// A ring buffer of one continuous sound (`AX_PB_TYPE_STREAM`): looping goes on
    /// decoding with the history it has, as the loop start follows the loop end.
    #[serde(default)]
    pub stream: bool,
    /// Fractional part of the resampling position, between the two samples below.
    phase: f32,
    previous: i16,
    current: i16,
}

impl Voice {
    pub fn new(format: SampleFormat, base_address: u32, end_offset: u32) -> Self {
        Self {
            format,
            base_address,
            loop_offset: 0,
            end_offset,
            current_offset: if format == SampleFormat::Adpcm { 2 } else { 0 },
            looping: false,
            playing: true,
            ratio: 1.0,
            volume_left: 1.0,
            volume_right: 1.0,
            adpcm_coefficients: [0; 16],
            adpcm_state: AdpcmState::default(),
            adpcm_loop_state: None,
            stream: false,
            phase: 0.0,
            previous: 0,
            current: 0,
        }
    }

    /// Decodes the sample at the current offset and advances by one source sample.
    #[allow(clippy::cast_possible_wrap)]
    fn next_source_sample(&mut self, memory: &dyn SampleMemory) -> i16 {
        let sample = match self.format {
            SampleFormat::Pcm8 => {
                i16::from(memory.read_u8(self.base_address + self.current_offset) as i8) << 8
            }
            SampleFormat::Pcm16 => {
                let at = self.base_address + self.current_offset * 2;
                i16::from_be_bytes([memory.read_u8(at), memory.read_u8(at + 1)])
            }
            SampleFormat::Adpcm => {
                let nibble_offset = self.current_offset;
                let frame_start = self.base_address + nibble_offset / NIBBLES_PER_FRAME * 8;
                let header = memory.read_u8(frame_start);
                let byte = memory.read_u8(self.base_address + nibble_offset / 2);
                let nibble = if nibble_offset.is_multiple_of(2) { byte >> 4 } else { byte & 0xF };
                adpcm::decode_sample(
                    nibble,
                    header,
                    &self.adpcm_coefficients,
                    &mut self.adpcm_state,
                )
            }
        };
        self.advance();
        sample
    }

    fn advance(&mut self) {
        if self.current_offset >= self.end_offset {
            if self.looping {
                self.current_offset = self.loop_offset;
                if self.format == SampleFormat::Adpcm {
                    if let Some(state) = self.adpcm_loop_state.filter(|_| !self.stream) {
                        self.adpcm_state = state;
                    }
                    if self.current_offset.is_multiple_of(NIBBLES_PER_FRAME) {
                        self.current_offset += 2;
                    }
                }
            } else {
                self.playing = false;
            }
            return;
        }
        self.current_offset += 1;
        // Skip ADPCM frame headers.
        if self.format == SampleFormat::Adpcm
            && self.current_offset.is_multiple_of(NIBBLES_PER_FRAME)
        {
            self.current_offset += 2;
        }
    }

    /// Produces one output sample (mono, `-1.0..=1.0`) at the voice's ratio,
    /// interpolating between source samples.
    pub fn render(&mut self, memory: &dyn SampleMemory) -> f32 {
        if !self.playing {
            return 0.0;
        }
        self.phase += self.ratio;
        while self.phase >= 1.0 && self.playing {
            self.previous = self.current;
            self.current = self.next_source_sample(memory);
            self.phase -= 1.0;
        }
        let (previous, current) = (f32::from(self.previous), f32::from(self.current));
        (current - previous).mul_add(self.phase.min(1.0), previous) / 32768.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Bytes(Vec<u8>);

    impl SampleMemory for Bytes {
        fn read_u8(&self, address: u32) -> u8 {
            self.0.get(address as usize).copied().unwrap_or(0)
        }
    }

    #[test]
    fn pcm16_one_shot_and_loop() {
        let memory = Bytes(vec![0x40, 0x00, 0xC0, 0x00]);
        let mut voice = Voice::new(SampleFormat::Pcm16, 0, 1);
        // One sample of delay: interpolation needs the one after the output position.
        assert_eq!(voice.render(&memory), 0.0);
        assert!((voice.render(&memory) - 0.5).abs() < 1e-6);
        voice.render(&memory);
        voice.render(&memory);
        assert!(!voice.playing);

        let mut looping = Voice::new(SampleFormat::Pcm16, 0, 1);
        looping.looping = true;
        let samples: Vec<f32> = (0..5).map(|_| looping.render(&memory)).collect();
        assert!((samples[3] - 0.5).abs() < 1e-6 && looping.playing);
    }

    #[test]
    fn a_slower_voice_is_interpolated_and_never_drops_to_silence() {
        // A rising ramp played at 2/3 of the output rate, as a 32 kHz sound is.
        let bytes = (0..64_i16).flat_map(|i| (1000 + i * 300).to_be_bytes()).collect();
        let memory = Bytes(bytes);
        let mut voice = Voice::new(SampleFormat::Pcm16, 0, 63);
        voice.ratio = 2.0 / 3.0;
        let out: Vec<f32> = (0..60).map(|_| voice.render(&memory)).collect();
        for pair in out[3..].windows(2) {
            let step = (pair[1] - pair[0]) * 32768.0;
            assert!((step - 200.0).abs() < 1.0, "step {step} in {pair:?}");
        }
    }

    #[test]
    fn looping_adpcm_restores_the_history_of_the_loop_start() {
        let memory = Bytes(vec![0; 16]);
        let mut voice = Voice::new(SampleFormat::Adpcm, 0, 4);
        voice.looping = true;
        voice.loop_offset = 2;
        let start = AdpcmState { hist1: 11, hist2: 22 };
        voice.adpcm_loop_state = Some(start);
        voice.adpcm_state = AdpcmState { hist1: 500, hist2: 400 };
        for _ in 0..3 {
            voice.render(&memory);
        }
        assert_eq!(voice.adpcm_state, start);
        assert_eq!(voice.current_offset, 2);
    }

    #[test]
    fn a_looping_stream_keeps_its_history() {
        let memory = Bytes(vec![0; 16]);
        let mut voice = Voice::new(SampleFormat::Adpcm, 0, 4);
        voice.looping = true;
        voice.stream = true;
        voice.loop_offset = 2;
        voice.adpcm_loop_state = Some(AdpcmState { hist1: 11, hist2: 22 });
        voice.adpcm_state = AdpcmState { hist1: 500, hist2: 400 };
        for _ in 0..3 {
            voice.render(&memory);
        }
        assert_ne!(voice.adpcm_state, AdpcmState { hist1: 11, hist2: 22 });
        assert_eq!(voice.current_offset, 2);
    }
}
