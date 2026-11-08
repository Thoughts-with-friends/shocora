//! `sndcore2.rpl` / `snd_core.rpl` (AX) and `snduser2.rpl` (AXFX).
//!
//! The guest allocates voices and describes them; every 3 ms the "DSP" (here:
//! [`CafeOs::sound_tick`]) renders them and the title's frame callbacks run.
//! Effects (`AXFX`) are accepted and leave the signal dry.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]

use std::collections::{HashMap, VecDeque};

use wiiu_hle_macros::hle_export;
use wiiu_memory::Memory;
use wiiu_peripherals::timebase::FREQUENCY;
use wiiu_sound::{SampleFormat, SampleMemory, Voice};

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
/// Frames made up for at most when the guest fell behind real time.
const MAX_CATCH_UP: u64 = 16;
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
            "voices {} (running {}, volume>0 {loud}, routed to TV {routed}, both {audible}) |              frame callbacks {} | final mix callbacks {:?} | aux callbacks {}",
            self.voices.len(),
            running.len(),
            self.app_frame_callbacks.len() + usize::from(self.frame_callback != 0),
            self.final_mix_callbacks.keys().collect::<Vec<_>>(),
            self.aux_callbacks.len(),
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

impl CafeOs {
    /// Runs every AX frame that became due: guest callbacks, then the mixer.
    pub fn sound_tick(&mut self, memory: &mut Memory) {
        if !self.ax.initialized {
            return;
        }
        let now = self.timebase.ticks_since_boot();
        let frames_due = frames_after(now.saturating_sub(self.ax.first_tick));
        let due = frames_due - self.ax.frames_due;
        if due == 0 {
            return;
        }
        self.ax.frames_due = frames_due;
        for _ in 0..due.min(MAX_CATCH_UP) {
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
                let sample = voice.voice.render(&source);
                gains = [gains[0] + step[0], gains[1] + step[1]];
                frame[0] = f32::mul_add(sample, gains[0], frame[0]);
                frame[1] = f32::mul_add(sample, gains[1], frame[1]);
            }
            voice.gains = target;
            if voice.voice.looping && voice.voice.current_offset < before {
                voice.loop_count += 1;
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

impl CafeOs {
    /// The block of AX frame `frame`, of the blocks starting at `first`.
    const fn final_mix_block(first: u32, frame: u32) -> u32 {
        first + (frame % final_mix::BLOCKS) * final_mix::SIZE
    }

    /// Queues what the TV hears: with a final mix callback, what it made of an earlier frame.
    fn play_frame(&mut self, memory: &Memory, mixed: &[i16]) {
        let Some(first) = self.ax.final_mix_blocks.get(&DEVICE_TV).copied() else {
            self.ax.queue_output(mixed);
            return;
        };
        let Some(frame) = self.ax.frames_mixed.checked_sub(final_mix::PLAY_DELAY) else {
            self.ax.queue_output(&vec![0; mixed.len()]);
            return;
        };
        let block = Self::final_mix_block(first, frame);
        let frame_samples = self.ax.samples_per_frame();
        let channel = |channel: u32, sample: u32| {
            let at = block + final_mix::SAMPLES + (channel * frame_samples + sample) * 4;
            memory.read_u32(at).unwrap_or(0).cast_signed().clamp(-32768, 32767) as i16
        };
        let played: Vec<i16> = (0..frame_samples)
            .flat_map(|sample| [channel(0, sample), channel(1, sample)])
            .collect();
        self.ax.queue_output(&played);
    }

    /// Hands this frame's samples to the final mix callbacks.
    fn run_final_mix(&mut self, memory: &mut Memory, mixed: &[i16]) {
        let callbacks: Vec<_> = self.ax.final_mix_callbacks.iter().map(|(d, f)| (*d, *f)).collect();
        let frame_samples = self.ax.samples_per_frame();
        for (device, function) in callbacks {
            let Some(first) = self.ax.final_mix_blocks.get(&device).copied() else { continue };
            let block = Self::final_mix_block(first, self.ax.frames_mixed);
            let (channels, devices) = final_mix::layout(device);
            let total = channels * devices;
            let samples = block + final_mix::SAMPLES;
            let _ = memory.fill(samples, total * frame_samples * 4, 0);
            for (i, sample) in mixed.iter().enumerate() {
                let (frame, channel) = ((i / 2) as u32, (i % 2) as u32);
                let at = samples + (channel * frame_samples + frame) * 4;
                let _ = memory.write_u32(at, i32::from(*sample).cast_unsigned());
            }
            for channel in 0..total {
                let pointer = samples + channel * frame_samples * 4;
                let _ =
                    memory.write_u32(block + final_mix::CHANNEL_POINTERS + channel * 4, pointer);
            }
            let _ = memory.write_u32(block, block + final_mix::CHANNEL_POINTERS);
            let _ = memory.write_u16(block + 4, channels as u16);
            let _ = memory.write_u16(block + 6, frame_samples as u16);
            let _ = memory.write_u16(block + 8, devices as u16);
            let _ = memory.write_u16(block + 10, channels as u16);
            self.callbacks.push(GuestCall { function, args: vec![block] });
        }
    }
}

fn initialize(ctx: &mut HleContext<'_>, renderer_48khz: bool) {
    if ctx.os.ax.initialized {
        return;
    }
    ctx.os.ax.initialized = true;
    ctx.os.ax.renderer_48khz = renderer_48khz;
    ctx.os.ax.first_tick = ctx.os.timebase.ticks_since_boot();
    let base = ctx.os.registry.allocate_data(MAX_VOICES * VPB_SIZE, 64);
    ctx.os.ax.voice_base = base;
    ctx.os.ax.free = (0..MAX_VOICES).rev().map(|i| base + i * VPB_SIZE).collect();
}

#[hle_export(lib = "sndcore2", name = "AXInit")]
fn ax_init(ctx: &mut HleContext<'_>) {
    initialize(ctx, false);
}

#[hle_export(lib = "sndcore2", name = "AXInitEx")]
fn ax_init_ex(ctx: &mut HleContext<'_>, _pipeline: u32) {
    initialize(ctx, false);
}

#[hle_export(lib = "sndcore2", name = "AXInitWithParams")]
fn ax_init_with_params(ctx: &mut HleContext<'_>, params: u32) -> Result<(), HleError> {
    let frequency = ctx.memory.read_u32(params)?;
    initialize(ctx, frequency == RENDERER_FREQ_48KHZ);
    Ok(())
}

#[hle_export(lib = "sndcore2", name = "AXIsInit")]
fn ax_is_init(ctx: &mut HleContext<'_>) -> bool {
    ctx.os.ax.initialized
}

#[hle_export(lib = "sndcore2", name = "AXQuit")]
fn ax_quit(ctx: &mut HleContext<'_>) {
    ctx.os.ax = AxState::default();
}

#[hle_export(lib = "sndcore2", name = "AXGetMaxVoices")]
fn ax_get_max_voices(_ctx: &mut HleContext<'_>) -> u32 {
    MAX_VOICES
}

#[hle_export(lib = "sndcore2", name = "AXGetInputSamplesPerFrame")]
fn ax_get_input_samples_per_frame(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.ax.samples_per_frame()
}

#[hle_export(lib = "sndcore2", name = "AXGetInputSamplesPerSec")]
fn ax_get_input_samples_per_sec(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.ax.samples_per_frame() * 1000 / FRAME_MILLISECONDS as u32
}

#[hle_export(lib = "sndcore2", name = "AXRegisterAppFrameCallback")]
fn ax_register_app_frame_callback(ctx: &mut HleContext<'_>, function: u32) -> i32 {
    if function == 0 {
        return -17;
    }
    ctx.os.ax.app_frame_callbacks.push(function);
    0
}

#[hle_export(lib = "sndcore2", name = "AXDeregisterAppFrameCallback")]
fn ax_deregister_app_frame_callback(ctx: &mut HleContext<'_>, function: u32) -> i32 {
    ctx.os.ax.app_frame_callbacks.retain(|f| *f != function);
    0
}

#[hle_export(lib = "sndcore2", name = "AXRegisterFrameCallback")]
fn ax_register_frame_callback(ctx: &mut HleContext<'_>, function: u32) -> u32 {
    std::mem::replace(&mut ctx.os.ax.frame_callback, function)
}

/// Aux effects are never called: the signal stays dry.
#[hle_export(lib = "sndcore2", name = "AXRegisterAuxCallback")]
fn ax_register_aux_callback(
    ctx: &mut HleContext<'_>,
    device: u32,
    id: u32,
    bus: u32,
    function: u32,
    context: u32,
) -> i32 {
    ctx.os.ax.aux_callbacks.insert((device << 16) | (id << 8) | bus, (function, context));
    0
}

#[hle_export(lib = "sndcore2", name = "AXGetAuxCallback")]
fn ax_get_aux_callback(
    ctx: &mut HleContext<'_>,
    device: u32,
    id: u32,
    bus: u32,
    function: u32,
    context: u32,
) -> Result<i32, HleError> {
    let (f, c) =
        ctx.os.ax.aux_callbacks.get(&((device << 16) | (id << 8) | bus)).copied().unwrap_or((0, 0));
    ctx.memory.write_u32(function, f)?;
    ctx.memory.write_u32(context, c)?;
    Ok(0)
}

#[hle_export(lib = "sndcore2", name = "AXRegisterDeviceFinalMixCallback")]
fn ax_register_device_final_mix_callback(
    ctx: &mut HleContext<'_>,
    device: u32,
    function: u32,
) -> i32 {
    if function == 0 {
        ctx.os.ax.final_mix_callbacks.remove(&device);
        ctx.os.ax.final_mix_blocks.remove(&device);
        return 0;
    }
    ctx.os.ax.final_mix_callbacks.insert(device, function);
    if !ctx.os.ax.final_mix_blocks.contains_key(&device) {
        let block = ctx.os.registry.allocate_data(final_mix::BLOCKS * final_mix::SIZE, 64);
        ctx.os.ax.final_mix_blocks.insert(device, block);
    }
    0
}

#[hle_export(lib = "sndcore2", name = "AXGetDeviceFinalMixCallback")]
fn ax_get_device_final_mix_callback(
    ctx: &mut HleContext<'_>,
    device: u32,
    out: u32,
) -> Result<i32, HleError> {
    let function = ctx.os.ax.final_mix_callbacks.get(&device).copied().unwrap_or(0);
    ctx.memory.write_u32(out, function)?;
    Ok(0)
}

/// Stereo on both outputs.
#[hle_export(lib = "sndcore2", name = "AXGetDeviceMode")]
fn ax_get_device_mode(ctx: &mut HleContext<'_>, _device: u32, out: u32) -> Result<i32, HleError> {
    ctx.memory.write_u32(out, 0)?;
    Ok(0)
}

#[hle_export(lib = "sndcore2", name = "AXUserBegin")]
fn ax_user_begin(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.ax.user_protection += 1;
    ctx.os.ax.user_protection
}

#[hle_export(lib = "sndcore2", name = "AXUserEnd")]
fn ax_user_end(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.ax.user_protection = ctx.os.ax.user_protection.saturating_sub(1);
    ctx.os.ax.user_protection
}

#[hle_export(lib = "sndcore2", name = "AXUserIsProtected")]
fn ax_user_is_protected(ctx: &mut HleContext<'_>) -> bool {
    ctx.os.ax.user_protection != 0
}

#[hle_export(lib = "sndcore2", name = "AXVoiceBegin")]
fn ax_voice_begin(_ctx: &mut HleContext<'_>, _voice: u32) -> u32 {
    1
}

#[hle_export(lib = "sndcore2", name = "AXVoiceEnd")]
fn ax_voice_end(_ctx: &mut HleContext<'_>, _voice: u32) -> u32 {
    0
}

#[hle_export(lib = "sndcore2", name = "AXVoiceIsProtected")]
fn ax_voice_is_protected(_ctx: &mut HleContext<'_>, _voice: u32) -> bool {
    false
}

fn acquire(
    ctx: &mut HleContext<'_>,
    priority: u32,
    callback_ex: u32,
    callback: u32,
    user: u32,
) -> Result<u32, HleError> {
    initialize(ctx, false);
    ctx.os.registry.commit(ctx.memory)?;
    let Some(address) = ctx.os.ax.free.pop() else { return Ok(0) };
    ctx.memory.fill(address, VPB_SIZE, 0)?;
    ctx.memory.write_u32(address + vpb::INDEX, (address - ctx.os.ax.voice_base) / VPB_SIZE)?;
    ctx.memory.write_u32(address + vpb::PRIORITY, priority)?;
    ctx.memory.write_u32(address + vpb::CALLBACK, callback)?;
    ctx.memory.write_u32(address + vpb::CALLBACK_EX, callback_ex)?;
    ctx.memory.write_u32(address + vpb::USER, user)?;
    ctx.os.ax.voices.insert(address, AxVoice::new());
    Ok(address)
}

#[hle_export(lib = "sndcore2", name = "AXAcquireVoiceEx")]
fn ax_acquire_voice_ex(
    ctx: &mut HleContext<'_>,
    priority: u32,
    callback: u32,
    user: u32,
) -> Result<u32, HleError> {
    acquire(ctx, priority, callback, 0, user)
}

#[hle_export(lib = "sndcore2", name = "AXAcquireVoice")]
fn ax_acquire_voice(
    ctx: &mut HleContext<'_>,
    priority: u32,
    callback: u32,
    user: u32,
) -> Result<u32, HleError> {
    acquire(ctx, priority, 0, callback, user)
}

#[hle_export(lib = "sndcore2", name = "AXFreeVoice")]
fn ax_free_voice(ctx: &mut HleContext<'_>, voice: u32) {
    if ctx.os.ax.voices.remove(&voice).is_some() {
        ctx.os.ax.free.push(voice);
    }
}

#[hle_export(lib = "sndcore2", name = "AXSetVoicePriority")]
fn ax_set_voice_priority(
    ctx: &mut HleContext<'_>,
    voice: u32,
    priority: u32,
) -> Result<(), HleError> {
    Ok(ctx.memory.write_u32(voice + vpb::PRIORITY, priority)?)
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceState")]
fn ax_set_voice_state(ctx: &mut HleContext<'_>, voice: u32, state: u32) -> Result<(), HleError> {
    ctx.memory.write_u32(voice + vpb::STATE, state)?;
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.running = state == STATE_RUNNING;
        if entry.running {
            entry.voice.playing = true;
        }
    }
    Ok(())
}

#[hle_export(lib = "sndcore2", name = "AXIsVoiceRunning")]
fn ax_is_voice_running(ctx: &mut HleContext<'_>, voice: u32) -> bool {
    ctx.os.ax.voices.get(&voice).is_some_and(|v| v.running)
}

/// `AXPBOFFSET`: format, loop flag, then offsets relative to the sample pointer.
fn set_offsets(
    ctx: &mut HleContext<'_>,
    voice: u32,
    offsets: u32,
    samples: Option<u32>,
) -> Result<(), HleError> {
    let mut raw = [0; 0x14];
    ctx.memory.read_bytes(offsets, &mut raw)?;
    ctx.memory.write_bytes(voice + vpb::OFFSETS, &raw)?;
    let word = |o: usize| u32::from_be_bytes([raw[o], raw[o + 1], raw[o + 2], raw[o + 3]]);
    let format = u16::from_be_bytes([raw[0], raw[1]]);
    let looping = u16::from_be_bytes([raw[2], raw[3]]) != 0;
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        let target = &mut entry.voice;
        target.format = match format {
            0 => SampleFormat::Adpcm,
            0x19 => SampleFormat::Pcm8,
            _ => SampleFormat::Pcm16,
        };
        target.looping = looping;
        target.loop_offset = word(4);
        target.end_offset = word(8);
        target.current_offset = word(12);
        target.base_address = samples.unwrap_or_else(|| word(16));
    }
    Ok(())
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceOffsets")]
fn ax_set_voice_offsets(
    ctx: &mut HleContext<'_>,
    voice: u32,
    offsets: u32,
) -> Result<(), HleError> {
    set_offsets(ctx, voice, offsets, None)
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceOffsetsEx")]
fn ax_set_voice_offsets_ex(
    ctx: &mut HleContext<'_>,
    voice: u32,
    offsets: u32,
    samples: u32,
) -> Result<(), HleError> {
    set_offsets(ctx, voice, offsets, Some(samples))
}

#[hle_export(lib = "sndcore2", name = "AXGetVoiceOffsets")]
fn ax_get_voice_offsets(ctx: &mut HleContext<'_>, voice: u32, out: u32) -> Result<(), HleError> {
    let mut raw = [0; 0x14];
    ctx.memory.read_bytes(voice + vpb::OFFSETS, &mut raw)?;
    if let Some(entry) = ctx.os.ax.voices.get(&voice) {
        raw[12..16].copy_from_slice(&entry.voice.current_offset.to_be_bytes());
    }
    ctx.memory.write_bytes(out, &raw)?;
    Ok(())
}

#[hle_export(lib = "sndcore2", name = "AXGetVoiceCurrentOffsetEx")]
fn ax_get_voice_current_offset_ex(ctx: &mut HleContext<'_>, voice: u32, _samples: u32) -> u32 {
    ctx.os.ax.voices.get(&voice).map_or(0, |v| v.voice.current_offset)
}

#[hle_export(lib = "sndcore2", name = "AXGetVoiceLoopCount")]
fn ax_get_voice_loop_count(ctx: &mut HleContext<'_>, voice: u32) -> u32 {
    ctx.os.ax.voices.get(&voice).map_or(0, |v| v.loop_count)
}

/// `AX_PB_TYPE_STREAM`: the samples are a ring buffer the title keeps refilling.
const VOICE_TYPE_STREAM: u32 = 1;

#[hle_export(lib = "sndcore2", name = "AXSetVoiceType")]
fn ax_set_voice_type(ctx: &mut HleContext<'_>, voice: u32, kind: u32) {
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.voice.stream = kind == VOICE_TYPE_STREAM;
    }
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceLoop")]
fn ax_set_voice_loop(ctx: &mut HleContext<'_>, voice: u32, looping: u32) {
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.voice.looping = looping != 0;
    }
}

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
                entry.voice.$field = offset;
                entry.voice.base_address = samples;
            }
        }
    )*};
}

