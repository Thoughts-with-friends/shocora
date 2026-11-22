//! `sndcore2.rpl` / `snd_core.rpl` (AX) and `snduser2.rpl` (AXFX).
//!
//! The guest allocates voices and describes them; every 3 ms the "DSP" (here:
//! [`CafeOs::sound_tick`]) renders them and the title's frame callbacks run.
//! Effects (`AXFX`) are accepted and leave the signal dry.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!                                   ▲▲▲▲▲  here
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ game's voices &    │   │ sndcore2.rs              │   │ mixed sound        │
//! │ samples            │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ AX audio library         │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The Wii U's sound library (AX). Games describe "voices" (sounds playing at some volume,
//! pitch and position); this mixes them for your speakers. Terms such as HLE, JIT or shader are
//! explained in `docs/guide/wiiu_term.md`.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]

use std::collections::{HashMap, VecDeque};

use wiiu_hle_macros::hle_export;
use wiiu_memory::Memory;
use wiiu_peripherals::timebase::FREQUENCY;
use wiiu_sound::{
    SampleFormat, SampleMemory, Voice,
    filter::{Biquad, LowPass},
};

use crate::{
    CafeOs,
    hle::{HleContext, HleError, HleExport, callback::GuestCall},
};

/// Voices the library can hand out.
const MAX_VOICES: u32 = 96;
const VPB_SIZE: u32 = 0x60;
/// Rate of the samples handed to the host, whatever the renderer runs at.
pub const OUTPUT_RATE: u32 = 48_000;
/// Samples per AX frame of a renderer at 48 kHz.
const SAMPLES_PER_FRAME_48KHZ: u32 = 144;
/// Samples per AX frame of a renderer at 32 kHz.
const SAMPLES_PER_FRAME_32KHZ: u32 = 96;
/// `AX_RENDERER_FREQ_48KHZ`, the first word of `AXINITPARAM`.
const RENDERER_FREQ_48KHZ: u32 = 1;
/// Length of an AX frame in milliseconds.
const FRAME_MILLISECONDS: u64 = 3;
/// A guest with this many callbacks waiting is not keeping up: frame callbacks
/// wait for it to catch up, instead of piling up without end.
const MAX_PENDING_CALLBACKS: usize = 32;
/// Frames mixed at most per tick when the guest fell behind real time.
const MAX_CATCH_UP: u64 = 16;
/// Frames the mixer may lag behind real time (300 ms) before the rest is skipped.
///
/// A shorter pause (a long compile, the emulation thread waiting for the renderer) is made
/// up for over the next ticks: dropped, every such pause left the host queue short, and
/// BotW at about 26 fps was heard with a regular click. A longer pause (loading, a
/// debugger) starts over from now.
const MAX_BEHIND: u64 = 100;
/// Interleaved stereo samples kept for the host (about 200 ms).
const MAX_QUEUED_OUTPUT: usize = 2 * 9600;

mod vpb {
    pub const INDEX: u32 = 0x00;
    pub const STATE: u32 = 0x04;
    pub const PRIORITY: u32 = 0x1C;
    pub const CALLBACK: u32 = 0x20;
    pub const USER: u32 = 0x24;
    pub const OFFSETS: u32 = 0x34;
    pub const CALLBACK_EX: u32 = 0x48;
}

/// `AX_VOICE_STATE`.
const STATE_RUNNING: u32 = 1;

/// `AX_DEV_TV`.
const DEVICE_TV: u32 = 0;
/// Bytes of one channel in `AXSetVoiceDeviceMix`'s array: four buses.
const MIX_CHANNEL_SIZE: u32 = 0x10;
/// Largest value of the 1.15 fixed-point volume.
const MAX_VOLUME: f32 = 2.0;

