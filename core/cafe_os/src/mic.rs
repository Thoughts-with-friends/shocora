//! `mic.rpl`: the GamePad's microphone, after Cemu's `mic.cpp`.
//!
//! Nothing is recorded. While the front end holds the MIC button ([`GamePadState::mic`]
//! above zero) the title's ring buffer fills with noise of that loudness, which is what
//! blowing into the microphone sounds like; otherwise with silence. Samples are signed
//! 16-bit big-endian at 32 kHz, and sizes and indices count samples.
//!
//! [`GamePadState::mic`]: wiiu_peripherals::GamePadState::mic
use wiiu_hle_macros::hle_export;
use wiiu_memory::Memory;
use wiiu_peripherals::timebase::FREQUENCY;

use crate::{
    CafeOs,
    hle::{HleContext, HleError, HleExport},
};

pub const SAMPLE_RATE: u32 = 32_000;
/// Peak of a full-strength sample (Cemu's synthetic one peaks there too).
const PEAK: f32 = 30_000.0;

/// `MIC_ERROR_*`.
mod result {
    pub const SUCCESS: i32 = 0;
    pub const BAD_PARAM: i32 = -2;
    pub const ALREADY_OPEN: i32 = -5;
    pub const NOT_OPEN: i32 = -6;
    pub const NOT_INITIALIZED: i32 = -7;
    pub const NOT_CONNECTED: i32 = -8;
    /// More samples consumed than were there.
    pub const OVERRUN: i32 = -81;
}

/// `MICStatus::flags`.
const FORMAT_PCM16: u32 = 1 << 0;
const IS_OPEN: u32 = 1 << 1;
const IS_CONNECTED: u32 = 1 << 2;

/// `MICStateId`.
mod state_id {
    pub const SAMPLE_RATE: u32 = 0;
    pub const GAIN_DB: u32 = 1;
    pub const GAIN_MIN: u32 = 2;
    pub const GAIN_MAX: u32 = 3;
    pub const GAIN_STEP: u32 = 4;
    pub const ECHO_CANCELLATION: u32 = 7;
    pub const AUTO_SELECTION: u32 = 8;
}

/// The handle of the GamePad's microphone; the second GamePad's (1) is never connected.
const DRC0: u32 = 0;

/// The microphone of the first GamePad.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct MicState {
    initialized: bool,
    open: bool,
    /// The title's ring buffer: its samples and how many it holds.
    ring: u32,
    size: u32,
    read: u32,
    write: u32,
    echo_cancellation: u32,
    auto_selection: u32,
    gain_db: u32,
    /// When the microphone was opened, in timebase ticks, and samples made since.
    opened_at: u64,
    made: u64,
    noise: u32,
    /// Guest time the tests ran the microphone for.
    #[cfg(test)]
    #[serde(skip)]
    elapsed_ms: u64,
}

impl MicState {
    /// Samples written and not consumed yet.
    const fn available(&self) -> u32 {
        if self.size == 0 { 0 } else { (self.size + self.write - self.read) % self.size }
    }

    /// The next noise sample in `-1.0..1.0` (xorshift).
    fn next_noise(&mut self) -> f32 {
        let mut x = if self.noise == 0 { 0x2545_F491 } else { self.noise };
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.noise = x;
        (x as i32 as f32) / (i32::MAX as f32)
    }
}

impl CafeOs {
    /// Writes the samples the open microphone made since the last call.
    pub(crate) fn mic_tick(&mut self, memory: &mut Memory) {
        let now = self.timebase.ticks_since_boot();
        self.feed_mic(memory, now);
    }

    /// Writes the samples made up to `now` (timebase ticks).
    fn feed_mic(&mut self, memory: &mut Memory, now: u64) {
        let level = self.gamepad.mic.clamp(0.0, 1.0);
        let mic = &mut self.mic;
        if !mic.open || mic.size == 0 {
            return;
        }
        let due = now.saturating_sub(mic.opened_at) * u64::from(SAMPLE_RATE) / FREQUENCY;
        let count = due.saturating_sub(mic.made);
        mic.made = due;
        // A full ring keeps what the title has not read: the newest samples are lost.
        let room = mic.size - 1 - mic.available();
        for _ in 0..count.min(u64::from(room)) {
            let sample = if level > 0.0 { mic.next_noise() * level * PEAK } else { 0.0 };
            if memory.write_u16(mic.ring + mic.write * 2, sample as i16 as u16).is_err() {
                return;
            }
            mic.write = (mic.write + 1) % mic.size;
        }
    }
}

/// `BAD_PARAM` for a handle that is no microphone, `NOT_INITIALIZED` before `MICInit`.
fn checked(ctx: &HleContext<'_>, handle: u32) -> Result<(), i32> {
    if handle > 1 {
        Err(result::BAD_PARAM)
    } else if handle != DRC0 || !ctx.os.mic.initialized {
        Err(result::NOT_INITIALIZED)
    } else {
        Ok(())
    }
}

