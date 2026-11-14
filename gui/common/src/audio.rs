//! Host audio output: plays what the AX mixer produced.
//!
//! The emulator delivers samples in bursts, one per guest frame, and its clock
//! is not the sound card's. A little is kept queued to ride out the bursts, and the
//! playback rate follows the queue's level by an inaudible amount, so samples
//! never have to be dropped or repeated while the guest keeps up.
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU32, Ordering},
    },
};

use cpal::{
    SampleFormat,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};

/// Stereo frames kept queued (about 60 ms at 48 kHz); playback starts at this level.
const TARGET_FRAMES: usize = 2880;
/// Stereo frames queued before the oldest are dropped (about 250 ms).
const MAX_FRAMES: usize = 12_000;
/// Largest change of the playback rate: 0.5 % is below what can be heard.
const MAX_RATE_CHANGE: f32 = 0.005;
/// How fast the sound fades per output frame after running out of samples.
const FADE_OUT: f32 = 0.995;

type Queue = Arc<Mutex<VecDeque<i16>>>;

/// Output gain shared with the UI, as `f32` bits: a change is heard from the next device
/// buffer on.
#[derive(Debug, Clone)]
pub struct Volume(Arc<AtomicU32>);

impl Volume {
    /// A gain of `value` (`0.0` silent, `1.0` as the title mixed it).
    pub fn new(value: f32) -> Self {
        Self(Arc::new(AtomicU32::new(value.clamp(0.0, 1.0).to_bits())))
    }

    pub fn set(&self, value: f32) {
        self.0.store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}

/// Turns queued stereo samples into a steady stream at the device's rate.
#[derive(Debug, Clone)]
struct Playback {
    /// Input frames per output frame.
    step: f32,
    phase: f32,
    previous: [f32; 2],
    current: [f32; 2],
    /// `false` while waiting for the queue to fill up.
    playing: bool,
}

impl Playback {
    const fn new(step: f32) -> Self {
        Self { step, phase: 0.0, previous: [0.0; 2], current: [0.0; 2], playing: false }
    }

    /// The rate that moves the queue towards its target. Called per device buffer.
    fn adjusted_step(&self, queued_frames: usize) -> f32 {
        let error = (queued_frames as f32 - TARGET_FRAMES as f32) / TARGET_FRAMES as f32;
        self.step * (1.0 + (error * MAX_RATE_CHANGE).clamp(-MAX_RATE_CHANGE, MAX_RATE_CHANGE))
    }

    /// The next output frame, in `-1.0..=1.0`.
    fn next_frame(&mut self, queue: &mut VecDeque<i16>, step: f32) -> [f32; 2] {
        if !self.playing && queue.len() / 2 >= TARGET_FRAMES {
            self.playing = true;
        }
        if !self.playing {
            // Out of samples: a fade instead of a step to a held value.
            self.previous = self.previous.map(|sample| sample * FADE_OUT);
            self.current = self.previous;
            return self.previous;
        }
        self.phase += step;
        // Each pass takes one off a small sum of positive steps: this cannot stall.
        #[allow(clippy::while_float)]
        while self.phase >= 1.0 {
            self.phase -= 1.0;
            self.previous = self.current;
            if let (Some(left), Some(right)) = (queue.pop_front(), queue.pop_front()) {
                self.current = [f32::from(left) / 32768.0, f32::from(right) / 32768.0];
            } else {
                self.playing = false;
                self.phase = 0.0;
                self.current = self.previous;
            }
        }
        let [left, right] = self.previous;
        let [next_left, next_right] = self.current;
        [
            f32::mul_add(next_left - left, self.phase, left),
            f32::mul_add(next_right - right, self.phase, right),
        ]
    }

    /// Fills a device buffer of interleaved frames.
    fn fill<T>(
        &mut self,
        queue: &mut VecDeque<i16>,
        data: &mut [T],
        channels: usize,
        gain: f32,
        convert: impl Fn(f32) -> T,
    ) {
        let step = self.adjusted_step(queue.len() / 2);
        for frame in data.chunks_mut(channels) {
            let stereo = self.next_frame(queue, step);
            for (channel, slot) in frame.iter_mut().enumerate() {
                *slot = convert(stereo[channel.min(1)] * gain);
            }
        }
    }
}

/// The open output stream. Without an audio device the samples are discarded.
pub struct AudioOutput {
    queue: Queue,
    _stream: Option<cpal::Stream>,
}

impl std::fmt::Debug for AudioOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioOutput").finish_non_exhaustive()
    }
}