/// `AXFINALMIXCBPARAM` and the planar 32-bit samples it points to: titles that
/// mix in software (every voice of theirs ends up here) overwrite the samples.
mod final_mix {
    pub const CHANNEL_POINTERS: u32 = 0x10;
    pub const SAMPLES: u32 = 0x40;
    /// Bytes of one device's block: pointers and samples for the most channels.
    pub const SIZE: u32 = SAMPLES + 8 * super::SAMPLES_PER_FRAME_48KHZ * 4;
    /// Blocks per device: frames made up for at once all get a block of their own.
    pub const BLOCKS: u32 = 2 * PLAY_DELAY;
    /// Frames between a block's callback being queued and the block being played:
    /// callbacks run later, when the guest gets to them, but always in order.
    pub const PLAY_DELAY: u32 = super::MAX_CATCH_UP as u32;

    /// `(channels, devices)` of the callback's view of `device`.
    #[inline]
    pub const fn layout(device: u32) -> (u32, u32) {
        if device == super::DEVICE_TV { (6, 1) } else { (4, 2) }
    }
}

/// One allocated voice.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
struct AxVoice {
    voice: Voice,
    running: bool,
    volume: f32,
    /// Change of `volume` per output sample.
    volume_delta: f32,
    tv_left: f32,
    tv_right: f32,
    /// Left and right gain at the end of the last frame: they ramp to new values
    /// over a frame, as a jump would be heard as a click.
    gains: [f32; 2],
    loop_count: u32,
    /// `AXSetVoiceBiquad`, then `AXSetVoiceLpf`: applied in this order, as the DSP does.
    #[serde(default)]
    biquad: Option<Biquad>,
    #[serde(default)]
    lpf: Option<LowPass>,
}

impl AxVoice {
    fn new() -> Self {
        Self {
            voice: Voice::new(SampleFormat::Pcm16, 0, 0),
            running: false,
            volume: 1.0,
            volume_delta: 0.0,
            // Silent until the title says where the voice goes.
            tv_left: 0.0,
            tv_right: 0.0,
            gains: [0.0; 2],
            loop_count: 0,
            biquad: None,
            lpf: None,
        }
    }
}

/// AX library state.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct AxState {
    initialized: bool,
    /// Titles choose: a voice's pitch ratio is relative to this rate, not the output's.
    renderer_48khz: bool,
    /// Last stereo sample of the previous frame, where upsampling continues from.
    upsample_from: [i16; 2],
    voice_base: u32,
    free: Vec<u32>,
    voices: HashMap<u32, AxVoice>,
    app_frame_callbacks: Vec<u32>,
    frame_callback: u32,
    aux_callbacks: HashMap<u32, (u32, u32)>,
    final_mix_callbacks: HashMap<u32, u32>,
    /// First guest block of each device with a final mix callback (see [`final_mix`]).
    final_mix_blocks: HashMap<u32, u32>,
    /// AX frames mixed so far.
    frames_mixed: u32,
    fx_hooks: (u32, u32),
    /// Timebase value when AX started.
    first_tick: u64,
    /// AX frames accounted for since then, whether they were made up for or not.
    frames_due: u64,
    /// AX frames skipped after long pauses (profiling).
    #[serde(default)]
    frames_skipped: u64,
    user_protection: u32,
    /// Interleaved stereo for the host.
    #[serde(skip)]
    pub output: VecDeque<i16>,
}

impl AxState {
    /// What the voices are doing, for profiling: why a title is silent.
    pub fn summary(&self) -> String {
        let running: Vec<&AxVoice> = self.voices.values().filter(|v| v.running).collect();
        let loud = running.iter().filter(|v| v.volume > 0.0).count();
        let routed = running.iter().filter(|v| v.tv_left + v.tv_right > 0.0).count();
        let audible =
            running.iter().filter(|v| v.volume > 0.0 && v.tv_left + v.tv_right > 0.0).count();
        format!(
            "voices {} (running {}, volume>0 {loud}, routed to TV {routed}, both {audible}) |              frame callbacks {} | final mix callbacks {:?} | aux callbacks {} | frames skipped {}",
            self.voices.len(),
            running.len(),
            self.app_frame_callbacks.len() + usize::from(self.frame_callback != 0),
            self.final_mix_callbacks.keys().collect::<Vec<_>>(),
            self.aux_callbacks.len(),
            self.frames_skipped,
        )
    }