offset_setters! {
    ax_set_voice_loop_offset, "AXSetVoiceLoopOffset", ax_set_voice_loop_offset_ex, "AXSetVoiceLoopOffsetEx", loop_offset;
    ax_set_voice_end_offset, "AXSetVoiceEndOffset", ax_set_voice_end_offset_ex, "AXSetVoiceEndOffsetEx", end_offset;
    ax_set_voice_current_offset, "AXSetVoiceCurrentOffset", ax_set_voice_current_offset_ex, "AXSetVoiceCurrentOffsetEx", current_offset;
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceSamplesAddr")]
fn ax_set_voice_samples_addr(ctx: &mut HleContext<'_>, voice: u32, samples: u32) {
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.voice.base_address = samples;
    }
}

/// `AXPBADPCM`: sixteen coefficients, gain, predictor/scale and history.
#[hle_export(lib = "sndcore2", name = "AXSetVoiceAdpcm")]
fn ax_set_voice_adpcm(ctx: &mut HleContext<'_>, voice: u32, adpcm: u32) -> Result<(), HleError> {
    let mut coefficients = [0_i16; 16];
    for (i, coefficient) in coefficients.iter_mut().enumerate() {
        *coefficient = ctx.memory.read_u16(adpcm + 2 * i as u32)?.cast_signed();
    }
    let history = [
        ctx.memory.read_u16(adpcm + 0x24)?.cast_signed(),
        ctx.memory.read_u16(adpcm + 0x26)?.cast_signed(),
    ];
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.voice.adpcm_coefficients = coefficients;
        entry.voice.adpcm_state =
            wiiu_sound::adpcm::AdpcmState { hist1: history[0], hist2: history[1] };
    }
    Ok(())
}

