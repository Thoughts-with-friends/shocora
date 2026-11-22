//! `vpad.rpl`: the Wii U GamePad.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ your keyboard/pad  │   │ vpad.rs                  │   │ GamePad state for  │
//! │                    │──►│ ──────────────────────── │──►│ the game           │
//! │                    │   │ GamePad library (VPAD)   │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Reports the GamePad to games: buttons, sticks, touch screen and motion, taken from your PC
//! controls. Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
use wiiu_hle_macros::hle_export;
use wiiu_memory::GuestPtr;
use wiiu_peripherals::Motion;

use crate::hle::{HleContext, HleError, HleExport};

/// `sizeof(VPADStatus)`.
const VPAD_STATUS_SIZE: u32 = 0xAC;

mod offset {
    pub const HOLD: u32 = 0x00;
    pub const TRIGGER: u32 = 0x04;
    pub const RELEASE: u32 = 0x08;
    pub const LEFT_STICK: u32 = 0x0C;
    pub const RIGHT_STICK: u32 = 0x14;
    /// `VPADAccStatus` (`acc`, `magnitude`, `variation`, `vertical`), then `gyro` at 0x38
    /// and `angle` at 0x44.
    pub const ACC: u32 = 0x1C;
    /// Three `VPADTPData`: the raw sample and two filtered ones.
    pub const TOUCH: [u32; 3] = [0x52, 0x5A, 0x62];
    /// `VPADDirection`: the GamePad's X, Y and Z axes.
    pub const DIRECTION: u32 = 0x6C;
    pub const ERROR: u32 = 0x50;
    pub const SLIDE_VOLUME: u32 = 0xA0;
    pub const SLIDE_VOLUME2: u32 = 0xA3;
}

/// `VPAD_READ_ERR_NO_CONTROLLER`.
const READ_ERR_NO_CONTROLLER: i32 = -2;
/// `VPADStatus::hold` bit set while a held button repeats.
const REPEAT: u32 = 0x8000_0000;
/// Stick clamp bounds (`VPAD_MIN_CLAMP`, `VPAD_MAX_CLAMP`).
const MIN_CLAMP: i32 = 0x102;
const MAX_CLAMP: i32 = 0x397;
/// Channels `VPADRead` accepts.
pub const CHANNELS: usize = 2;
/// Ticks per millisecond.
const TICKS_PER_MS: u64 = wiiu_peripherals::timebase::FREQUENCY / 1000;
/// Ticks between sampling callbacks (Cemu's 5 ms alarm).
const SAMPLING_PERIOD: u64 = TICKS_PER_MS * 5;

/// Settings a title makes per GamePad channel (Cemu `vpad::g_vpad` and the
/// `g_vpad*` tables), with Cemu's `SetChannelDefaults`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct VpadChannel {
    /// `VPADSetAccParam`: play radius and sensitivity.
    pub acc_param: (f32, f32),
    pub acc_play_mode: u32,
    /// `VPADSetBtnRepeat`, in milliseconds (Cemu stores `seconds * 200`).
    pub repeat_delay: u32,
    pub repeat_pulse: u32,
    pub sampling_callback: u32,
    /// `VPADTPCalibrationParam`: offset x, y and scale x, y.
    pub tp_calibration: (u16, u16, f32, f32),
    /// Rotation, input range and radius of the left and right stick.
    pub cross_stick: [[f32; 3]; 2],
    pub button_proc_mode: u32,
    pub cross_clamp: bool,
    /// Max and min of the left and right stick clamp.
    pub clamp: [(i32, i32); 2],
    pub lcd_mode: u32,
    pub gyro_direction: [f32; 9],
    pub gyro_zero_drift_mode: u32,
    pub gyro_revise_enabled: bool,
    pub gyro_revise_base: [f32; 9],
    pub gyro_revise_weight: f32,
    /// Held buttons at the last read, when they last changed and the last repeat.
    pub last_hold: u32,
    pub last_hold_change: u64,
    pub last_pulse: u64,
}

