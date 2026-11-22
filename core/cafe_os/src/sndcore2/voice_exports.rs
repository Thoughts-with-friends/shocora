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
//! │ game's voices &    │   │ sndcore2/voice_exports.r │   │ mixed sound        │
//! │ samples            │   │ s                        │   │                    │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ AX audio library: voice  │   │                    │
//! │                    │   │ exports                  │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "voice exports" share of sndcore2.rs (AX audio library), moved here so that no file is
//! longer than 500 lines. It works exactly as if it were still written in that file. The Wii
//! U's sound library (AX). Games describe "voices" (sounds playing at some volume, pitch and
//! position); this mixes them for your speakers. Terms such as HLE, JIT or shader are explained
//! in `docs/guide/wiiu_term.md`.

use super::*;

#[hle_export(lib = "sndcore2", name = "AXGetVoiceCurrentOffsetEx")]
pub(super) fn ax_get_voice_current_offset_ex(
    ctx: &mut HleContext<'_>,
    voice: u32,
    samples: u32,
) -> u32 {
    ctx.os.ax.voices.get(&voice).map_or(0, |v| {
        let from_samples = units_in(v.voice.format, samples.wrapping_sub(v.voice.base_address));
        v.voice.current_offset.wrapping_sub(from_samples)
    })
}

#[hle_export(lib = "sndcore2", name = "AXGetVoiceLoopCount")]
pub(super) fn ax_get_voice_loop_count(ctx: &mut HleContext<'_>, voice: u32) -> u32 {
    ctx.os.ax.voices.get(&voice).map_or(0, |v| v.loop_count)
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceType")]
pub(super) fn ax_set_voice_type(ctx: &mut HleContext<'_>, voice: u32, kind: u32) {
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.voice.stream = kind == VOICE_TYPE_STREAM;
    }
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceLoop")]
pub(super) fn ax_set_voice_loop(ctx: &mut HleContext<'_>, voice: u32, looping: u32) {
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.voice.looping = looping != 0;
    }
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceSamplesAddr")]
pub(super) fn ax_set_voice_samples_addr(ctx: &mut HleContext<'_>, voice: u32, samples: u32) {
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.voice.base_address = samples;
    }
}

/// `AXPBADPCM`: sixteen coefficients, gain, predictor/scale and history.
#[hle_export(lib = "sndcore2", name = "AXSetVoiceAdpcm")]
pub(super) fn ax_set_voice_adpcm(
    ctx: &mut HleContext<'_>,
    voice: u32,
    adpcm: u32,
) -> Result<(), HleError> {
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
pub(super) fn ax_set_voice_adpcm_loop(
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
pub(super) fn ax_set_voice_src(
    ctx: &mut HleContext<'_>,
    voice: u32,
    src: u32,
) -> Result<(), HleError> {
    let ratio = ctx.memory.read_u32(src)?;
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.voice.ratio = ratio as f32 / 65536.0;
    }
    Ok(())
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceSrcRatio")]
pub(super) fn ax_set_voice_src_ratio(ctx: &mut HleContext<'_>, voice: u32, ratio: f32) -> i32 {
    if !(0.0..=64.0).contains(&ratio) {
        return -1;
    }
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.voice.ratio = ratio;
    }
    0
}

/// `AXPBLPF`: `on`, then the history and coefficients (16 bits each).
#[hle_export(lib = "sndcore2", name = "AXSetVoiceLpf")]
pub(super) fn ax_set_voice_lpf(
    ctx: &mut HleContext<'_>,
    voice: u32,
    lpf: u32,
) -> Result<(), HleError> {
    let field = |i: u32| ctx.memory.read_u16(lpf + 2 * i).map(u16::cast_signed);
    let (on, yn1, a0, b0) = (field(0)?, field(1)?, field(2)?, field(3)?);
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.lpf = (on != 0).then(|| LowPass::new(a0, b0, yn1));
    }
    Ok(())
}

/// The one-pole low-pass coefficients for a cutoff of `freq` Hz at 32 kHz, as Cemu computes
/// them. Missing, the coefficients a title asked for stayed zero and its filtered voices came
/// out silent or faint (Mario Tennis: Ultra Smash sounded far too quiet).
pub(super) fn lpf_coefficients(freq: u32) -> (u16, u16) {
    let t1 = (freq as f32 / 32000.0 * std::f32::consts::TAU).cos();
    let t2 = 2.0 - t1;
    let b = (t2 - t2.mul_add(t2, -1.0).sqrt()).mul_add(32768.0, 0.5);
    if b < 32768.0 { (0x7FFF - b as u16, b as u16) } else { (0, 0x7FFF) }
}