/// `AXPBADPCMLOOP`: predictor/scale and the history at the loop start.
#[hle_export(lib = "sndcore2", name = "AXSetVoiceAdpcmLoop")]
fn ax_set_voice_adpcm_loop(
    ctx: &mut HleContext<'_>,
    voice: u32,
    adpcm_loop: u32,
) -> Result<(), HleError> {
    let history = [
        ctx.memory.read_u16(adpcm_loop + 2)?.cast_signed(),
        ctx.memory.read_u16(adpcm_loop + 4)?.cast_signed(),
    ];
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.voice.adpcm_loop_state =
            Some(wiiu_sound::adpcm::AdpcmState { hist1: history[0], hist2: history[1] });
    }
    Ok(())
}

/// `AXPBSRC`: 16.16 fixed-point ratio.
#[hle_export(lib = "sndcore2", name = "AXSetVoiceSrc")]
fn ax_set_voice_src(ctx: &mut HleContext<'_>, voice: u32, src: u32) -> Result<(), HleError> {
    let ratio = ctx.memory.read_u32(src)?;
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.voice.ratio = ratio as f32 / 65536.0;
    }
    Ok(())
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceSrcRatio")]
fn ax_set_voice_src_ratio(ctx: &mut HleContext<'_>, voice: u32, ratio: f32) -> i32 {
    if !(0.0..=64.0).contains(&ratio) {
        return -1;
    }
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.voice.ratio = ratio;
    }
    0
}