impl Default for VpadChannel {
    fn default() -> Self {
        Self {
            acc_param: (0.0, 1.0),
            acc_play_mode: 1,
            repeat_delay: 0,
            repeat_pulse: 0,
            sampling_callback: 0,
            tp_calibration: (92, 254, 1280.0 / 3883.0, 720.0 / 3694.0),
            cross_stick: [[0.0; 3]; 2],
            button_proc_mode: 1,
            cross_clamp: false,
            clamp: [(MAX_CLAMP, MIN_CLAMP); 2],
            lcd_mode: 0xFF,
            gyro_direction: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.1],
            gyro_zero_drift_mode: 1,
            gyro_revise_enabled: false,
            gyro_revise_base: [0.0; 9],
            gyro_revise_weight: 0.0,
            last_hold: 0,
            last_hold_change: 0,
            last_pulse: 0,
        }
    }
}

/// State of `vpad.rpl` across calls.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct VpadState {
    pub channels: [VpadChannel; CHANNELS],
    pub next_sample: u64,
}

impl crate::CafeOs {
    /// Cemu's vpad alarm (every 5 ms): calls the sampling callback of the GamePad.
    pub(crate) fn vpad_tick(&mut self) {
        let now = self.timebase.ticks_since_boot();
        if now < self.vpad.next_sample {
            return;
        }
        self.vpad.next_sample = now + SAMPLING_PERIOD;
        // Only channel 0, the GamePad, is ever connected.
        let callback = self.vpad.channels[0].sampling_callback;
        if callback != 0 {
            self.callbacks
                .push(crate::hle::callback::GuestCall { function: callback, args: vec![0] });
        }
    }
}

fn channel<'a>(ctx: &'a mut HleContext<'_>, channel: u32) -> Option<&'a mut VpadChannel> {
    ctx.os.vpad.channels.get_mut(channel as usize)
}

/// Adds `VPAD_REPEAT` to `hold` as Cemu's `VPADController::VPADRead` does, and
/// returns `(trigger, release)` of the buttons.
fn repeat_and_edges(state: &mut VpadChannel, hold: &mut u32, now: u64) -> (u32, u32) {
    let current = *hold;
    if current != state.last_hold {
        state.last_hold_change = now;
        state.last_pulse = now;
    }
    if state.repeat_pulse > 0
        && current != 0
        && state.last_hold_change + u64::from(state.repeat_delay) * TICKS_PER_MS <= now
        && state.last_pulse + u64::from(state.repeat_pulse) * TICKS_PER_MS < now
    {
        state.last_pulse = now;
        *hold |= REPEAT;
    }
    let release = state.last_hold & !current;
    let trigger = !state.last_hold & current;
    state.last_hold = current;
    (trigger, release)
}

/// Writes the motion fields of a `VPADStatus` at `base`.
fn write_motion(ctx: &mut HleContext<'_>, base: u32, motion: &Motion) -> Result<(), HleError> {
    let mut floats = Vec::with_capacity(27);
    floats.extend(motion.acc);
    floats.extend([motion.acc_magnitude, motion.acc_variation]);
    floats.extend(motion.acc_vertical);
    floats.extend(motion.gyro);
    floats.extend(motion.angle);
    // `acc` through `angle` are contiguous.
    for (index, value) in (0_u32..).zip(floats) {
        ctx.memory.write_f32(base + offset::ACC + index * 4, value)?;
    }
    for (index, value) in (0_u32..).zip(motion.direction.into_iter().flatten()) {
        ctx.memory.write_f32(base + offset::DIRECTION + index * 4, value)?;
    }
    Ok(())
}

/// `VPADTPData::validity`.
const TOUCH_VALID: u16 = 0;
const TOUCH_INVALID: u16 = 3;

/// What the touch panel reports across the screen and where it starts: its Y axis
/// runs upwards. Cemu's `VPADController::update_touch`.
const PANEL_WIDTH: f32 = 3883.0;
const PANEL_HEIGHT: f32 = 3694.0;
const PANEL_LEFT: f32 = 92.0;
const PANEL_TOP: f32 = 4095.0 - 254.0;

/// Screen pixels of the GamePad, which `GamePadState::touch` counts in.
const SCREEN: (f32, f32) = (854.0, 480.0);