    /// Samples the renderer makes per AX frame.
    const fn samples_per_frame(&self) -> u32 {
        if self.renderer_48khz { SAMPLES_PER_FRAME_48KHZ } else { SAMPLES_PER_FRAME_32KHZ }
    }

    /// Queues a frame for the host, upsampled from a 32 kHz renderer.
    fn queue_output(&mut self, frame: &[i16]) {
        if self.renderer_48khz {
            self.output.extend(frame);
            return;
        }
        let at = |channel: usize, index: i64| -> f32 {
            if index < 0 {
                f32::from(self.upsample_from[channel])
            } else {
                f32::from(frame[index as usize * 2 + channel])
            }
        };
        // Three output samples for two input samples: one sample of delay gives every output
        // sample a neighbor on both sides, and frames join without a seam.
        let produced = i64::from(SAMPLES_PER_FRAME_48KHZ);
        let mut output = Vec::with_capacity(produced as usize * 2);
        for sample in 0..produced {
            let position = (sample + 1) * 2;
            let (whole, third) = (position.div_euclid(3) - 1, position.rem_euclid(3));
            for channel in 0..2 {
                let before = at(channel, whole);
                // The frame's last sample falls exactly on an input sample, with none after it.
                let value = if third == 0 {
                    before
                } else {
                    (at(channel, whole + 1) - before).mul_add(third as f32 / 3.0, before)
                };
                output.push(value as i16);
            }
        }
        self.output.extend(output);
        if let [.., left, right] = *frame {
            self.upsample_from = [left, right];
        }
    }
}

struct GuestSamples<'a>(&'a Memory);

impl SampleMemory for GuestSamples<'_> {
    fn read_u8(&self, address: u32) -> u8 {
        self.0.read_u8(address).unwrap_or(0)
    }
}

/// AX frames in `ticks` of the timebase.
const fn frames_after(ticks: u64) -> u64 {
    ticks * 1000 / (FRAME_MILLISECONDS * FREQUENCY)
}

/// Of the AX frames the mixer is `behind` real time, how many to skip and how many to mix
/// now; the rest wait for the next tick.
const fn catch_up(behind: u64) -> (u64, u64) {
    let skipped = behind.saturating_sub(MAX_BEHIND);
    let left = behind - skipped;
    (skipped, if left < MAX_CATCH_UP { left } else { MAX_CATCH_UP })
}

impl CafeOs {
    /// Runs every AX frame that became due: guest callbacks, then the mixer.
    pub fn sound_tick(&mut self, memory: &mut Memory) {
        self.mic_tick(memory);
        if !self.ax.initialized {
            return;
        }
        let now = self.timebase.ticks_since_boot();
        let frames_due = frames_after(now.saturating_sub(self.ax.first_tick));
        let behind = frames_due.saturating_sub(self.ax.frames_due);
        if behind == 0 {
            return;
        }
        let (skipped, mixed) = catch_up(behind);
        self.ax.frames_due += skipped + mixed;
        self.ax.frames_skipped += skipped;
        for _ in 0..mixed {
            if self.callbacks.pending() < MAX_PENDING_CALLBACKS {
                if self.ax.frame_callback != 0 {
                    self.callbacks
                        .push(GuestCall { function: self.ax.frame_callback, args: Vec::new() });
                }
                for index in 0..self.ax.app_frame_callbacks.len() {
                    let function = self.ax.app_frame_callbacks[index];
                    self.callbacks.push(GuestCall { function, args: Vec::new() });
                }
            }
            self.mix_frame(memory);
        }
    }

