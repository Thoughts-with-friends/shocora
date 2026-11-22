//! Part of [`super`]: see its documentation.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ your keyboard/pad  │   │ vpad/stick_exports.rs    │   │ GamePad state for  │
//! │                    │──►│ ──────────────────────── │──►│ the game           │
//! │                    │   │ GamePad library (VPAD):  │   │                    │
//! │                    │   │ stick exports            │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "stick exports" share of vpad.rs (GamePad library (VPAD)), moved here so that no file is
//! longer than 500 lines. It works exactly as if it were still written in that file. Reports
//! the GamePad to games: buttons, sticks, touch screen and motion, taken from your PC controls.
//! Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.

use super::*;

#[hle_export(lib = "vpad", name = "VPADGetLStickClampThreshold")]
pub(super) fn vpad_get_l_stick_clamp_threshold(
    ctx: &mut HleContext<'_>,
    ch: u32,
    max: u32,
    min: u32,
) -> Result<(), HleError> {
    get_clamp(ctx, ch, 0, max, min)
}

#[hle_export(lib = "vpad", name = "VPADGetRStickClampThreshold")]
pub(super) fn vpad_get_r_stick_clamp_threshold(
    ctx: &mut HleContext<'_>,
    ch: u32,
    max: u32,
    min: u32,
) -> Result<(), HleError> {
    get_clamp(ctx, ch, 1, max, min)
}

pub(super) fn set_cross_stick(ctx: &mut HleContext<'_>, ch: u32, stick: usize, params: [f32; 3]) {
    if let Some(c) = channel(ctx, ch) {
        c.cross_stick[stick] = params;
    }
}

pub(super) fn get_cross_stick(
    ctx: &mut HleContext<'_>,
    ch: u32,
    stick: usize,
    out: [u32; 3],
) -> Result<(), HleError> {
    if let Some(c) = channel(ctx, ch) {
        let params = c.cross_stick[stick];
        for (at, value) in out.into_iter().zip(params) {
            ctx.memory.write_f32(at, value)?;
        }
    }
    Ok(())
}

#[hle_export(lib = "vpad", name = "VPADSetCrossStickEmulationParamsL")]
pub(super) fn vpad_set_cross_stick_emulation_params_l(
    ctx: &mut HleContext<'_>,
    ch: u32,
    rotation: f32,
    range: f32,
    radius: f32,
) {
    set_cross_stick(ctx, ch, 0, [rotation, range, radius]);
}

#[hle_export(lib = "vpad", name = "VPADSetCrossStickEmulationParamsR")]
pub(super) fn vpad_set_cross_stick_emulation_params_r(
    ctx: &mut HleContext<'_>,
    ch: u32,
    rotation: f32,
    range: f32,
    radius: f32,
) {
    set_cross_stick(ctx, ch, 1, [rotation, range, radius]);
}

#[hle_export(lib = "vpad", name = "VPADGetCrossStickEmulationParamsL")]
pub(super) fn vpad_get_cross_stick_emulation_params_l(
    ctx: &mut HleContext<'_>,
    ch: u32,
    rotation: u32,
    range: u32,
    radius: u32,
) -> Result<(), HleError> {
    get_cross_stick(ctx, ch, 0, [rotation, range, radius])
}

#[hle_export(lib = "vpad", name = "VPADGetCrossStickEmulationParamsR")]
pub(super) fn vpad_get_cross_stick_emulation_params_r(
    ctx: &mut HleContext<'_>,
    ch: u32,
    rotation: u32,
    range: u32,
    radius: u32,
) -> Result<(), HleError> {
    get_cross_stick(ctx, ch, 1, [rotation, range, radius])
}

/// `VPAD_BUTTON_PROC_MODE_TIGHT` for a channel out of range.
#[hle_export(lib = "vpad", name = "VPADGetButtonProcMode")]
pub(super) fn vpad_get_button_proc_mode(ctx: &mut HleContext<'_>, ch: u32) -> u32 {
    channel(ctx, ch).map_or(1, |c| c.button_proc_mode)
}

#[hle_export(lib = "vpad", name = "VPADSetButtonProcMode")]
pub(super) fn vpad_set_button_proc_mode(ctx: &mut HleContext<'_>, ch: u32, mode: u32) {
    if let Some(c) = channel(ctx, ch) {
        c.button_proc_mode = mode & 0xFF;
    }
}

#[hle_export(lib = "vpad", name = "VPADSetLcdMode")]
pub(super) fn vpad_set_lcd_mode(ctx: &mut HleContext<'_>, ch: u32, mode: u32) {
    if let Some(c) = channel(ctx, ch) {
        c.lcd_mode = mode;
    }
}

#[hle_export(lib = "vpad", name = "VPADGetLcdMode")]
pub(super) fn vpad_get_lcd_mode(
    ctx: &mut HleContext<'_>,
    ch: u32,
    mode: u32,
) -> Result<(), HleError> {
    if let Some(&mut VpadChannel { lcd_mode, .. }) = channel(ctx, ch) {
        ctx.memory.write_u32(mode, lcd_mode)?;
    }
    Ok(())
}

