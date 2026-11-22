//! Part of [`super`]: see its documentation.
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
//! │ game's voices &    │   │ sndcore2/ax_state.rs     │   │ mixed sound        │
//! │ samples            │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ AX audio library: ax     │   │                    │
//! │                    │   │ state                    │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "ax state" share of sndcore2.rs (AX audio library), moved here so that no file is longer
//! than 500 lines. It works exactly as if it were still written in that file. The Wii U's sound
//! library (AX). Games describe "voices" (sounds playing at some volume, pitch and position);
//! this mixes them for your speakers. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;

impl CafeOs {
    /// The block of AX frame `frame`, of the blocks starting at `first`.
    pub(super) const fn final_mix_block(first: u32, frame: u32) -> u32 {
        first + (frame % final_mix::BLOCKS) * final_mix::SIZE
    }

    /// Queues what the TV hears: with a final mix callback, what it made of an earlier frame.
    pub(super) fn play_frame(&mut self, memory: &Memory, mixed: &[i16]) {
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
    pub(super) fn run_final_mix(&mut self, memory: &mut Memory, mixed: &[i16]) {
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
            // Gated like the frame callbacks: pushed regardless, these kept the queue above
            // the limit once it got there, the frame callbacks that feed streamed voices
            // were dropped from then on, and those voices repeated a stale buffer as a tone.
            // Skipped, the block plays as mixed.
            if self.callbacks.pending() < MAX_PENDING_CALLBACKS {
                self.callbacks.push(GuestCall { function, args: vec![block] });
            }
        }
    }
}