    fn mix_frame(&mut self, memory: &mut Memory) {
        let source = GuestSamples(memory);
        let frame_samples = self.ax.samples_per_frame();
        let frame_size = frame_samples as usize * 2;
        let mut mix = vec![0.0_f32; frame_size];
        let mut finished = Vec::new();
        for (address, voice) in &mut self.ax.voices {
            if !voice.running {
                continue;
            }
            let before = voice.voice.current_offset;
            voice.volume = voice
                .volume_delta
                .mul_add(frame_samples as f32, voice.volume)
                .clamp(0.0, MAX_VOLUME);
            let target = [voice.volume * voice.tv_left, voice.volume * voice.tv_right];
            let step = [
                (target[0] - voice.gains[0]) / frame_samples as f32,
                (target[1] - voice.gains[1]) / frame_samples as f32,
            ];
            let mut gains = voice.gains;
            for frame in mix.as_chunks_mut::<2>().0 {
                let mut sample = voice.voice.render(&source);
                if let Some(biquad) = &mut voice.biquad {
                    sample = biquad.process(sample);
                }
                if let Some(lpf) = &mut voice.lpf {
                    sample = lpf.process(sample);
                }
                gains = [gains[0] + step[0], gains[1] + step[1]];
                frame[0] = f32::mul_add(sample, gains[0], frame[0]);
                frame[1] = f32::mul_add(sample, gains[1], frame[1]);
            }
            voice.gains = target;
            if voice.voice.looping && voice.voice.current_offset < before {
                voice.loop_count += 1;
                log::debug!(
                    "voice {address:08x} looped to {:08x} (offset {:x} of base {:08x})",
                    voice.voice.base_address.wrapping_add(voice.voice.loop_offset / 2),
                    voice.voice.loop_offset,
                    voice.voice.base_address
                );
            }
            if !voice.voice.playing {
                voice.running = false;
                finished.push(*address);
            }
        }
        for address in finished {
            let _ = memory.write_u32(address + vpb::STATE, 0);
        }
        let samples = mix.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16).collect::<Vec<_>>();
        self.play_frame(memory, &samples);
        self.run_final_mix(memory, &samples);
        self.ax.frames_mixed = self.ax.frames_mixed.wrapping_add(1);
        let output = &mut self.ax.output;
        let excess = output.len().saturating_sub(MAX_QUEUED_OUTPUT);
        output.drain(..excess);
    }
}

/// `AX_PB_TYPE_STREAM`: the samples are a ring buffer the title keeps refilling.
const VOICE_TYPE_STREAM: u32 = 1;

macro_rules! offset_setters {
    ($($fn_name:ident, $name:literal, $ex_fn:ident, $ex_name:literal, $field:ident;)*) => {$(
        #[hle_export(lib = "sndcore2", name = $name)]
        fn $fn_name(ctx: &mut HleContext<'_>, voice: u32, offset: u32) {
            if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
                entry.voice.$field = offset;
            }
        }

        #[hle_export(lib = "sndcore2", name = $ex_name)]
        fn $ex_fn(ctx: &mut HleContext<'_>, voice: u32, offset: u32, samples: u32) {
            if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
                entry.voice.$field = rebase(&mut entry.voice, offset, samples);
            }
        }
    )*};
}

offset_setters! {
    ax_set_voice_loop_offset, "AXSetVoiceLoopOffset", ax_set_voice_loop_offset_ex, "AXSetVoiceLoopOffsetEx", loop_offset;
    ax_set_voice_end_offset, "AXSetVoiceEndOffset", ax_set_voice_end_offset_ex, "AXSetVoiceEndOffsetEx", end_offset;
    ax_set_voice_current_offset, "AXSetVoiceCurrentOffset", ax_set_voice_current_offset_ex, "AXSetVoiceCurrentOffsetEx", current_offset;
}