/// `AXPBVE`: current volume in 1.15 fixed point, and its change per sample.
#[hle_export(lib = "sndcore2", name = "AXSetVoiceVe")]
fn ax_set_voice_ve(ctx: &mut HleContext<'_>, voice: u32, ve: u32) -> Result<(), HleError> {
    let volume = f32::from(ctx.memory.read_u16(ve)?) / 32768.0;
    let delta = f32::from(ctx.memory.read_u16(ve + 2)?.cast_signed()) / 32768.0;
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.volume = volume;
        entry.volume_delta = delta;
    }
    Ok(())
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceVeDelta")]
fn ax_set_voice_ve_delta(ctx: &mut HleContext<'_>, voice: u32, delta: u32) {
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.volume_delta = f32::from((delta as u16).cast_signed()) / 32768.0;
    }
}

/// Four `{volume, delta}` buses per channel. Only the main bus of the TV is mixed,
/// and of its channels the front left and right.
#[hle_export(lib = "sndcore2", name = "AXSetVoiceDeviceMix")]
fn ax_set_voice_device_mix(
    ctx: &mut HleContext<'_>,
    voice: u32,
    device: u32,
    _index: u32,
    mix: u32,
) -> Result<i32, HleError> {
    if device != 0 {
        return Ok(0);
    }
    let left = f32::from(ctx.memory.read_u16(mix)?) / 32768.0;
    let right = f32::from(ctx.memory.read_u16(mix + MIX_CHANNEL_SIZE)?) / 32768.0;
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.tv_left = left;
        entry.tv_right = right;
    }
    Ok(0)
}