#[hle_export(lib = "sndcore2", name = "AXComputeLpfCoefs")]
pub(super) fn ax_compute_lpf_coefs(
    ctx: &mut HleContext<'_>,
    freq: u32,
    a0: u32,
    b0: u32,
) -> Result<(), HleError> {
    let (a, b) = lpf_coefficients(freq);
    ctx.memory.write_u16(a0, a)?;
    ctx.memory.write_u16(b0, b)?;
    Ok(())
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceLpfCoefs")]
pub(super) fn ax_set_voice_lpf_coefs(ctx: &mut HleContext<'_>, voice: u32, a0: u32, b0: u32) {
    let lpf = ctx.os.ax.voices.get_mut(&voice).and_then(|entry| entry.lpf.as_mut());
    if let Some(lpf) = lpf {
        lpf.set_coefficients((a0 as u16).cast_signed(), (b0 as u16).cast_signed());
    }
}

/// `AXPBBIQUAD`: `on`, `xn1, xn2, yn1, yn2`, then `b0, b1, b2, a1, a2` (16 bits each).
#[hle_export(lib = "sndcore2", name = "AXSetVoiceBiquad")]
pub(super) fn ax_set_voice_biquad(
    ctx: &mut HleContext<'_>,
    voice: u32,
    biquad: u32,
) -> Result<(), HleError> {
    let mut fields = [0_i16; 10];
    for (i, field) in fields.iter_mut().enumerate() {
        *field = ctx.memory.read_u16(biquad + 2 * i as u32)?.cast_signed();
    }
    let [on, xn1, xn2, yn1, yn2, b0, b1, b2, a1, a2] = fields;
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.biquad = (on != 0).then(|| Biquad::new([b0, b1, b2, a1, a2], [xn1, xn2], [yn1, yn2]));
    }
    Ok(())
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceBiquadCoefs")]
pub(super) fn ax_set_voice_biquad_coefs(
    ctx: &mut HleContext<'_>,
    voice: u32,
    b0: u32,
    b1: u32,
    b2: u32,
    a1: u32,
    a2: u32,
) {
    let biquad = ctx.os.ax.voices.get_mut(&voice).and_then(|entry| entry.biquad.as_mut());
    if let Some(biquad) = biquad {
        biquad.set_coefficients([b0, b1, b2, a1, a2].map(|c| (c as u16).cast_signed()));
    }
}

/// `AXPBVE`: current volume in 1.15 fixed point, and its change per sample.
#[hle_export(lib = "sndcore2", name = "AXSetVoiceVe")]
pub(super) fn ax_set_voice_ve(
    ctx: &mut HleContext<'_>,
    voice: u32,
    ve: u32,
) -> Result<(), HleError> {
    let volume = f32::from(ctx.memory.read_u16(ve)?) / 32768.0;
    let delta = f32::from(ctx.memory.read_u16(ve + 2)?.cast_signed()) / 32768.0;
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.volume = volume;
        entry.volume_delta = delta;
    }
    Ok(())
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceVeDelta")]
pub(super) fn ax_set_voice_ve_delta(ctx: &mut HleContext<'_>, voice: u32, delta: u32) {
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.volume_delta = f32::from((delta as u16).cast_signed()) / 32768.0;
    }
}

/// Four `{volume, delta}` buses per channel. Only the main bus of the TV is mixed,
/// and of its channels the front left and right.
#[hle_export(lib = "sndcore2", name = "AXSetVoiceDeviceMix")]
pub(super) fn ax_set_voice_device_mix(
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
pub(super) fn axfx_set_hooks(ctx: &mut HleContext<'_>, alloc: u32, free: u32) {
    ctx.os.ax.fx_hooks = (alloc, free);
}

#[hle_export(lib = "snduser2", name = "AXFXGetHooks")]
pub(super) fn axfx_get_hooks(
    ctx: &mut HleContext<'_>,
    alloc: u32,
    free: u32,
) -> Result<(), HleError> {
    ctx.memory.write_u32(alloc, ctx.os.ax.fx_hooks.0)?;
    ctx.memory.write_u32(free, ctx.os.ax.fx_hooks.1)?;
    Ok(())
}

#[hle_export(lib = "snduser2", name = "AXFXReverbStdExpGetMemSize")]
pub(super) fn axfx_reverb_std_exp_get_mem_size(_ctx: &mut HleContext<'_>, _params: u32) -> u32 {
    FX_MEM_SIZE
}

#[hle_export(lib = "snduser2", name = "AXFXReverbStdExpInit")]
pub(super) fn axfx_reverb_std_exp_init(_ctx: &mut HleContext<'_>, _params: u32) -> bool {
    true
}

#[hle_export(lib = "snduser2", name = "AXFXReverbStdExpSettings")]
pub(super) fn axfx_reverb_std_exp_settings(_ctx: &mut HleContext<'_>, _params: u32) -> bool {
    true
}

#[hle_export(lib = "snduser2", name = "AXFXReverbStdExpSettingsUpdate")]
pub(super) fn axfx_reverb_std_exp_settings_update(_ctx: &mut HleContext<'_>, _params: u32) -> bool {
    true
}

#[hle_export(lib = "snduser2", name = "AXFXDelayExpGetMemSize")]
pub(super) fn axfx_delay_exp_get_mem_size(_ctx: &mut HleContext<'_>, _params: u32) -> u32 {
    FX_MEM_SIZE
}

#[hle_export(lib = "snduser2", name = "AXFXDelayExpInit")]
pub(super) fn axfx_delay_exp_init(_ctx: &mut HleContext<'_>, _params: u32) -> bool {
    true
}

#[hle_export(lib = "snduser2", name = "AXFXDelayExpSettings")]
pub(super) fn axfx_delay_exp_settings(_ctx: &mut HleContext<'_>, _params: u32) -> bool {
    true
}

#[hle_export(lib = "snduser2", name = "AXFXDelayExpSettingsUpdate")]
pub(super) fn axfx_delay_exp_settings_update(_ctx: &mut HleContext<'_>, _params: u32) -> bool {
    true
}