/// Screen position to raw panel coordinates.
fn panel_from_screen(x: u16, y: u16) -> (u16, u16) {
    let (x, y) = ((f32::from(x) / SCREEN.0).min(1.0), (f32::from(y) / SCREEN.1).min(1.0));
    (
        f32::mul_add(x, PANEL_WIDTH, PANEL_LEFT) as u16,
        f32::mul_add(y, -PANEL_HEIGHT, PANEL_TOP) as u16,
    )
}

/// Raw panel coordinates to a position on a `width` x `height` screen.
fn screen_from_panel(x: u16, y: u16, width: f32, height: f32) -> (u16, u16) {
    let x = (f32::from(x) - PANEL_LEFT).max(0.0) / PANEL_WIDTH;
    let y = (PANEL_TOP - f32::from(y)).max(0.0) / PANEL_HEIGHT;
    ((x.min(1.0) * width) as u16, (y.min(1.0) * height) as u16)
}

/// Copies `VPADTPData` converted to `width` x `height` from `raw` to `out`.
fn calibrate(
    ctx: &mut HleContext<'_>,
    out: u32,
    raw: u32,
    width: f32,
    height: f32,
) -> Result<(), HleError> {
    let (x, y) = (ctx.memory.read_u16(raw)?, ctx.memory.read_u16(raw + 2)?);
    let (touch, validity) = (ctx.memory.read_u16(raw + 4)?, ctx.memory.read_u16(raw + 6)?);
    let (x, y) = screen_from_panel(x, y, width, height);
    ctx.memory.write_u16(out, x)?;
    ctx.memory.write_u16(out + 2, y)?;
    ctx.memory.write_u16(out + 4, touch)?;
    ctx.memory.write_u16(out + 6, validity)?;
    Ok(())
}

/// `VPAD_READ_SUCCESS`.
const READ_SUCCESS: i32 = 0;