// ---- snduser2 --------------------------------------------------------------

#[hle_export(lib = "snduser2", name = "AXFXSetHooks")]
fn axfx_set_hooks(ctx: &mut HleContext<'_>, alloc: u32, free: u32) {
    ctx.os.ax.fx_hooks = (alloc, free);
}

#[hle_export(lib = "snduser2", name = "AXFXGetHooks")]
fn axfx_get_hooks(ctx: &mut HleContext<'_>, alloc: u32, free: u32) -> Result<(), HleError> {
    ctx.memory.write_u32(alloc, ctx.os.ax.fx_hooks.0)?;
    ctx.memory.write_u32(free, ctx.os.ax.fx_hooks.1)?;
    Ok(())
}

/// Placeholder work area: the effects stay dry, like in Cemu.
const FX_MEM_SIZE: u32 = 0xC;

#[hle_export(lib = "snduser2", name = "AXFXReverbStdExpGetMemSize")]
fn axfx_reverb_std_exp_get_mem_size(_ctx: &mut HleContext<'_>, _params: u32) -> u32 {
    FX_MEM_SIZE
}

#[hle_export(lib = "snduser2", name = "AXFXReverbStdExpInit")]
fn axfx_reverb_std_exp_init(_ctx: &mut HleContext<'_>, _params: u32) -> bool {
    true
}