/// Returns the handle, or -1 with the reason in `*error`.
#[hle_export(lib = "mic", name = "MICInit")]
fn mic_init(
    ctx: &mut HleContext<'_>,
    handle: u32,
    _unknown: u32,
    ring: u32,
    error: u32,
) -> Result<i32, HleError> {
    let reason = if handle != DRC0 {
        result::NOT_CONNECTED
    } else if ctx.os.mic.initialized {
        result::ALREADY_OPEN
    } else {
        let size = ctx.memory.read_u32(ring)?;
        let samples = ctx.memory.read_u32(ring + 4)?;
        ctx.os.mic = MicState {
            initialized: true,
            ring: samples,
            size,
            echo_cancellation: 1,
            auto_selection: 1,
            ..MicState::default()
        };
        result::SUCCESS
    };
    if error != 0 {
        ctx.memory.write_u32(error, reason as u32)?;
    }
    Ok(if reason == result::SUCCESS { handle as i32 } else { -1 })
}

#[hle_export(lib = "mic", name = "MICUninit")]
fn mic_uninit(ctx: &mut HleContext<'_>, handle: u32) -> i32 {
    if let Err(error) = checked(ctx, handle) {
        return error;
    }
    ctx.os.mic = MicState::default();
    result::SUCCESS
}

#[hle_export(lib = "mic", name = "MICOpen")]
fn mic_open(ctx: &mut HleContext<'_>, handle: u32) -> i32 {
    if let Err(error) = checked(ctx, handle) {
        return error;
    }
    if ctx.os.mic.open {
        return result::ALREADY_OPEN;
    }
    let now = ctx.os.timebase.ticks_since_boot();
    let mic = &mut ctx.os.mic;
    mic.open = true;
    mic.opened_at = now;
    mic.made = 0;
    result::SUCCESS
}

#[hle_export(lib = "mic", name = "MICClose")]
fn mic_close(ctx: &mut HleContext<'_>, handle: u32) -> i32 {
    if let Err(error) = checked(ctx, handle) {
        return error;
    }
    if !ctx.os.mic.open {
        return result::NOT_OPEN;
    }
    ctx.os.mic.open = false;
    result::SUCCESS
}

/// Fills `MICStatus`: flags, samples available, read index.
#[hle_export(lib = "mic", name = "MICGetStatus")]
fn mic_get_status(ctx: &mut HleContext<'_>, handle: u32, status: u32) -> Result<i32, HleError> {
    if let Err(error) = checked(ctx, handle) {
        return Ok(error);
    }
    let mic = &ctx.os.mic;
    let flags = FORMAT_PCM16 | IS_CONNECTED | if mic.open { IS_OPEN } else { 0 };
    let (available, read) = (mic.available(), mic.read);
    ctx.memory.write_u32(status, flags)?;
    ctx.memory.write_u32(status + 4, available)?;
    ctx.memory.write_u32(status + 8, read)?;
    Ok(result::SUCCESS)
}

#[hle_export(lib = "mic", name = "MICGetState")]
fn mic_get_state(
    ctx: &mut HleContext<'_>,
    handle: u32,
    id: u32,
    value: u32,
) -> Result<i32, HleError> {
    if let Err(error) = checked(ctx, handle) {
        return Ok(error);
    }
    let mic = &ctx.os.mic;
    // Gains are S7.8 dB; their range is Cemu's guess.
    let state = match id {
        state_id::SAMPLE_RATE => SAMPLE_RATE,
        state_id::GAIN_DB => mic.gain_db,
        state_id::GAIN_MIN => 0,
        state_id::GAIN_MAX => 0x200,
        state_id::GAIN_STEP => 1,
        state_id::ECHO_CANCELLATION => mic.echo_cancellation,
        state_id::AUTO_SELECTION => mic.auto_selection,
        _ => {
            log::debug!("MICGetState: unknown state {id}");
            0
        }
    };
    ctx.memory.write_u32(value, state)?;
    Ok(result::SUCCESS)
}

#[hle_export(lib = "mic", name = "MICSetState")]
fn mic_set_state(ctx: &mut HleContext<'_>, handle: u32, id: u32, value: u32) -> i32 {
    if let Err(error) = checked(ctx, handle) {
        return error;
    }
    let mic = &mut ctx.os.mic;
    match id {
        state_id::ECHO_CANCELLATION => mic.echo_cancellation = u32::from(value != 0),
        state_id::AUTO_SELECTION => mic.auto_selection = u32::from(value != 0),
        state_id::GAIN_DB => mic.gain_db = value,
        _ => log::debug!("MICSetState: unknown state {id} = {value}"),
    }
    result::SUCCESS
}

/// The title read `count` samples from the read index on.
#[hle_export(lib = "mic", name = "MICSetDataConsumed")]
fn mic_set_data_consumed(ctx: &mut HleContext<'_>, handle: u32, count: u32) -> i32 {
    if let Err(error) = checked(ctx, handle) {
        return error;
    }
    let mic = &mut ctx.os.mic;
    if !mic.open {
        return result::NOT_OPEN;
    }
    if count > mic.available() {
        mic.read = mic.write;
        return result::OVERRUN;
    }
    mic.read = (mic.read + count) % mic.size;
    result::SUCCESS
}