#[hle_export(lib = "vpad", name = "VPADInit")]
fn vpad_init(_ctx: &mut HleContext<'_>) {}

/// Returns the number of samples written (one: the latest state).
#[hle_export(lib = "vpad", name = "VPADRead")]
fn vpad_read(
    ctx: &mut HleContext<'_>,
    channel: u32,
    buffers: GuestPtr<u8>,
    count: u32,
    error: GuestPtr<i32>,
) -> Result<i32, HleError> {
    if buffers.is_null() {
        return Ok(0);
    }
    let base = buffers.address();
    if channel != 0 {
        // Only the GamePad is connected.
        if !error.is_null() {
            error.write(ctx.memory, READ_ERR_NO_CONTROLLER)?;
        }
        if count > 0 {
            ctx.memory.fill(base, VPAD_STATUS_SIZE, 0)?;
            ctx.memory.write_bytes(base + offset::ERROR, &[0xFF])?;
        }
        return Ok(0);
    }
    if count == 0 {
        return Ok(0);
    }
    ctx.memory.fill(base, VPAD_STATUS_SIZE, 0)?;
    let pad = ctx.os.gamepad;
    let mut held = pad.held_with_stick_directions().bits();
    let now = ctx.os.timebase.ticks_since_boot();
    let (trigger, release) = repeat_and_edges(&mut ctx.os.vpad.channels[0], &mut held, now);
    ctx.memory.write_u32(base + offset::HOLD, held)?;
    ctx.memory.write_u32(base + offset::TRIGGER, trigger)?;
    ctx.memory.write_u32(base + offset::RELEASE, release)?;
    // Slide volume at full, battery full (0xC0).
    ctx.memory.write_bytes(base + offset::SLIDE_VOLUME, &[0xFF, 0xC0])?;
    ctx.memory.write_bytes(base + offset::SLIDE_VOLUME2, &[0xFF])?;
    for (at, stick) in
        [(offset::LEFT_STICK, pad.left_stick), (offset::RIGHT_STICK, pad.right_stick)]
    {
        ctx.memory.write_f32(base + at, stick.x)?;
        ctx.memory.write_f32(base + at + 4, stick.y)?;
    }
    write_motion(ctx, base, &pad.motion.unwrap_or(Motion::REST))?;
    // Coordinates stay after the finger leaves: some titles read them then.
    if let Some((x, y)) = pad.touch {
        ctx.os.last_touch = panel_from_screen(x, y);
    }
    let (x, y) = ctx.os.last_touch;
    let (touching, validity) =
        if pad.touch.is_some() { (1, TOUCH_VALID) } else { (0, TOUCH_INVALID) };
    for at in offset::TOUCH {
        ctx.memory.write_u16(base + at, x)?;
        ctx.memory.write_u16(base + at + 2, y)?;
        ctx.memory.write_u16(base + at + 4, touching)?;
        ctx.memory.write_u16(base + at + 6, validity)?;
    }
    if !error.is_null() {
        error.write(ctx.memory, READ_SUCCESS)?;
    }
    Ok(1)
}

/// Raw panel coordinates to the 1280 x 720 titles use by default.
#[hle_export(lib = "vpad", name = "VPADGetTPCalibratedPoint")]
fn vpad_get_tp_calibrated_point(
    ctx: &mut HleContext<'_>,
    _channel: u32,
    out: u32,
    raw: u32,
) -> Result<(), HleError> {
    calibrate(ctx, out, raw, 1280.0, 720.0)
}

/// `VPADTPResolution`: 0 is 1920 x 1080, 1 is 1280 x 720, 2 is 854 x 480.
#[hle_export(lib = "vpad", name = "VPADGetTPCalibratedPointEx")]
fn vpad_get_tp_calibrated_point_ex(
    ctx: &mut HleContext<'_>,
    _channel: u32,
    resolution: u32,
    out: u32,
    raw: u32,
) -> Result<(), HleError> {
    let (width, height) = match resolution {
        0 => (1920.0, 1080.0),
        1 => (1280.0, 720.0),
        _ => SCREEN,
    };
    calibrate(ctx, out, raw, width, height)
}

#[hle_export(lib = "vpad", name = "VPADSetBtnRepeat")]
fn vpad_set_btn_repeat(ctx: &mut HleContext<'_>, ch: u32, delay: f32, pulse: f32) {
    if let Some(c) = channel(ctx, ch) {
        // Cemu keeps `seconds * 200` and reads it as milliseconds.
        if pulse == 0.0 {
            (c.repeat_delay, c.repeat_pulse) = (40000, 0);
        } else {
            c.repeat_delay = delay.mul_add(200.0, 0.5) as u32;
            c.repeat_pulse = pulse.mul_add(200.0, 0.5) as u32;
        }
    }
}

/// Returns the previous callback.
#[hle_export(lib = "vpad", name = "VPADSetSamplingCallback")]
fn vpad_set_sampling_callback(ctx: &mut HleContext<'_>, ch: u32, callback: u32) -> u32 {
    channel(ctx, ch).map_or(0, |c| std::mem::replace(&mut c.sampling_callback, callback))
}

#[hle_export(lib = "vpad", name = "VPADSetAccParam")]
fn vpad_set_acc_param(ctx: &mut HleContext<'_>, ch: u32, radius: f32, sensitivity: f32) {
    if let Some(c) = channel(ctx, ch) {
        c.acc_param = (radius, sensitivity);
    }
}

#[hle_export(lib = "vpad", name = "VPADGetAccParam")]
fn vpad_get_acc_param(
    ctx: &mut HleContext<'_>,
    ch: u32,
    radius: u32,
    sensitivity: u32,
) -> Result<(), HleError> {
    if let Some(&mut VpadChannel { acc_param: (r, s), .. }) = channel(ctx, ch) {
        ctx.memory.write_f32(radius, r)?;
        ctx.memory.write_f32(sensitivity, s)?;
    }
    Ok(())
}

#[hle_export(lib = "vpad", name = "VPADSetAccPlayMode")]
fn vpad_set_acc_play_mode(ctx: &mut HleContext<'_>, ch: u32, mode: u32) {
    if let Some(c) = channel(ctx, ch) {
        c.acc_play_mode = mode;
    }
}

/// `VPAD_PLAY_MODE_TIGHT` for a channel out of range.
#[hle_export(lib = "vpad", name = "VPADGetAccPlayMode")]
fn vpad_get_acc_play_mode(ctx: &mut HleContext<'_>, ch: u32) -> u32 {
    channel(ctx, ch).map_or(1, |c| c.acc_play_mode)
}

#[hle_export(lib = "vpad", name = "VPADEnableStickCrossClamp")]
fn vpad_enable_stick_cross_clamp(ctx: &mut HleContext<'_>, ch: u32) {
    if let Some(c) = channel(ctx, ch) {
        c.cross_clamp = true;
    }
}

#[hle_export(lib = "vpad", name = "VPADDisableStickCrossClamp")]
fn vpad_disable_stick_cross_clamp(ctx: &mut HleContext<'_>, ch: u32) {
    if let Some(c) = channel(ctx, ch) {
        c.cross_clamp = false;
    }
}

fn set_clamp(ctx: &mut HleContext<'_>, ch: u32, stick: usize, max: i32, min: i32) {
    if let Some(c) = channel(ctx, ch) {
        c.clamp[stick] = (max.min(MAX_CLAMP), min.max(MIN_CLAMP));
    }
}

#[allow(clippy::cast_sign_loss)]
fn get_clamp(
    ctx: &mut HleContext<'_>,
    ch: u32,
    stick: usize,
    max: u32,
    min: u32,
) -> Result<(), HleError> {
    if let Some(&mut VpadChannel { clamp, .. }) = channel(ctx, ch) {
        ctx.memory.write_u32(max, clamp[stick].0 as u32)?;
        ctx.memory.write_u32(min, clamp[stick].1 as u32)?;
    }
    Ok(())
}

#[hle_export(lib = "vpad", name = "VPADSetLStickClampThreshold")]
fn vpad_set_l_stick_clamp_threshold(ctx: &mut HleContext<'_>, ch: u32, max: i32, min: i32) {
    set_clamp(ctx, ch, 0, max, min);
}

#[hle_export(lib = "vpad", name = "VPADSetRStickClampThreshold")]
fn vpad_set_r_stick_clamp_threshold(ctx: &mut HleContext<'_>, ch: u32, max: i32, min: i32) {
    set_clamp(ctx, ch, 1, max, min);
}

pub const EXPORTS: &[HleExport] = &[
    VPAD_INIT,
    VPAD_READ,
    VPAD_GET_TP_CALIBRATED_POINT,
    VPAD_GET_TP_CALIBRATED_POINT_EX,
    VPAD_SET_BTN_REPEAT,
    VPAD_SET_SAMPLING_CALLBACK,
    VPAD_SET_ACC_PARAM,
    VPAD_GET_ACC_PARAM,
    VPAD_SET_ACC_PLAY_MODE,
    VPAD_GET_ACC_PLAY_MODE,
    VPAD_ENABLE_STICK_CROSS_CLAMP,
    VPAD_DISABLE_STICK_CROSS_CLAMP,
    VPAD_SET_L_STICK_CLAMP_THRESHOLD,
    VPAD_SET_R_STICK_CLAMP_THRESHOLD,
    VPAD_GET_L_STICK_CLAMP_THRESHOLD,
    VPAD_GET_R_STICK_CLAMP_THRESHOLD,
    VPAD_SET_CROSS_STICK_EMULATION_PARAMS_L,
    VPAD_SET_CROSS_STICK_EMULATION_PARAMS_R,
    VPAD_GET_CROSS_STICK_EMULATION_PARAMS_L,
    VPAD_GET_CROSS_STICK_EMULATION_PARAMS_R,
    VPAD_GET_BUTTON_PROC_MODE,
    VPAD_SET_BUTTON_PROC_MODE,
    VPAD_SET_LCD_MODE,
    VPAD_GET_LCD_MODE,
    VPAD_CONTROL_MOTOR,
    VPAD_STOP_MOTOR,
    VPAD_GET_TP_CALIBRATION_PARAM,
    VPAD_SET_TP_CALIBRATION_PARAM,
    VPAD_SET_GYRO_DIRECTION,
    VPAD_GET_GYRO_ZERO_DRIFT_MODE,
    VPAD_SET_GYRO_ZERO_DRIFT_MODE,
    VPAD_SET_GYRO_DIR_REVISE_BASE,
    VPAD_DISABLE_GYRO_DIR_REVISE,
    VPAD_SET_GYRO_DIR_REVISE_PARAM,
];

#[cfg(test)]
#[path = "vpad_tests.rs"]
mod tests;

mod stick_exports;
#[allow(unused_imports)]
pub(crate) use stick_exports::*;