#[hle_export(lib = "snduser2", name = "AXFXReverbStdExpSettings")]
fn axfx_reverb_std_exp_settings(_ctx: &mut HleContext<'_>, _params: u32) -> bool {
    true
}

#[hle_export(lib = "snduser2", name = "AXFXReverbStdExpSettingsUpdate")]
fn axfx_reverb_std_exp_settings_update(_ctx: &mut HleContext<'_>, _params: u32) -> bool {
    true
}

#[hle_export(lib = "snduser2", name = "AXFXDelayExpGetMemSize")]
fn axfx_delay_exp_get_mem_size(_ctx: &mut HleContext<'_>, _params: u32) -> u32 {
    FX_MEM_SIZE
}

#[hle_export(lib = "snduser2", name = "AXFXDelayExpInit")]
fn axfx_delay_exp_init(_ctx: &mut HleContext<'_>, _params: u32) -> bool {
    true
}

#[hle_export(lib = "snduser2", name = "AXFXDelayExpSettings")]
fn axfx_delay_exp_settings(_ctx: &mut HleContext<'_>, _params: u32) -> bool {
    true
}

#[hle_export(lib = "snduser2", name = "AXFXDelayExpSettingsUpdate")]
fn axfx_delay_exp_settings_update(_ctx: &mut HleContext<'_>, _params: u32) -> bool {
    true
}

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
mod tests {
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn voices_play_and_stop_at_their_end() {
        let mut env = TestEnv::new();
        let voice = env.call("sndcore2", "AXAcquireVoiceEx", &[1, 0, 0]);
        assert_ne!(voice, 0);
        let (offsets, samples) = (SCRATCH, SCRATCH + 0x100);
        env.memory.write_u16(samples, 0x4000).unwrap();
        env.memory.write_u16(offsets, 0xA).unwrap(); // PCM16
        env.memory.write_u32(offsets + 8, 1).unwrap(); // end
        env.memory.write_u32(offsets + 16, samples).unwrap();
        env.call("sndcore2", "AXSetVoiceOffsets", &[voice, offsets]);
        let mix = SCRATCH + 0x200;
        env.memory.write_u16(mix, 0x8000).unwrap();
        env.call("sndcore2", "AXSetVoiceDeviceMix", &[voice, 0, 0, mix]);
        env.call("sndcore2", "AXSetVoiceState", &[voice, 1]);
        assert_eq!(env.call("sndcore2", "AXIsVoiceRunning", &[voice]), 1);
        env.os.mix_frame(&mut env.memory);
        assert_eq!(env.call("sndcore2", "AXIsVoiceRunning", &[voice]), 0);
        assert!(env.os.ax.output.iter().any(|s| *s != 0));
    }

