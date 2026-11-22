//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!                                   ▲▲▲▲▲  here
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ sndcore2_tests.rs        │   │ pass / fail        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tests of sndcore2.rs     │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for sndcore2.rs (AX audio library). They run with `cargo test` only and are not
//! part of the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

/// Intent: the low-pass coefficients pass sound through (Cemu's formula): the higher
/// the cutoff, the more passes; none of them is zero.
#[test]
fn lpf_coefficients_pass_sound() {
    let (low_a, low_b) = super::lpf_coefficients(500);
    let (high_a, high_b) = super::lpf_coefficients(15000);
    assert!(low_a > 0 && high_a > low_a, "{low_a} {high_a}");
    assert!(usize::from(low_a) + usize::from(low_b) <= 0x8000);
    assert!(high_b < low_b);
}
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

/// Intent: once the callback queue is full, final mix callbacks stop being queued too,
/// so the queue can drain and frame callbacks (which feed streamed voices) resume.
/// Pass: mixing frames with a full queue leaves its length unchanged.
/// Intent: a pause shorter than `MAX_BEHIND` is made up for over the next ticks instead
/// of dropped (the host heard every dropped stretch as a click); a longer one is skipped.
/// Pass: 50 frames behind mix as 16, 16, 16, 2 with none skipped; 400 behind skip all
/// but `MAX_BEHIND` and mix 16 of those.
#[test]
fn short_pauses_are_made_up_for_and_long_ones_skipped() {
    let mut behind = 50;
    let mut mixed = Vec::new();
    while behind > 0 {
        let (skipped, now) = super::catch_up(behind);
        assert_eq!(skipped, 0);
        mixed.push(now);
        behind -= now;
    }
    assert_eq!(mixed, [16, 16, 16, 2]);
    assert_eq!(super::catch_up(400), (400 - super::MAX_BEHIND, super::MAX_CATCH_UP));
}

#[test]
fn a_full_callback_queue_does_not_grow_with_final_mix_callbacks() {
    let mut env = TestEnv::new();
    env.call("sndcore2", "AXInit", &[]);
    env.call("sndcore2", "AXRegisterDeviceFinalMixCallback", &[0, 0x1000]);
    for _ in 0..super::MAX_PENDING_CALLBACKS {
        env.os
            .callbacks
            .push(crate::hle::callback::GuestCall { function: 0x2000, args: Vec::new() });
    }
    for _ in 0..8 {
        env.os.mix_frame(&mut env.memory);
    }
    assert_eq!(env.os.callbacks.pending(), super::MAX_PENDING_CALLBACKS);
}

/// Intent: an `Ex` offset in another buffer points there without moving what plays now.
/// Pass: the loop lands at the second buffer's address, the current position keeps its
/// address, and offsets read back relative to either buffer.
#[test]
fn ex_offsets_address_other_buffers() {
    let (a, b) = (0x1000_0100_u32, 0x1000_0900_u32);
    let mut voice = super::Voice::new(super::SampleFormat::Adpcm, a, 0x200);
    voice.current_offset = 0x40;
    let loop_offset = super::rebase(&mut voice, 2, b);
    assert_eq!(voice.base_address + loop_offset / 2, b + 1);
    assert_eq!(voice.base_address + voice.current_offset / 2, a + 0x20);
    // A buffer below the base: the base moves, every address stays.
    let below = 0x1000_0000_u32;
    let end = super::rebase(&mut voice, 0x10, below);
    assert_eq!(voice.base_address, below);
    assert_eq!(voice.base_address + voice.current_offset / 2, a + 0x20);
    assert_eq!(voice.base_address + end / 2, below + 8);
    let back = super::units_in(voice.format, a - voice.base_address);
    assert_eq!(voice.current_offset - back, 0x40);
}