pub(super) fn initialize(ctx: &mut HleContext<'_>, renderer_48khz: bool) {
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
pub(super) fn ax_init(ctx: &mut HleContext<'_>) {
    initialize(ctx, false);
}

#[hle_export(lib = "sndcore2", name = "AXInitEx")]
pub(super) fn ax_init_ex(ctx: &mut HleContext<'_>, _pipeline: u32) {
    initialize(ctx, false);
}

#[hle_export(lib = "sndcore2", name = "AXInitWithParams")]
pub(super) fn ax_init_with_params(ctx: &mut HleContext<'_>, params: u32) -> Result<(), HleError> {
    let frequency = ctx.memory.read_u32(params)?;
    initialize(ctx, frequency == RENDERER_FREQ_48KHZ);
    Ok(())
}

#[hle_export(lib = "sndcore2", name = "AXIsInit")]
pub(super) fn ax_is_init(ctx: &mut HleContext<'_>) -> bool {
    ctx.os.ax.initialized
}

#[hle_export(lib = "sndcore2", name = "AXQuit")]
pub(super) fn ax_quit(ctx: &mut HleContext<'_>) {
    ctx.os.ax = AxState::default();
}

#[hle_export(lib = "sndcore2", name = "AXGetMaxVoices")]
pub(super) fn ax_get_max_voices(_ctx: &mut HleContext<'_>) -> u32 {
    MAX_VOICES
}

#[hle_export(lib = "sndcore2", name = "AXGetInputSamplesPerFrame")]
pub(super) fn ax_get_input_samples_per_frame(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.ax.samples_per_frame()
}

#[hle_export(lib = "sndcore2", name = "AXGetInputSamplesPerSec")]
pub(super) fn ax_get_input_samples_per_sec(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.ax.samples_per_frame() * 1000 / FRAME_MILLISECONDS as u32
}

#[hle_export(lib = "sndcore2", name = "AXRegisterAppFrameCallback")]
pub(super) fn ax_register_app_frame_callback(ctx: &mut HleContext<'_>, function: u32) -> i32 {
    if function == 0 {
        return -17;
    }
    ctx.os.ax.app_frame_callbacks.push(function);
    0
}

#[hle_export(lib = "sndcore2", name = "AXDeregisterAppFrameCallback")]
pub(super) fn ax_deregister_app_frame_callback(ctx: &mut HleContext<'_>, function: u32) -> i32 {
    ctx.os.ax.app_frame_callbacks.retain(|f| *f != function);
    0
}

#[hle_export(lib = "sndcore2", name = "AXRegisterFrameCallback")]
pub(super) fn ax_register_frame_callback(ctx: &mut HleContext<'_>, function: u32) -> u32 {
    std::mem::replace(&mut ctx.os.ax.frame_callback, function)
}

/// Aux effects are never called: the signal stays dry.
#[hle_export(lib = "sndcore2", name = "AXRegisterAuxCallback")]
pub(super) fn ax_register_aux_callback(
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
pub(super) fn ax_get_aux_callback(
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
pub(super) fn ax_register_device_final_mix_callback(
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
pub(super) fn ax_get_device_final_mix_callback(
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
pub(super) fn ax_get_device_mode(
    ctx: &mut HleContext<'_>,
    _device: u32,
    out: u32,
) -> Result<i32, HleError> {
    ctx.memory.write_u32(out, 0)?;
    Ok(0)
}

#[hle_export(lib = "sndcore2", name = "AXUserBegin")]
pub(super) fn ax_user_begin(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.ax.user_protection += 1;
    ctx.os.ax.user_protection
}

#[hle_export(lib = "sndcore2", name = "AXUserEnd")]
pub(super) fn ax_user_end(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.ax.user_protection = ctx.os.ax.user_protection.saturating_sub(1);
    ctx.os.ax.user_protection
}

#[hle_export(lib = "sndcore2", name = "AXUserIsProtected")]
pub(super) fn ax_user_is_protected(ctx: &mut HleContext<'_>) -> bool {
    ctx.os.ax.user_protection != 0
}

#[hle_export(lib = "sndcore2", name = "AXVoiceBegin")]
pub(super) fn ax_voice_begin(_ctx: &mut HleContext<'_>, _voice: u32) -> u32 {
    1
}

#[hle_export(lib = "sndcore2", name = "AXVoiceEnd")]
pub(super) fn ax_voice_end(_ctx: &mut HleContext<'_>, _voice: u32) -> u32 {
    0
}

#[hle_export(lib = "sndcore2", name = "AXVoiceIsProtected")]
pub(super) fn ax_voice_is_protected(_ctx: &mut HleContext<'_>, _voice: u32) -> bool {
    false
}

pub(super) fn acquire(
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
pub(super) fn ax_acquire_voice_ex(
    ctx: &mut HleContext<'_>,
    priority: u32,
    callback: u32,
    user: u32,
) -> Result<u32, HleError> {
    acquire(ctx, priority, callback, 0, user)
}

#[hle_export(lib = "sndcore2", name = "AXAcquireVoice")]
pub(super) fn ax_acquire_voice(
    ctx: &mut HleContext<'_>,
    priority: u32,
    callback: u32,
    user: u32,
) -> Result<u32, HleError> {
    acquire(ctx, priority, 0, callback, user)
}

#[hle_export(lib = "sndcore2", name = "AXFreeVoice")]
pub(super) fn ax_free_voice(ctx: &mut HleContext<'_>, voice: u32) {
    if ctx.os.ax.voices.remove(&voice).is_some() {
        ctx.os.ax.free.push(voice);
    }
}

#[hle_export(lib = "sndcore2", name = "AXSetVoicePriority")]
pub(super) fn ax_set_voice_priority(
    ctx: &mut HleContext<'_>,
    voice: u32,
    priority: u32,
) -> Result<(), HleError> {
    Ok(ctx.memory.write_u32(voice + vpb::PRIORITY, priority)?)
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceState")]
pub(super) fn ax_set_voice_state(
    ctx: &mut HleContext<'_>,
    voice: u32,
    state: u32,
) -> Result<(), HleError> {
    ctx.memory.write_u32(voice + vpb::STATE, state)?;
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.running = state == STATE_RUNNING;
        if entry.running {
            entry.voice.playing = true;
            let v = &entry.voice;
            log::debug!(
                "voice {voice:08x} start: {:?} base {:08x} cur {:x} loop {:x} end {:x} looping {} stream {} ratio {:.3} vol {:.3} tv {:.2}/{:.2}",
                v.format,
                v.base_address,
                v.current_offset,
                v.loop_offset,
                v.end_offset,
                v.looping,
                v.stream,
                v.ratio,
                entry.volume,
                entry.tv_left,
                entry.tv_right
            );
        }
    }
    Ok(())
}

#[hle_export(lib = "sndcore2", name = "AXIsVoiceRunning")]
pub(super) fn ax_is_voice_running(ctx: &mut HleContext<'_>, voice: u32) -> bool {
    ctx.os.ax.voices.get(&voice).is_some_and(|v| v.running)
}

/// `AXPBOFFSET`: format, loop flag, then offsets relative to the sample pointer.
pub(super) fn set_offsets(
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
pub(super) fn ax_set_voice_offsets(
    ctx: &mut HleContext<'_>,
    voice: u32,
    offsets: u32,
) -> Result<(), HleError> {
    set_offsets(ctx, voice, offsets, None)
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceOffsetsEx")]
pub(super) fn ax_set_voice_offsets_ex(
    ctx: &mut HleContext<'_>,
    voice: u32,
    offsets: u32,
    samples: u32,
) -> Result<(), HleError> {
    set_offsets(ctx, voice, offsets, Some(samples))
}

#[hle_export(lib = "sndcore2", name = "AXGetVoiceOffsets")]
pub(super) fn ax_get_voice_offsets(
    ctx: &mut HleContext<'_>,
    voice: u32,
    out: u32,
) -> Result<(), HleError> {
    let mut raw = [0; 0x14];
    ctx.memory.read_bytes(voice + vpb::OFFSETS, &mut raw)?;
    if let Some(entry) = ctx.os.ax.voices.get(&voice) {
        raw[12..16].copy_from_slice(&entry.voice.current_offset.to_be_bytes());
    }
    ctx.memory.write_bytes(out, &raw)?;
    Ok(())
}

/// Whether the loop, end and current offsets of an `AXPBOFFSET` lie in the same 512 MiB
/// window of physical memory as each other: the DSP keeps only the low 29 bits of its sample
/// pointers. Titles stream sounds in pieces until this holds (Paper Mario: Color Splash
/// waited for it forever while it was missing). As Cemu does.
#[hle_export(lib = "sndcore2", name = "AXCheckVoiceOffsets")]
pub(super) fn ax_check_voice_offsets(
    ctx: &mut HleContext<'_>,
    offsets: u32,
) -> Result<bool, HleError> {
    let format = ctx.memory.read_u16(offsets)?;
    let word = |at: u32| ctx.memory.read_u32(offsets + at);
    let (loop_offset, end_offset, current_offset) = (word(4)?, word(8)?, word(12)?);
    let samples = word(16)?;
    let base = wiiu_mem_addr::effective_to_physical(samples).unwrap_or(samples);
    let window = |offset: u32| -> u32 {
        let bytes = match format {
            0x19 => offset,                 // PCM8
            0x0A => offset.wrapping_mul(2), // PCM16
            _ => offset / 2,                // ADPCM: nibbles
        };
        base.wrapping_add(bytes) >> 29
    };
    let current = window(current_offset);
    Ok(window(loop_offset) == current && window(end_offset) == current)
}

/// Offset units in `bytes` of sample data: nibbles for ADPCM, samples for PCM.
pub(super) fn units_in(format: SampleFormat, bytes: u32) -> u32 {
    match format {
        SampleFormat::Adpcm => bytes.wrapping_mul(2),
        SampleFormat::Pcm16 => (bytes.cast_signed() / 2).cast_unsigned(),
        SampleFormat::Pcm8 => bytes,
    }
}

/// Makes `offset`, counted from `samples`, an offset from the voice's own base. The `Ex`
/// setters address memory, not the voice's buffer: a stream's loop is often in another
/// buffer than the one playing (Super Mario 3D World's title music), and moving the base
/// there made the voice play from the wrong place and run past its data, as a buzz.
pub(super) fn rebase(voice: &mut Voice, offset: u32, samples: u32) -> u32 {
    if samples < voice.base_address {
        // Offsets stay positive: the base moves down to the lower buffer.
        let shift = units_in(voice.format, voice.base_address - samples);
        voice.base_address = samples;
        voice.current_offset = voice.current_offset.wrapping_add(shift);
        voice.loop_offset = voice.loop_offset.wrapping_add(shift);
        voice.end_offset = voice.end_offset.wrapping_add(shift);
    }
    offset.wrapping_add(units_in(voice.format, samples - voice.base_address))
}