/// Placeholder work area: the effects stay dry, like in Cemu.
const FX_MEM_SIZE: u32 = 0xC;

pub const EXPORTS: &[HleExport] = &[
    AX_INIT,
    AX_INIT_EX,
    AX_INIT_WITH_PARAMS,
    AX_IS_INIT,
    AX_QUIT,
    AX_GET_MAX_VOICES,
    AX_GET_INPUT_SAMPLES_PER_FRAME,
    AX_GET_INPUT_SAMPLES_PER_SEC,
    AX_REGISTER_APP_FRAME_CALLBACK,
    AX_DEREGISTER_APP_FRAME_CALLBACK,
    AX_REGISTER_FRAME_CALLBACK,
    AX_REGISTER_AUX_CALLBACK,
    AX_GET_AUX_CALLBACK,
    AX_REGISTER_DEVICE_FINAL_MIX_CALLBACK,
    AX_GET_DEVICE_FINAL_MIX_CALLBACK,
    AX_GET_DEVICE_MODE,
    AX_USER_BEGIN,
    AX_USER_END,
    AX_USER_IS_PROTECTED,
    AX_VOICE_BEGIN,
    AX_VOICE_END,
    AX_VOICE_IS_PROTECTED,
    AX_ACQUIRE_VOICE_EX,
    AX_ACQUIRE_VOICE,
    AX_FREE_VOICE,
    AX_SET_VOICE_PRIORITY,
    AX_SET_VOICE_STATE,
    AX_IS_VOICE_RUNNING,
    AX_SET_VOICE_OFFSETS,
    AX_SET_VOICE_OFFSETS_EX,
    AX_GET_VOICE_OFFSETS,
    AX_CHECK_VOICE_OFFSETS,
    AX_GET_VOICE_CURRENT_OFFSET_EX,
    AX_GET_VOICE_LOOP_COUNT,
    AX_SET_VOICE_LOOP,
    AX_SET_VOICE_TYPE,
    AX_SET_VOICE_LOOP_OFFSET,
    AX_SET_VOICE_LOOP_OFFSET_EX,
    AX_SET_VOICE_END_OFFSET,
    AX_SET_VOICE_END_OFFSET_EX,
    AX_SET_VOICE_CURRENT_OFFSET,
    AX_SET_VOICE_CURRENT_OFFSET_EX,
    AX_SET_VOICE_SAMPLES_ADDR,
    AX_SET_VOICE_ADPCM,
    AX_SET_VOICE_ADPCM_LOOP,
    AX_SET_VOICE_SRC,
    AX_SET_VOICE_SRC_RATIO,
    AX_SET_VOICE_LPF,
    AX_COMPUTE_LPF_COEFS,
    AX_SET_VOICE_LPF_COEFS,
    AX_SET_VOICE_BIQUAD,
    AX_SET_VOICE_BIQUAD_COEFS,
    AX_SET_VOICE_VE,
    AX_SET_VOICE_VE_DELTA,
    AX_SET_VOICE_DEVICE_MIX,
    AXFX_SET_HOOKS,
    AXFX_GET_HOOKS,
    AXFX_REVERB_STD_EXP_GET_MEM_SIZE,
    AXFX_REVERB_STD_EXP_INIT,
    AXFX_REVERB_STD_EXP_SETTINGS,
    AXFX_REVERB_STD_EXP_SETTINGS_UPDATE,
    AXFX_DELAY_EXP_GET_MEM_SIZE,
    AXFX_DELAY_EXP_INIT,
    AXFX_DELAY_EXP_SETTINGS,
    AXFX_DELAY_EXP_SETTINGS_UPDATE,
];

#[cfg(test)]
#[path = "sndcore2_tests.rs"]
mod tests;

mod ax_state;
mod voice_exports;
#[allow(unused_imports)]
pub(crate) use ax_state::*;
#[allow(unused_imports)]
pub(crate) use voice_exports::*;
