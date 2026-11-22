//! Host audio output: plays what the AX mixer produced.
//!
//! The emulator delivers samples in bursts, one per guest frame, and its clock
//! is not the sound card's. A little is kept queued to ride out the bursts. The
//! playback rate follows how fast samples arrive, averaged over seconds, plus a small
//! pull towards the queue's target level, so samples never have to be dropped or
//! repeated while the guest keeps up, and the bursts do not move the pitch.
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
/// Largest change of the playback rate. 0.5 % could not follow a title running a few
/// percent slow (BotW's field at about 56 of 60 vblanks a second): the queue ran dry over
/// and over, and each time the sound faded out and waited, heard as periodic noise. A few
/// percent of pitch is far less noticeable than that.
const MAX_RATE_CHANGE: f32 = 0.08;
/// Output frames over which the arrival rate is averaged (5 s). Following the queue's
/// level directly by up to [`MAX_RATE_CHANGE`] moved the pitch with every burst of a
/// guest frame, by about 2 % within each second: heard as wobbling music.
const RATE_AVERAGE_FRAMES: f32 = 240_000.0;
/// Output frames over which the queue's level is averaged (0.2 s).
const LEVEL_AVERAGE_FRAMES: f32 = 9_600.0;
/// How strongly the rate follows the averaged distance from [`TARGET_FRAMES`].
const RATE_GAIN: f32 = 0.1;
/// Largest correction towards [`TARGET_FRAMES`], on top of the arrival rate.
const MAX_LEVEL_CORRECTION: f32 = 0.02;
/// How fast the sound fades per output frame after running out of samples.
const FADE_OUT: f32 = 0.995;
/// Gain added per output frame when the sound comes back (5 ms at 48 kHz): resuming at
/// full gain was a step from silence, heard as a click after every time the queue ran dry.
const FADE_IN: f32 = 1.0 / 240.0;

/// Samples waiting to be played.
#[derive(Debug, Default)]
struct Fifo {
    /// Interleaved stereo.
    samples: VecDeque<i16>,
    /// Stereo frames ever pushed, to measure how fast they arrive.
    pushed: u64,
}

type Queue = Arc<Mutex<Fifo>>;

/// Output gain shared with the UI, as `f32` bits: a change is heard from the next device
/// buffer on.
#[derive(Debug, Clone)]
pub struct Volume(Arc<AtomicU32>);

impl Volume {
    /// Loudest gain: the settings slider goes up to 200 %.
    pub const MAX: f32 = 2.0;

    /// A gain of `value` (`0.0` silent, `1.0` as the title mixed it, up to [`Self::MAX`]).
    pub fn new(value: f32) -> Self {
        Self(Arc::new(AtomicU32::new(value.clamp(0.0, Self::MAX).to_bits())))
    }

    pub fn set(&self, value: f32) {
        self.0.store(value.clamp(0.0, Self::MAX).to_bits(), Ordering::Relaxed);
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
    /// Gain while the sound comes back, up to 1 (see [`FADE_IN`]).
    gain: f32,
    /// Arrival rate of the input over the nominal one, averaged.
    rate: f32,
    /// Queued stereo frames, averaged.
    level: f32,
    /// [`Fifo::pushed`] at the last device buffer.
    pushed_seen: u64,
}

impl Playback {
    const fn new(step: f32) -> Self {
        Self {
            step,
            phase: 0.0,
            previous: [0.0; 2],
            current: [0.0; 2],
            playing: false,
            gain: 0.0,
            rate: 1.0,
            level: TARGET_FRAMES as f32,
            pushed_seen: 0,
        }
    }

    /// The step for a device buffer of `frames`, after `arrived` input frames came in
    /// since the last one and with `queued_frames` waiting.
    fn adjusted_step(&mut self, frames: usize, arrived: u64, queued_frames: usize) -> f32 {
        if self.playing {
            let expected = (frames as f32 * self.step).max(1.0);
            let share = (frames as f32 / RATE_AVERAGE_FRAMES).min(1.0);
            self.rate =
                ((arrived as f32 / expected).min(3.0) - self.rate).mul_add(share, self.rate);

            let share = (frames as f32 / LEVEL_AVERAGE_FRAMES).min(1.0);
            self.level = (queued_frames as f32 - self.level).mul_add(share, self.level);
        } else {
            self.level = queued_frames as f32;
        }

        let rate = self.rate.clamp(1.0 - MAX_RATE_CHANGE, 1.0 + MAX_RATE_CHANGE);
        let error = (self.level - TARGET_FRAMES as f32) / TARGET_FRAMES as f32;
        let correction = (error * RATE_GAIN).clamp(-MAX_LEVEL_CORRECTION, MAX_LEVEL_CORRECTION);

        self.step * rate * (1.0 + correction)
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

            let gain = self.gain;
            self.gain = 0.0;

            return self.previous.map(|sample| sample * gain);
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

        let gain = self.gain;
        self.gain = (self.gain + FADE_IN).min(1.0);

        [
            f32::mul_add(next_left - left, self.phase, left) * gain,
            f32::mul_add(next_right - right, self.phase, right) * gain,
        ]
    }