impl AudioOutput {
    /// Opens the default output device, playing at `volume`.
    pub fn open(input_rate: u32, volume: Volume) -> Self {
        let queue = Queue::default();
        let stream = Self::open_stream(input_rate, &queue, volume);
        if let Err(error) = &stream {
            log::warn!("no audio output: {error}");
        }
        Self { queue, _stream: stream.ok() }
    }

    fn open_stream(
        input_rate: u32,
        queue: &Queue,
        volume: Volume,
    ) -> Result<cpal::Stream, Box<dyn std::error::Error>> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("no default output device")?;
        let supported = device.default_output_config()?;
        let config = supported.config();
        let channels = usize::from(config.channels);
        let mut playback = Playback::new(input_rate as f32 / config.sample_rate as f32);
        let on_error = |error| log::warn!("audio stream: {error}");
        let queue = Arc::clone(queue);
        let float_volume = volume.clone();
        let stream = match supported.sample_format() {
            SampleFormat::I16 => device.build_output_stream(
                &config,
                move |data: &mut [i16], _: &_| {
                    let mut guard = queue.lock().unwrap_or_else(PoisonError::into_inner);
                    playback.fill(&mut guard, data, channels, volume.get(), |sample| {
                        (sample.clamp(-1.0, 1.0) * 32767.0) as i16
                    });
                },
                on_error,
                None,
            )?,
            _ => device.build_output_stream(
                &config,
                move |data: &mut [f32], _: &_| {
                    let mut guard = queue.lock().unwrap_or_else(PoisonError::into_inner);
                    playback.fill(&mut guard, data, channels, float_volume.get(), |sample| sample);
                },
                on_error,
                None,
            )?,
        };
        stream.play()?;
        Ok(stream)
    }

    /// Appends interleaved stereo samples.
    pub fn push(&self, samples: &[i16]) {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        queue.extend(samples.iter().copied());
        let excess = queue.len().saturating_sub(MAX_FRAMES * 2);
        queue.drain(..excess);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(frames: usize, value: i16) -> VecDeque<i16> {
        std::iter::repeat_n(value, frames * 2).collect()
    }

    #[test]
    fn waits_for_the_queue_to_fill_before_playing() {
        let mut playback = Playback::new(1.0);
        let mut queue = filled(TARGET_FRAMES - 1, 16384);
        assert_eq!(playback.next_frame(&mut queue, 1.0), [0.0, 0.0]);
        assert_eq!(queue.len(), (TARGET_FRAMES - 1) * 2);
        queue.extend([16384, 16384]);
        playback.next_frame(&mut queue, 1.0);
        assert!(playback.playing);
    }

    #[test]
    fn slower_input_is_interpolated_without_steps() {
        let mut playback = Playback::new(0.5);
        let mut queue: VecDeque<i16> =
            (0..TARGET_FRAMES as i32).flat_map(|i| [(i * 4) as i16; 2]).collect();
        let output: Vec<f32> = (0..64).map(|_| playback.next_frame(&mut queue, 0.5)[0]).collect();
        let steps: Vec<f32> = output.windows(2).map(|w| w[1] - w[0]).collect();
        let expected = 2.0 / 32768.0;
        assert!(steps[8..].iter().all(|step| (step - expected).abs() < 1e-5), "{steps:?}");
    }

    #[test]
    fn running_out_fades_and_waits_for_the_target_again() {
        let mut playback = Playback::new(1.0);
        let mut queue = filled(TARGET_FRAMES, 16384);
        for _ in 0..=TARGET_FRAMES {
            playback.next_frame(&mut queue, 1.0);
        }
        assert!(!playback.playing);

        let first = playback.next_frame(&mut queue, 1.0)[0];
        let later = (0..1000).fold(first, |_, _| playback.next_frame(&mut queue, 1.0)[0]);
        assert!(first > 0.4 && later < 0.01, "{first} then {later}");
    }

    #[test]
    fn the_rate_follows_the_queue_within_the_limit() {
        let playback = Playback::new(1.0);
        assert!((playback.adjusted_step(TARGET_FRAMES) - 1.0).abs() < 1e-6);
        assert!(playback.adjusted_step(TARGET_FRAMES * 4) <= 1.0 + MAX_RATE_CHANGE + 1e-6);
        assert!(playback.adjusted_step(TARGET_FRAMES * 4) > 1.0);
        assert!(playback.adjusted_step(0) < 1.0);
    }
}