pub const EXPORTS: &[HleExport] = &[
    MIC_INIT,
    MIC_UNINIT,
    MIC_OPEN,
    MIC_CLOSE,
    MIC_GET_STATUS,
    MIC_GET_STATE,
    MIC_SET_STATE,
    MIC_SET_DATA_CONSUMED,
];

#[cfg(test)]
mod tests {
    use super::{FREQUENCY, SAMPLE_RATE, result};
    use crate::hle::test_env::{SCRATCH, TestEnv};

    const RING: u32 = SCRATCH + 0x100;
    const SAMPLES: u32 = SCRATCH + 0x1000;
    const STATUS: u32 = SCRATCH + 0x200;
    const SIZE: u32 = 0x800;

    fn opened() -> TestEnv {
        let mut env = TestEnv::new();
        env.memory.write_u32(RING, SIZE).unwrap();
        env.memory.write_u32(RING + 4, SAMPLES).unwrap();
        assert_eq!(env.call("mic", "MICInit", &[0, 0, RING, SCRATCH]), 0);
        assert_eq!(env.memory.read_u32(SCRATCH).unwrap(), 0);
        assert_eq!(env.call("mic", "MICOpen", &[0]), 0);
        env
    }

    /// Runs the microphone for `ms` milliseconds of guest time and returns the samples
    /// available.
    fn run(env: &mut TestEnv, ms: u64) -> u32 {
        env.os.mic.elapsed_ms += ms;
        let now = env.os.mic.opened_at + (FREQUENCY * env.os.mic.elapsed_ms).div_ceil(1000);
        env.os.feed_mic(&mut env.memory, now);
        env.call("mic", "MICGetStatus", &[0, STATUS]);
        env.memory.read_u32(STATUS + 4).unwrap()
    }

    fn loudest(env: &TestEnv, count: u32) -> i16 {
        (0..count)
            .map(|i| env.memory.read_u16(SAMPLES + i * 2).unwrap() as i16)
            .map(i16::saturating_abs)
            .max()
            .unwrap_or(0)
    }

    /// Intent: an open microphone makes 32 kHz of samples, silent unless MIC is held,
    /// loud while it is.
    /// Pass: 10 ms gives 320 samples of zeros; with `mic` at 1.0 the next ones are loud.
    #[test]
    fn samples_are_silent_until_mic_is_held() {
        let mut env = opened();
        assert_eq!(run(&mut env, 10), SAMPLE_RATE / 100);
        assert_eq!(loudest(&env, 320), 0);
        let flags = env.memory.read_u32(STATUS).unwrap();
        assert_eq!(flags, 0b111, "PCM16 | open | connected");

        env.os.gamepad.mic = 1.0;
        assert_eq!(run(&mut env, 10), 640);
        let loud = (320..640).map(|i| env.memory.read_u16(SAMPLES + i * 2).unwrap() as i16);
        assert!(loud.map(i16::saturating_abs).max().unwrap() > 10_000);
    }

    /// Intent: consuming moves the read index, too much empties the ring with the error
    /// code titles expect, and a full ring stops short of overwriting unread samples.
    /// Pass: available drops by the count; overrun returns -81; never more than size - 1.
    #[test]
    fn consuming_and_overflow() {
        let mut env = opened();
        assert_eq!(run(&mut env, 10), 320);
        assert_eq!(env.call("mic", "MICSetDataConsumed", &[0, 100]), 0);
        assert_eq!(run(&mut env, 0), 220);
        assert_eq!(env.call("mic", "MICSetDataConsumed", &[0, 1000]) as i32, result::OVERRUN);
        assert_eq!(run(&mut env, 0), 0);
        assert_eq!(run(&mut env, 1000), SIZE - 1);
    }

    /// Intent: the calls refuse what Cemu refuses.
    /// Pass: a second init, the second GamePad and an unknown handle fail with their codes.
    #[test]
    fn bad_handles_and_double_init() {
        let mut env = opened();
        assert_eq!(env.call("mic", "MICInit", &[0, 0, RING, SCRATCH]) as i32, -1);
        assert_eq!(env.memory.read_u32(SCRATCH).unwrap() as i32, result::ALREADY_OPEN);
        assert_eq!(env.call("mic", "MICInit", &[1, 0, RING, SCRATCH]) as i32, -1);
        assert_eq!(env.memory.read_u32(SCRATCH).unwrap() as i32, result::NOT_CONNECTED);
        assert_eq!(env.call("mic", "MICOpen", &[0]) as i32, result::ALREADY_OPEN);
        assert_eq!(env.call("mic", "MICOpen", &[5]) as i32, result::BAD_PARAM);
        assert_eq!(env.call("mic", "MICClose", &[0]), 0);
        assert_eq!(env.call("mic", "MICSetDataConsumed", &[0, 1]) as i32, result::NOT_OPEN);
    }
}