    /// A PCM16 voice holding `value`, running with its mix at `mix`.
    fn constant_voice(env: &mut TestEnv, value: u16, mix: u32) -> u32 {
        let voice = env.call("sndcore2", "AXAcquireVoiceEx", &[1, 0, 0]);
        let (offsets, samples) = (SCRATCH, SCRATCH + 0x1000);
        for i in 0..0x400 {
            env.memory.write_u16(samples + i * 2, value).unwrap();
        }
        env.memory.write_u16(offsets, 0xA).unwrap();
        env.memory.write_u16(offsets + 2, 1).unwrap(); // looping
        env.memory.write_u32(offsets + 8, 0x3FF).unwrap();
        env.memory.write_u32(offsets + 16, samples).unwrap();
        env.call("sndcore2", "AXSetVoiceOffsets", &[voice, offsets]);
        env.call("sndcore2", "AXSetVoiceDeviceMix", &[voice, 0, 0, mix]);
        env.call("sndcore2", "AXSetVoiceState", &[voice, 1]);
        voice
    }

    #[test]
    fn ax_makes_its_frames_in_real_time() {
        // 333 frames a second: of 144 samples at 48 kHz, of 96 at 32 kHz.
        assert_eq!(super::frames_after(wiiu_peripherals::timebase::FREQUENCY), 333);
        assert_eq!(super::frames_after(wiiu_peripherals::timebase::FREQUENCY * 3), 1000);
    }

    #[test]
    fn channels_of_the_device_mix_are_four_buses_apart() {
        let mut env = TestEnv::new();
        let mix = SCRATCH + 0x200;
        // Left: main bus only. The second bus of the left channel must not reach the right.
        env.memory.write_u16(mix, 0x8000).unwrap();
        env.memory.write_u16(mix + 4, 0x8000).unwrap();
        env.memory.write_u16(mix + super::MIX_CHANNEL_SIZE, 0).unwrap();
        constant_voice(&mut env, 0x4000, mix);
        for _ in 0..2 {
            env.os.mix_frame(&mut env.memory);
        }
        let last: Vec<i16> = env.os.ax.output.iter().rev().take(2).copied().collect();
        assert_eq!(last[0], 0, "right");
        assert!(last[1] > 16000, "left {}", last[1]);
    }

    #[test]
    fn a_new_voice_ramps_up_instead_of_clicking() {
        let mut env = TestEnv::new();
        let mix = SCRATCH + 0x200;
        env.memory.write_u16(mix, 0x8000).unwrap();
        env.memory.write_u16(mix + super::MIX_CHANNEL_SIZE, 0x8000).unwrap();
        constant_voice(&mut env, 0x4000, mix);
        env.os.mix_frame(&mut env.memory);
        let left: Vec<i16> = env.os.ax.output.iter().step_by(2).copied().collect();
        let largest = left.windows(2).map(|w| (w[1] - w[0]).abs()).max().unwrap();
        assert!(largest < 1000, "jump of {largest}");
    }