    /// Fills a device buffer of interleaved frames.
    fn fill<T>(
        &mut self,
        queue: &mut Fifo,
        data: &mut [T],
        channels: usize,
        gain: f32,
        convert: impl Fn(f32) -> T,
    ) {
        let arrived = queue.pushed - self.pushed_seen;
        self.pushed_seen = queue.pushed;

        let frames = data.len() / channels.max(1);
        let step = self.adjusted_step(frames, arrived, queue.samples.len() / 2);

        for frame in data.chunks_mut(channels) {
            let stereo = self.next_frame(&mut queue.samples, step);
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
                    playback.fill(&mut guard, data, channels, float_volume.get(), |sample| {
                        sample.clamp(-1.0, 1.0)
                    });
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
        queue.samples.extend(samples.iter().copied());
        queue.pushed += samples.len() as u64 / 2;

        let excess = queue.samples.len().saturating_sub(MAX_FRAMES * 2);
        queue.samples.drain(..excess);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(frames: usize, value: i16) -> VecDeque<i16> {
        std::iter::repeat_n(value, frames * 2).collect()
    }

    /// Intent: the settings slider's range above 100 % is heard, not cut back to 100 %.
    #[test]
    fn volume_goes_above_full() {
        let volume = Volume::new(1.5);
        assert!((volume.get() - 1.5).abs() < f32::EPSILON);
        volume.set(9.0);
        assert!((volume.get() - Volume::MAX).abs() < f32::EPSILON);
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

        let output: Vec<f32> = (0..400).map(|_| playback.next_frame(&mut queue, 0.5)[0]).collect();
        let steps: Vec<f32> = output.windows(2).map(|w| w[1] - w[0]).collect();
        let expected = 2.0 / 32768.0;

        // After the fade-in.
        assert!(steps[300..].iter().all(|step| (step - expected).abs() < 1e-5), "{steps:?}");
    }

    /// Intent: after running dry, the sound comes back with a short fade instead of a step
    /// from silence, which was heard as a click.
    /// Pass: refilled with a constant, the first output is near 0, outputs never fall, and
    /// the full value is reached within `1 / FADE_IN` frames.
    #[test]
    fn coming_back_fades_in() {
        let mut playback = Playback::new(1.0);
        let mut queue = filled(TARGET_FRAMES * 2, 16384);
        let output: Vec<f32> = (0..400).map(|_| playback.next_frame(&mut queue, 1.0)[0]).collect();

        assert!(output[1] < 0.01, "{}", output[1]);
        assert!(output.windows(2).all(|w| w[1] >= w[0] - 1e-6));
        assert!((output[300] - 0.5).abs() < 1e-3, "{}", output[300]);
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

    /// Intent: the rate follows a title running slow, within the limit.
    /// Pass: input arriving at 93 % for 20 s brings the step near 0.93; none at all
    /// stops at the limit.
    #[test]
    fn the_rate_follows_slow_input_within_the_limit() {
        let mut playback = Playback::new(1.0);
        playback.playing = true;

        let mut step = 1.0;
        for _ in 0..2000 {
            step = playback.adjusted_step(480, 446, TARGET_FRAMES);
        }
        assert!((step - 0.93).abs() < 0.005, "{step}");

        for _ in 0..2000 {
            step = playback.adjusted_step(480, 0, TARGET_FRAMES);
        }
        assert!(step >= 1.0 - MAX_RATE_CHANGE - 1e-6, "{step}");
    }

    /// Intent: input in bursts of a guest frame does not move the pitch.
    /// Pass: 800 frames every 16.7 ms against 10 ms device buffers keep the step within
    /// 0.3 % of 1 once settled.
    #[test]
    fn bursts_do_not_move_the_pitch() {
        let mut playback = Playback::new(1.0);
        playback.playing = true;

        let (mut queued, mut due, mut steps) = (TARGET_FRAMES as f32, 0.0_f32, Vec::new());
        for buffer in 0..6000 {
            due += 480.0;
            let arrived = if due >= 800.0 {
                due -= 800.0;
                800
            } else {
                0
            };
            queued += arrived as f32;

            let step = playback.adjusted_step(480, arrived, queued as usize);
            queued = 480.0_f32.mul_add(-step, queued);

            if buffer > 3000 {
                steps.push(step);
            }
        }
        assert!(steps.iter().all(|s| (s - 1.0).abs() < 0.003), "{steps:?}");
    }
}