/// The host GamePad has no motor: patterns are accepted (Cemu queues them as rumble).
#[hle_export(lib = "vpad", name = "VPADControlMotor")]
pub(super) fn vpad_control_motor(
    _ctx: &mut HleContext<'_>,
    _ch: u32,
    _pattern: u32,
    _length: u32,
) -> i32 {
    0
}

#[hle_export(lib = "vpad", name = "VPADStopMotor")]
pub(super) fn vpad_stop_motor(_ctx: &mut HleContext<'_>, _ch: u32) {}

/// `VPADTPCalibrationParam`: `u16 x, y; f32 scale_x, scale_y`.
#[hle_export(lib = "vpad", name = "VPADGetTPCalibrationParam")]
pub(super) fn vpad_get_tp_calibration_param(
    ctx: &mut HleContext<'_>,
    ch: u32,
    param: u32,
) -> Result<(), HleError> {
    if let Some(&mut VpadChannel { tp_calibration: (x, y, sx, sy), .. }) = channel(ctx, ch) {
        ctx.memory.write_u16(param, x)?;
        ctx.memory.write_u16(param + 2, y)?;
        ctx.memory.write_f32(param + 4, sx)?;
        ctx.memory.write_f32(param + 8, sy)?;
    }
    Ok(())
}

#[hle_export(lib = "vpad", name = "VPADSetTPCalibrationParam")]
pub(super) fn vpad_set_tp_calibration_param(
    ctx: &mut HleContext<'_>,
    ch: u32,
    param: u32,
) -> Result<(), HleError> {
    let value = (
        ctx.memory.read_u16(param)?,
        ctx.memory.read_u16(param + 2)?,
        ctx.memory.read_f32(param + 4)?,
        ctx.memory.read_f32(param + 8)?,
    );
    if let Some(c) = channel(ctx, ch) {
        c.tp_calibration = value;
    }
    Ok(())
}

/// A `VPADDir`: three axes of three floats.
pub(super) fn read_dir(ctx: &HleContext<'_>, dir: u32) -> Result<[f32; 9], HleError> {
    let mut out = [0.0; 9];
    for (i, value) in (0_u32..).zip(out.iter_mut()) {
        *value = ctx.memory.read_f32(dir + i * 4)?;
    }
    Ok(out)
}

#[hle_export(lib = "vpad", name = "VPADSetGyroDirection")]
pub(super) fn vpad_set_gyro_direction(
    ctx: &mut HleContext<'_>,
    ch: u32,
    dir: u32,
) -> Result<(), HleError> {
    let value = read_dir(ctx, dir)?;
    if let Some(c) = channel(ctx, ch) {
        c.gyro_direction = value;
    }
    Ok(())
}

/// `VPAD_GYRO_ZERODRIFT_NONE` for a channel out of range.
#[hle_export(lib = "vpad", name = "VPADGetGyroZeroDriftMode")]
pub(super) fn vpad_get_gyro_zero_drift_mode(
    ctx: &mut HleContext<'_>,
    ch: u32,
    mode: u32,
) -> Result<(), HleError> {
    let value = channel(ctx, ch).map_or(3, |c| c.gyro_zero_drift_mode);
    ctx.memory.write_u32(mode, value)?;
    Ok(())
}

#[hle_export(lib = "vpad", name = "VPADSetGyroZeroDriftMode")]
pub(super) fn vpad_set_gyro_zero_drift_mode(ctx: &mut HleContext<'_>, ch: u32, mode: u32) {
    if let Some(c) = channel(ctx, ch)
        && mode <= 3
    {
        c.gyro_zero_drift_mode = mode;
    }
}

#[hle_export(lib = "vpad", name = "VPADSetGyroDirReviseBase")]
pub(super) fn vpad_set_gyro_dir_revise_base(
    ctx: &mut HleContext<'_>,
    ch: u32,
    dir: u32,
) -> Result<(), HleError> {
    let value = read_dir(ctx, dir)?;
    if let Some(c) = channel(ctx, ch) {
        c.gyro_revise_base = value;
    }
    Ok(())
}

#[hle_export(lib = "vpad", name = "VPADDisableGyroDirRevise")]
pub(super) fn vpad_disable_gyro_dir_revise(ctx: &mut HleContext<'_>, ch: u32) {
    if let Some(c) = channel(ctx, ch) {
        c.gyro_revise_enabled = false;
    }
}

#[hle_export(lib = "vpad", name = "VPADSetGyroDirReviseParam")]
pub(super) fn vpad_set_gyro_dir_revise_param(ctx: &mut HleContext<'_>, ch: u32, weight: f32) {
    if let Some(c) = channel(ctx, ch) {
        c.gyro_revise_weight = weight;
    }
}