    /// Source samples a voice at ratio 1 uses up per frame, and output values per frame.
    fn rates_after(env: &mut TestEnv) -> (u32, usize) {
        let mix = SCRATCH + 0x200;
        env.memory.write_u16(mix, 0x8000).unwrap();
        let voice = constant_voice(env, 0x4000, mix);
        let before = env.os.ax.voices[&voice].voice.current_offset;
        env.os.mix_frame(&mut env.memory);
        let after = env.os.ax.voices[&voice].voice.current_offset;
        (after - before, env.os.ax.output.len())
    }

    #[test]
    fn plain_init_renders_at_32_khz_and_still_outputs_48() {
        let mut env = TestEnv::new();
        env.call("sndcore2", "AXInit", &[]);
        assert_eq!(env.call("sndcore2", "AXGetInputSamplesPerSec", &[]), 32_000);
        assert_eq!(env.call("sndcore2", "AXGetInputSamplesPerFrame", &[]), 96);
        // A sound at ratio 1 plays at 32 kHz, not 1.5 times too fast and too high.
        assert_eq!(rates_after(&mut env), (96, 288));
    }

    #[test]
    fn the_older_library_name_is_the_same_library() {
        let mut env = TestEnv::new();
        env.call("snd_core", "AXInit", &[]);
        assert_eq!(env.call("snd_core", "AXGetInputSamplesPerSec", &[]), 32_000);
    }

    #[test]
    fn titles_can_ask_for_a_48_khz_renderer() {
        let mut env = TestEnv::new();
        env.memory.write_u32(SCRATCH + 0x300, super::RENDERER_FREQ_48KHZ).unwrap();
        env.call("sndcore2", "AXInitWithParams", &[SCRATCH + 0x300]);
        assert_eq!(env.call("sndcore2", "AXGetInputSamplesPerSec", &[]), 48_000);
        assert_eq!(rates_after(&mut env), (144, 288));
    }

    #[test]
    fn params_can_ask_for_32_khz_too() {
        let mut env = TestEnv::new();
        env.memory.write_u32(SCRATCH + 0x300, 0).unwrap();
        env.call("sndcore2", "AXInitWithParams", &[SCRATCH + 0x300]);
        assert_eq!(env.call("sndcore2", "AXGetInputSamplesPerSec", &[]), 32_000);
    }

    #[test]
    fn upsampling_joins_frames_without_a_seam() {
        let mut ax = super::AxState::default();
        // A rising ramp over two frames, the same on both channels.
        let ramp = |from: i16| -> Vec<i16> {
            (0..96_i16).flat_map(|i| [from + i * 30, from + i * 30]).collect()
        };
        ax.queue_output(&ramp(0));
        ax.queue_output(&ramp(96 * 30));
        assert_eq!(ax.output.len(), 2 * 2 * 144);
        let left: Vec<i32> = ax.output.iter().step_by(2).map(|s| i32::from(*s)).collect();
        // 30 per input sample is 20 per output sample, everywhere including the join.
        for pair in left[2..].windows(2) {
            assert!((pair[1] - pair[0] - 20).abs() <= 1, "step in {pair:?}");
        }
    }

    #[test]
    fn final_mix_results_play_in_order_even_when_callbacks_lag() {
        let mut env = TestEnv::new();
        env.call("sndcore2", "AXInit", &[]);
        env.call("sndcore2", "AXRegisterDeviceFinalMixCallback", &[0, 0x1000]);
        let first = env.os.ax.final_mix_blocks[&0];
        // Every frame's block gets a value of its own, as a late callback would write.
        let delay = super::final_mix::PLAY_DELAY;
        for frame in 0..delay + 4 {
            env.os.mix_frame(&mut env.memory);
            let block = crate::CafeOs::final_mix_block(first, frame);
            let samples = block + super::final_mix::SAMPLES;
            for sample in 0..env.os.ax.samples_per_frame() {
                env.memory.write_u32(samples + sample * 4, frame + 100).unwrap();
            }
        }
        assert_eq!(env.os.ax.output[0], 0, "nothing to play before the delay has passed");
        // The frame played last is the one `delay` frames before it, callback result included.
        let left = env.os.ax.output.iter().rev().nth(1).unwrap();
        assert_eq!(i32::from(*left), 100 + 3);
    }
}
