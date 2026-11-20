//! `padscore.rpl`: WPAD and KPAD, the Wii Remote / Pro Controller libraries.
//! Port of Cemu's padscore and of its `WPADController` reads.
//!
//! Channels hold the Pro Controllers the frontend connects
//! (`CafeOs::pro_controllers`). Connection, extension and sampling callbacks run
//! from a 5 ms tick, as Cemu's padscore alarm does.
use wiiu_hle_macros::hle_export;
use wiiu_peripherals::{GamePadState, VpadButtons};

use crate::{
    CafeOs,
    hle::{HleContext, HleError, HleExport, callback::GuestCall},
};

/// Channels WPAD serves (`kWPADMaxControllers`).
pub const PRO_CHANNELS: usize = 4;
/// `kKPADMaxControllers`.
const KPAD_MAX_CONTROLLERS: u32 = 7;

/// `WPAD_ERR_*`.
mod wpad_err {
    pub const NONE: i32 = 0;
    pub const NO_CONTROLLER: i32 = -1;
}
/// `KPAD_ERROR`.
mod kpad_err {
    pub const NONE: i32 = 0;
    pub const NO_SAMPLE_DATA: i32 = -1;
    pub const NO_CONTROLLER: i32 = -2;
}
/// `WPADBatteryLevel::FULL`.
const BATTERY_FULL: u32 = 4;
/// `WPADLed::CHAN0`.
const LED_CHAN0: u8 = 1;
/// What `WPADProbe` reports for an empty channel.
const DEV_NOT_FOUND: u32 = 253;
/// `kWAPDevURCC`: the Wii U Pro Controller.
const DEV_PRO: u8 = 31;
/// `kWPADButtonRepeat`.
const BUTTON_REPEAT: u32 = 0x8000_0000;
/// `KPADGetMplsWorkSize`.
const KPAD_MPLS_WORK_SIZE: u32 = 0x5FE0;
/// A controller delivers a new sample at most this often; titles read until there is none.
const KPAD_SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1);
/// Period of the connection/sampling tick.
const TICK_PERIOD: u64 = wiiu_peripherals::timebase::FREQUENCY / 200;

/// `WPADDataFormat`.
pub mod format {
    pub const CORE: u32 = 0;
    pub const CORE_ACC: u32 = 1;
    pub const CORE_ACC_DPD: u32 = 2;
    pub const FREESTYLE: u32 = 3;
    pub const FREESTYLE_ACC: u32 = 4;
    pub const FREESTYLE_ACC_DPD: u32 = 5;
    pub const CLASSIC: u32 = 6;
    pub const CLASSIC_ACC: u32 = 7;
    pub const CLASSIC_ACC_DPD: u32 = 8;
    pub const CORE_ACC_DPD_FULL: u32 = 9;
    pub const TRAIN: u32 = 10;
    pub const GUITAR: u32 = 11;
    pub const BALANCE_CHECKER: u32 = 12;
    pub const DRUM: u32 = 15;
    pub const MPLS: u32 = 16;
    pub const TAIKO: u32 = 17;
    pub const URCC: u32 = 22;
}

/// `sizeof(KPADStatus)`.
const KPAD_STATUS_SIZE: u32 = 0xF0;
/// `sizeof(KPADUnifiedWpadStatus)`.
const UNIFIED_STATUS_SIZE: u32 = 0x44;

/// Offsets inside `KPADStatus`.
pub(crate) mod kpad {
    pub const DEVICE: u32 = 0x5C;
    pub const ERROR: u32 = 0x5D;
    pub const DPD_VALID: u32 = 0x5E;
    pub const FORMAT: u32 = 0x5F;
    pub const PRO_HOLD: u32 = 0x60;
    pub const PRO_TRIGGER: u32 = 0x64;
    pub const PRO_RELEASE: u32 = 0x68;
    pub const PRO_LEFT_STICK: u32 = 0x6C;
    pub const PRO_RIGHT_STICK: u32 = 0x74;
    pub const PRO_CHARGE: u32 = 0x7C;
    /// Wired: the battery never runs out.
    pub const PRO_CABLE: u32 = 0x80;
}

/// Offsets inside `WPADStatus` and its extended forms.
mod wpad {
    pub const BUTTON: u32 = 0x0;
    pub const DEVICE: u32 = 0x28;
    pub const ERROR: u32 = 0x29;
    /// `WPADStatus` alone.
    pub const CORE_SIZE: u32 = 0x2A;
    pub const FS_STICK_X: u32 = 0x30;
    pub const FS_STICK_Y: u32 = 0x31;
    pub const FS_SIZE: u32 = 0x32;
    pub const CL_BUTTON: u32 = 0x2A;
    pub const CL_LEFT_STICK: u32 = 0x2C;
    pub const CL_RIGHT_STICK: u32 = 0x30;
    pub const CL_SIZE: u32 = 0x36;
    pub const MP_STAT: u32 = 0x36;
    pub const UC_BUTTON: u32 = 0x2C;
    pub const UC_LEFT_STICK: u32 = 0x30;
    pub const UC_RIGHT_STICK: u32 = 0x34;
    pub const UC_CHARGE: u32 = 0x38;
    pub const UC_CABLE: u32 = 0x3C;
    pub const UC_SIZE: u32 = 0x40;
}

/// `WPADInfo` fields.
mod info {
    pub const BATTERY: u32 = 0x14;
    pub const LED: u32 = 0x15;
    pub const SIZE: u32 = 0x16;
}

/// Per-channel state of padscore (`g_padscore.controller_data`).
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Channel {
    pub extension_callback: u32,
    pub connect_callback: u32,
    pub sampling_callback: u32,
    pub dpd_callback: u32,
    pub dpd_enabled: bool,
    pub disconnect_called: bool,
    /// Data format set by `WPADSetDataFormat`.
    pub format: u32,
    /// `KPADSetBtnRepeat` delay and pulse, in milliseconds.
    pub repeat_delay_ms: u32,
    pub repeat_pulse_ms: u32,
    /// Last KPAD hold value and when it changed, for the edges and button repeat.
    pub last_hold: u32,
    pub hold_changed_tick: u64,
    pub last_pulse_tick: u64,
    /// Connection state already reported to the connect callback.
    pub reported_connected: bool,
    /// Device reported to the extension callback.
    pub reported_extension: bool,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            extension_callback: 0,
            connect_callback: 0,
            sampling_callback: 0,
            dpd_callback: 0,
            dpd_enabled: true,
            disconnect_called: false,
            format: format::URCC,
            repeat_delay_ms: 0,
            repeat_pulse_ms: 0,
            last_hold: 0,
            hold_changed_tick: 0,
            last_pulse_tick: 0,
            reported_connected: false,
            reported_extension: false,
        }
    }
}

/// `WPADState_t`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WpadState {
    #[default]
    Master,
    Shutdown,
    Initializing,
    Acquired,
    Released,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PadscoreState {
    pub channels: [Channel; PRO_CHANNELS],
    pub kpad_initialized: bool,
    pub ring_buffer: u32,
    pub ring_buffer_length: u32,
    pub callback_by_kpad: bool,
    pub max_controllers: u32,
    pub wpad_state: WpadState,
    /// Timebase tick of the next connection/sampling tick.
    pub next_tick: u64,
}

impl Default for PadscoreState {
    fn default() -> Self {
        Self {
            channels: [Channel::default(); PRO_CHANNELS],
            kpad_initialized: false,
            ring_buffer: 0,
            ring_buffer_length: 0,
            callback_by_kpad: false,
            max_controllers: PRO_CHANNELS as u32,
            wpad_state: WpadState::Master,
            next_tick: 0,
        }
    }
}

/// GamePad buttons to the bits of the Pro Controller (`kProButton_*`).
pub(crate) fn pro_buttons(held: VpadButtons) -> u32 {
    const MAP: [(VpadButtons, u32); 17] = [
        (VpadButtons::UP, 0x1),
        (VpadButtons::LEFT, 0x2),
        (VpadButtons::ZR, 0x4),
        (VpadButtons::X, 0x8),
        (VpadButtons::A, 0x10),
        (VpadButtons::Y, 0x20),
        (VpadButtons::B, 0x40),
        (VpadButtons::ZL, 0x80),
        (VpadButtons::R, 0x200),
        (VpadButtons::PLUS, 0x400),
        (VpadButtons::HOME, 0x800),
        (VpadButtons::MINUS, 0x1000),
        (VpadButtons::L, 0x2000),
        (VpadButtons::DOWN, 0x4000),
        (VpadButtons::RIGHT, 0x8000),
        (VpadButtons::STICK_R, 0x1_0000),
        (VpadButtons::STICK_L, 0x2_0000),
    ];
    map_buttons(held, &MAP)
}

/// GamePad buttons to the Wii Remote's (`kWPADButton_*`), for the core formats.
fn core_buttons(held: VpadButtons) -> u32 {
    const MAP: [(VpadButtons, u32); 11] = [
        (VpadButtons::LEFT, 0x1),
        (VpadButtons::RIGHT, 0x2),
        (VpadButtons::DOWN, 0x4),
        (VpadButtons::UP, 0x8),
        (VpadButtons::PLUS, 0x10),
        (VpadButtons::Y, 0x100),
        (VpadButtons::X, 0x200),
        (VpadButtons::B, 0x400),
        (VpadButtons::A, 0x800),
        (VpadButtons::MINUS, 0x1000),
        (VpadButtons::HOME, 0x8000),
    ];
    map_buttons(held, &MAP)
}

fn map_buttons(held: VpadButtons, map: &[(VpadButtons, u32)]) -> u32 {
    map.iter().filter(|(button, _)| held.contains(*button)).fold(0, |bits, (_, bit)| bits | bit)
}

/// Buttons of `pad` in the layout of `format`.
fn buttons_for_format(format: u32, held: VpadButtons) -> u32 {
    match format {
        format::CLASSIC | format::CLASSIC_ACC | format::CLASSIC_ACC_DPD => {
            pro_buttons(held) & 0xFFFF
        }
        format::URCC => pro_buttons(held),
        format::CORE
        | format::CORE_ACC
        | format::CORE_ACC_DPD
        | format::CORE_ACC_DPD_FULL
        | format::FREESTYLE
        | format::FREESTYLE_ACC
        | format::FREESTYLE_ACC_DPD
        | format::MPLS => core_buttons(held),
        _ => 0,
    }
}

/// A stick axis as the 12-bit-scaled value of the extension reports (`axis * 2048`).
#[allow(clippy::cast_possible_truncation)]
fn axis_u16(value: f32) -> u16 {
    ((value * 2048.0) as i32) as u16
}

fn controller(ctx: &HleContext<'_>, channel: u32) -> Option<GamePadState> {
    ctx.os.pro_controllers.get(channel as usize).copied().flatten()
}

fn channel_mut<'a>(ctx: &'a mut HleContext<'_>, channel: u32) -> Option<&'a mut Channel> {
    ctx.os.padscore.channels.get_mut(channel as usize)
}

// ---- WPAD ------------------------------------------------------------------

#[hle_export(lib = "padscore", name = "WPADGetStatus")]
const fn wpad_get_status(_ctx: &mut HleContext<'_>) -> u32 {
    1
}

#[hle_export(lib = "padscore", name = "WPADGetBatteryLevel")]
const fn wpad_get_battery_level(_ctx: &mut HleContext<'_>, _channel: u32) -> u32 {
    BATTERY_FULL
}

#[hle_export(lib = "padscore", name = "WPADProbe")]
fn wpad_probe(ctx: &mut HleContext<'_>, channel: u32, device: u32) -> Result<i32, HleError> {
    let connected = controller(ctx, channel).is_some();
    if device != 0 {
        let kind = if connected { u32::from(DEV_PRO) } else { DEV_NOT_FOUND };
        ctx.memory.write_u32(device, kind)?;
    }
    Ok(if connected { wpad_err::NONE } else { wpad_err::NO_CONTROLLER })
}

fn write_info(ctx: &mut HleContext<'_>, out: u32) -> Result<(), HleError> {
    ctx.memory.fill(out, info::SIZE, 0)?;
    ctx.memory.write_u8(out + info::BATTERY, BATTERY_FULL as u8)?;
    ctx.memory.write_u8(out + info::LED, LED_CHAN0)?;
    Ok(())
}

#[hle_export(lib = "padscore", name = "WPADGetInfo")]
fn wpad_get_info(ctx: &mut HleContext<'_>, channel: u32, out: u32) -> Result<i32, HleError> {
    if controller(ctx, channel).is_none() {
        return Ok(wpad_err::NO_CONTROLLER);
    }
    write_info(ctx, out)?;
    Ok(wpad_err::NONE)
}

#[hle_export(lib = "padscore", name = "WPADGetInfoAsync")]
fn wpad_get_info_async(
    ctx: &mut HleContext<'_>,
    channel: u32,
    out: u32,
    callback: u32,
) -> Result<i32, HleError> {
    let result =
        if controller(ctx, channel).is_some() { wpad_err::NONE } else { wpad_err::NO_CONTROLLER };
    if result == wpad_err::NONE {
        write_info(ctx, out)?;
    }
    if callback != 0 {
        let error = if result == wpad_err::NONE { kpad_err::NONE } else { kpad_err::NO_CONTROLLER };
        ctx.os.callbacks.push(GuestCall { function: callback, args: vec![channel, error as u32] });
    }
    Ok(result)
}

/// Fills a `WPADStatus` of `channel`'s data format (`WPADController::WPADRead`).
fn write_wpad_status(
    ctx: &mut HleContext<'_>,
    channel: u32,
    pad: GamePadState,
    out: u32,
) -> Result<(), HleError> {
    let format = ctx.os.padscore.channels[channel as usize].format;
    let button = buttons_for_format(format, pad.held);
    #[allow(clippy::cast_possible_truncation)]
    match format {
        format::FREESTYLE | format::FREESTYLE_ACC | format::FREESTYLE_ACC_DPD => {
            ctx.memory.fill(out, wpad::FS_SIZE, 0)?;
            ctx.memory.write_u16(out + wpad::BUTTON, button as u16)?;
            ctx.memory.write_u8(out + wpad::FS_STICK_X, (pad.left_stick.x * 127.0) as i8 as u8)?;
            ctx.memory.write_u8(out + wpad::FS_STICK_Y, (pad.left_stick.y * 127.0) as i8 as u8)?;
        }
        format::CLASSIC
        | format::CLASSIC_ACC
        | format::CLASSIC_ACC_DPD
        | format::GUITAR
        | format::DRUM
        | format::TAIKO => {
            ctx.memory.fill(out, wpad::CL_SIZE, 0)?;
            ctx.memory.write_u16(out + wpad::CL_BUTTON, button as u16)?;
            let at = out + wpad::CL_LEFT_STICK;
            ctx.memory.write_u16(at, axis_u16(pad.left_stick.x))?;
            ctx.memory.write_u16(at + 2, axis_u16(pad.left_stick.y))?;
            let at = out + wpad::CL_RIGHT_STICK;
            ctx.memory.write_u16(at, axis_u16(pad.right_stick.x))?;
            ctx.memory.write_u16(at + 2, axis_u16(pad.right_stick.y))?;
        }
        format::TRAIN | format::BALANCE_CHECKER => {}
        format::MPLS => ctx.memory.write_u8(out + wpad::MP_STAT, 1)?,
        format::URCC => {
            ctx.memory.fill(out, wpad::UC_SIZE, 0)?;
            ctx.memory.write_u32(out + wpad::UC_BUTTON, button)?;
            ctx.memory.write_u32(out + wpad::UC_CABLE, 1)?;
            ctx.memory.write_u32(out + wpad::UC_CHARGE, 1)?;
            let at = out + wpad::UC_LEFT_STICK;
            ctx.memory.write_u16(at, axis_u16(pad.left_stick.x))?;
            ctx.memory.write_u16(at + 2, axis_u16(pad.left_stick.y))?;
            let at = out + wpad::UC_RIGHT_STICK;
            ctx.memory.write_u16(at, axis_u16(pad.right_stick.x))?;
            ctx.memory.write_u16(at + 2, axis_u16(pad.right_stick.y))?;
        }
        _ => {
            ctx.memory.fill(out, wpad::CORE_SIZE, 0)?;
            ctx.memory.write_u16(out + wpad::BUTTON, button as u16)?;
        }
    }
    ctx.memory.write_u8(out + wpad::DEVICE, DEV_PRO)?;
    ctx.memory.write_u8(out + wpad::ERROR, 0)?;
    Ok(())
}

/// Cemu returns `WPAD_ERR_NO_CONTROLLER` even after filling the status.
#[hle_export(lib = "padscore", name = "WPADRead")]
fn wpad_read(ctx: &mut HleContext<'_>, channel: u32, out: u32) -> Result<i32, HleError> {
    if let Some(pad) = controller(ctx, channel)
        && out != 0
    {
        write_wpad_status(ctx, channel, pad, out)?;
    }
    Ok(wpad_err::NO_CONTROLLER)
}

#[hle_export(lib = "padscore", name = "WPADSetDataFormat")]
fn wpad_set_data_format(ctx: &mut HleContext<'_>, channel: u32, format: u32) -> u32 {
    if controller(ctx, channel).is_some()
        && let Some(state) = channel_mut(ctx, channel)
    {
        state.format = format;
    }
    0
}

#[hle_export(lib = "padscore", name = "WPADGetDataFormat")]
fn wpad_get_data_format(ctx: &mut HleContext<'_>, channel: u32) -> u32 {
    if controller(ctx, channel).is_some() {
        ctx.os.padscore.channels[channel as usize].format
    } else {
        format::CORE
    }
}

#[hle_export(lib = "padscore", name = "WPADIsMotorEnabled")]
const fn wpad_is_motor_enabled(_ctx: &mut HleContext<'_>) -> bool {
    true
}

/// Rumble is not forwarded to host controllers.
#[hle_export(lib = "padscore", name = "WPADControlMotor")]
const fn wpad_control_motor(_ctx: &mut HleContext<'_>, _channel: u32, _command: u32) -> u32 {
    0
}

#[hle_export(lib = "padscore", name = "WPADControlDpd")]
fn wpad_control_dpd(ctx: &mut HleContext<'_>, channel: u32, _command: u32, callback: u32) -> i32 {
    if controller(ctx, channel).is_none() {
        return wpad_err::NO_CONTROLLER;
    }
    ctx.os.padscore.channels[channel as usize].dpd_callback = callback;
    if callback != 0 {
        ctx.os.callbacks.push(GuestCall { function: callback, args: vec![channel, 0] });
    }
    wpad_err::NONE
}

/// Replaces one callback of `channel` and returns the previous one.
fn swap_callback(
    ctx: &mut HleContext<'_>,
    channel: u32,
    callback: u32,
    field: fn(&mut Channel) -> &mut u32,
) -> u32 {
    channel_mut(ctx, channel).map_or(0, |state| std::mem::replace(field(state), callback))
}

#[hle_export(lib = "padscore", name = "WPADSetExtensionCallback")]
fn wpad_set_extension_callback(ctx: &mut HleContext<'_>, channel: u32, callback: u32) -> u32 {
    swap_callback(ctx, channel, callback, |c| &mut c.extension_callback)
}

#[hle_export(lib = "padscore", name = "KPADSetConnectCallback")]
fn kpad_set_connect_callback(ctx: &mut HleContext<'_>, channel: u32, callback: u32) -> u32 {
    swap_callback(ctx, channel, callback, |c| &mut c.connect_callback)
}

#[hle_export(lib = "padscore", name = "WPADSetConnectCallback")]
fn wpad_set_connect_callback(ctx: &mut HleContext<'_>, channel: u32, callback: u32) -> u32 {
    swap_callback(ctx, channel, callback, |c| &mut c.connect_callback)
}

#[hle_export(lib = "padscore", name = "KPADSetSamplingCallback")]
fn kpad_set_sampling_callback(ctx: &mut HleContext<'_>, channel: u32, callback: u32) -> u32 {
    swap_callback(ctx, channel, callback, |c| &mut c.sampling_callback)
}

#[hle_export(lib = "padscore", name = "WPADSetSamplingCallback")]
fn wpad_set_sampling_callback(ctx: &mut HleContext<'_>, channel: u32, callback: u32) -> u32 {
    swap_callback(ctx, channel, callback, |c| &mut c.sampling_callback)
}

#[hle_export(lib = "padscore", name = "WPADSetCallbackByKPAD")]
fn wpad_set_callback_by_kpad(ctx: &mut HleContext<'_>, state: u32) -> u32 {
    ctx.os.padscore.callback_by_kpad = state != 0;
    0
}

/// Reports whether MotionPlus is attached (never, for a Pro Controller) and calls the
/// optional callback with `(channel, status)` before returning.
#[hle_export(lib = "padscore", name = "WPADIsMplsAttached")]
fn wpad_is_mpls_attached(
    ctx: &mut HleContext<'_>,
    channel: u32,
    attached: u32,
    callback: u32,
) -> Result<i32, HleError> {
    if channel >= KPAD_MAX_CONTROLLERS {
        return Ok(wpad_err::NO_CONTROLLER);
    }
    if attached != 0 {
        ctx.memory.write_u32(attached, 0)?;
    }
    if callback != 0 {
        let status = if controller(ctx, channel).is_some() {
            wpad_err::NONE
        } else {
            wpad_err::NO_CONTROLLER
        };
        ctx.os.callbacks.push(GuestCall { function: callback, args: vec![channel, status as u32] });
    }
    Ok(wpad_err::NONE)
}

/// Accelerometer gravity unit: Wii Remotes report about 99 per g on each axis.
#[hle_export(lib = "padscore", name = "WPADGetAccGravityUnit")]
fn wpad_get_acc_gravity_unit(
    ctx: &mut HleContext<'_>,
    channel: u32,
    _kind: u32,
    out: u32,
) -> Result<(), HleError> {
    if channel >= KPAD_MAX_CONTROLLERS || out == 0 {
        return Ok(());
    }
    for i in 0..3 {
        ctx.memory.write_u32(out + i * 4, 99)?;
    }
    Ok(())
}

// ---- KPAD ------------------------------------------------------------------

#[hle_export(lib = "padscore", name = "KPADInit")]
fn kpad_init(ctx: &mut HleContext<'_>) -> u32 {
    kpad_init_ex(ctx, 0, 0)
}

#[hle_export(lib = "padscore", name = "KPADInitEx")]
fn kpad_init_ex(ctx: &mut HleContext<'_>, ring_buffer: u32, length: u32) -> u32 {
    let state = &mut ctx.os.padscore;
    if !state.kpad_initialized {
        for channel in &mut state.channels {
            channel.dpd_enabled = true;
        }
        state.ring_buffer = ring_buffer;
        state.ring_buffer_length = length;
        state.kpad_initialized = true;
    }
    0
}

#[hle_export(lib = "padscore", name = "KPADGetMaxControllers")]
fn kpad_get_max_controllers(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.padscore.max_controllers
}

#[hle_export(lib = "padscore", name = "KPADSetMaxControllers")]
fn kpad_set_max_controllers(ctx: &mut HleContext<'_>, count: u32) -> i32 {
    if count != KPAD_MAX_CONTROLLERS && count != PRO_CHANNELS as u32 {
        return -4;
    }
    let state = &mut ctx.os.padscore;
    if state.max_controllers == count {
        return 0;
    }
    state.max_controllers = count;
    state.callback_by_kpad = true;
    count.cast_signed()
}

#[hle_export(lib = "padscore", name = "KPADEnableDPD")]
fn kpad_enable_dpd(ctx: &mut HleContext<'_>, channel: u32) -> u32 {
    if controller(ctx, channel).is_some()
        && let Some(state) = channel_mut(ctx, channel)
        && state.format != format::FREESTYLE
        && state.format != 0x1F
    {
        state.dpd_enabled = true;
    }
    0
}

#[hle_export(lib = "padscore", name = "KPADGetMplsWorkSize")]
const fn kpad_get_mpls_work_size(_ctx: &mut HleContext<'_>) -> u32 {
    KPAD_MPLS_WORK_SIZE
}

/// Delay and pulse arrive in seconds; Cemu truncates them to whole milliseconds counts,
/// which disables the repeat for usual values. Here they are converted properly.
#[hle_export(lib = "padscore", name = "KPADSetBtnRepeat")]
fn kpad_set_btn_repeat(ctx: &mut HleContext<'_>, channel: u32, delay: f32, pulse: f32) -> u32 {
    if let Some(state) = channel_mut(ctx, channel) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            state.repeat_delay_ms = (delay.max(0.0) * 1000.0) as u32;
            state.repeat_pulse_ms = (pulse.max(0.0) * 1000.0) as u32;
        }
    }
    0
}

#[hle_export(lib = "padscore", name = "KPADGetUnifiedWpadStatus")]
fn kpad_get_unified_wpad_status(
    ctx: &mut HleContext<'_>,
    channel: u32,
    out: u32,
    count: u32,
) -> Result<u32, HleError> {
    if channel as usize >= PRO_CHANNELS || out == 0 {
        return Ok(0);
    }
    ctx.memory.fill(out, UNIFIED_STATUS_SIZE.saturating_mul(count), 0)?;
    match controller(ctx, channel) {
        Some(pad) => {
            let format = ctx.os.padscore.channels[channel as usize].format;
            if matches!(format, format::CORE | format::CORE_ACC | format::CORE_ACC_DPD) {
                #[allow(clippy::cast_possible_truncation)]
                ctx.memory.write_u8(out + UNIFIED_STATUS_SIZE - 6, format as u8)?;
                write_wpad_status(ctx, channel, pad, out)?;
            }
        }
        None => ctx.memory.write_u8(out + wpad::ERROR, wpad_err::NO_CONTROLLER as u8)?,
    }
    Ok(0)
}

/// Milliseconds of timebase ticks.
const fn ticks_ms(ms: u32) -> u64 {
    wiiu_peripherals::timebase::FREQUENCY / 1000 * ms as u64
}

/// Fills `buffer` with one sample of `channel`; returns the samples written and the error.
fn kpad_read(
    ctx: &mut HleContext<'_>,
    channel: u32,
    buffer: u32,
    count: u32,
) -> Result<(u32, i32), HleError> {
    let index = channel as usize;
    if index >= PRO_CHANNELS {
        return Ok((0, kpad_err::NO_CONTROLLER));
    }
    let Some(pad) = controller(ctx, channel) else {
        return Ok((0, kpad_err::NO_CONTROLLER));
    };
    let now = std::time::Instant::now();
    let too_soon =
        ctx.os.pro_last_read[index].is_some_and(|last| now - last < KPAD_SAMPLE_INTERVAL);
    if count == 0 || buffer == 0 || too_soon {
        return Ok((0, kpad_err::NO_SAMPLE_DATA));
    }
    ctx.os.pro_last_read[index] = Some(now);
    let tick = ctx.os.timebase.ticks_since_boot();
    let state = &mut ctx.os.padscore.channels[index];
    let format = state.format;
    let mut hold = pro_buttons(pad.held);
    if hold != state.last_hold {
        state.hold_changed_tick = tick;
        state.last_pulse_tick = tick;
    }
    if state.repeat_pulse_ms > 0
        && tick >= state.hold_changed_tick + ticks_ms(state.repeat_delay_ms)
        && tick >= state.last_pulse_tick + ticks_ms(state.repeat_pulse_ms)
    {
        state.last_pulse_tick = tick;
        hold |= BUTTON_REPEAT;
    }
    let release = state.last_hold & !hold;
    let trigger = !state.last_hold & hold;
    state.last_hold = hold & !BUTTON_REPEAT;

    ctx.memory.fill(buffer, KPAD_STATUS_SIZE, 0)?;
    ctx.memory.write_u8(buffer + kpad::ERROR, 0)?;
    #[allow(clippy::cast_possible_truncation)]
    ctx.memory.write_u8(buffer + kpad::FORMAT, format as u8)?;
    ctx.memory.write_u8(buffer + kpad::DEVICE, DEV_PRO)?;
    ctx.memory.write_u8(buffer + kpad::DPD_VALID, 0)?;
    ctx.memory.write_u32(buffer + kpad::PRO_HOLD, hold)?;
    ctx.memory.write_u32(buffer + kpad::PRO_TRIGGER, trigger)?;
    ctx.memory.write_u32(buffer + kpad::PRO_RELEASE, release)?;
    for (at, stick) in
        [(kpad::PRO_LEFT_STICK, pad.left_stick), (kpad::PRO_RIGHT_STICK, pad.right_stick)]
    {
        ctx.memory.write_f32(buffer + at, stick.x)?;
        ctx.memory.write_f32(buffer + at + 4, stick.y)?;
    }
    ctx.memory.write_u32(buffer + kpad::PRO_CHARGE, 0)?;
    ctx.memory.write_u32(buffer + kpad::PRO_CABLE, 1)?;
    Ok((1, kpad_err::NONE))
}

#[hle_export(lib = "padscore", name = "KPADReadEx")]
fn kpad_read_ex(
    ctx: &mut HleContext<'_>,
    channel: u32,
    buffer: u32,
    count: u32,
    error: u32,
) -> Result<u32, HleError> {
    let (samples, result) = kpad_read(ctx, channel, buffer, count)?;
    if error != 0 {
        ctx.memory.write_u32(error, result.cast_unsigned())?;
    }
    Ok(samples)
}

#[hle_export(lib = "padscore", name = "KPADRead")]
fn kpad_read_plain(
    ctx: &mut HleContext<'_>,
    channel: u32,
    buffer: u32,
    count: u32,
) -> Result<u32, HleError> {
    Ok(kpad_read(ctx, channel, buffer, count)?.0)
}

// ---- Tick ------------------------------------------------------------------

impl CafeOs {
    /// Cemu's padscore alarm (every 5 ms): reports connections to the connect callback,
    /// the device to the extension callback, and calls the sampling callbacks.
    pub(crate) fn padscore_tick(&mut self) {
        let now = self.timebase.ticks_since_boot();
        if now < self.padscore.next_tick {
            return;
        }
        self.padscore.next_tick = now + TICK_PERIOD;
        let mut calls = Vec::new();
        for (i, channel) in self.padscore.channels.iter_mut().enumerate() {
            let index = i as u32;
            let connected = self.pro_controllers[i].is_some();
            if channel.connect_callback != 0 {
                if !channel.disconnect_called {
                    channel.disconnect_called = true;
                    let args = vec![index, wpad_err::NO_CONTROLLER.cast_unsigned()];
                    calls.push(GuestCall { function: channel.connect_callback, args });
                    continue;
                }
                if connected != channel.reported_connected {
                    channel.reported_connected = connected;
                    let error = if connected { wpad_err::NONE } else { wpad_err::NO_CONTROLLER };
                    let args = vec![index, error.cast_unsigned()];
                    calls.push(GuestCall { function: channel.connect_callback, args });
                }
            }
            if channel.extension_callback != 0 && connected && !channel.reported_extension {
                channel.reported_extension = true;
                let args = vec![index, u32::from(DEV_PRO)];
                calls.push(GuestCall { function: channel.extension_callback, args });
            }
            if !connected {
                channel.reported_extension = false;
            }
        }
        for (i, channel) in self.padscore.channels.iter().enumerate() {
            if channel.sampling_callback != 0 && self.pro_controllers[i].is_some() {
                let args = vec![i as u32];
                calls.push(GuestCall { function: channel.sampling_callback, args });
            }
        }
        for call in calls {
            self.callbacks.push(call);
        }
    }
}

pub const EXPORTS: &[HleExport] = &[
    WPAD_GET_STATUS,
    WPAD_GET_BATTERY_LEVEL,
    WPAD_PROBE,
    WPAD_GET_INFO,
    WPAD_GET_INFO_ASYNC,
    WPAD_READ,
    WPAD_SET_DATA_FORMAT,
    WPAD_GET_DATA_FORMAT,
    WPAD_IS_MOTOR_ENABLED,
    WPAD_CONTROL_MOTOR,
    WPAD_CONTROL_DPD,
    WPAD_SET_EXTENSION_CALLBACK,
    KPAD_SET_CONNECT_CALLBACK,
    WPAD_SET_CONNECT_CALLBACK,
    KPAD_SET_SAMPLING_CALLBACK,
    WPAD_SET_SAMPLING_CALLBACK,
    WPAD_SET_CALLBACK_BY_KPAD,
    WPAD_IS_MPLS_ATTACHED,
    WPAD_GET_ACC_GRAVITY_UNIT,
    KPAD_INIT,
    KPAD_INIT_EX,
    KPAD_GET_MAX_CONTROLLERS,
    KPAD_SET_MAX_CONTROLLERS,
    KPAD_ENABLE_DPD,
    KPAD_GET_MPLS_WORK_SIZE,
    KPAD_SET_BTN_REPEAT,
    KPAD_GET_UNIFIED_WPAD_STATUS,
    KPAD_READ_EX,
    KPAD_READ_PLAIN,
];

#[cfg(test)]
mod tests {
    use wiiu_peripherals::{GamePadState, VpadButtons};

    use super::kpad;
    use crate::hle::test_env::{SCRATCH, TestEnv};

    /// Intent: `WPADProbe` tells titles which channels have a Pro Controller.
    /// Pass: an empty channel returns -1 with device 0xFD; a connected one returns 0 with
    /// device 31 (`WPAD_DEV_URCC`).
    #[test]
    fn probe_reports_connected_pro_controllers() {
        let mut env = TestEnv::new();
        assert_eq!(env.call("padscore", "WPADProbe", &[0, SCRATCH]) as i32, -1);
        assert_eq!(env.memory.read_u32(SCRATCH).unwrap(), 0xFD);
        env.os.pro_controllers[0] = Some(GamePadState::default());
        assert_eq!(env.call("padscore", "WPADProbe", &[0, SCRATCH]), 0);
        assert_eq!(env.memory.read_u32(SCRATCH).unwrap(), 31);
    }

    /// Intent: `KPADReadEx` gives a Pro Controller sample with the buttons in its own bit
    /// layout (A is 0x10, not the GamePad's 0x8000) and edge bits for new presses.
    /// Pass: one sample, no error, hold and trigger both 0x10, format 22 (`WPAD_FMT_URCC`).
    #[test]
    fn kpad_reads_pro_controller_buttons() {
        let mut env = TestEnv::new();
        let pad = GamePadState { held: VpadButtons::A, ..GamePadState::default() };
        env.os.pro_controllers[1] = Some(pad);
        let error = SCRATCH + 0x200;
        assert_eq!(env.call("padscore", "KPADReadEx", &[1, SCRATCH, 1, error]), 1);
        assert_eq!(env.memory.read_u32(error).unwrap(), 0);
        assert_eq!(env.memory.read_u32(SCRATCH + kpad::PRO_HOLD).unwrap(), 0x10);
        assert_eq!(env.memory.read_u32(SCRATCH + kpad::PRO_TRIGGER).unwrap(), 0x10);
        assert_eq!(env.memory.read_u8(SCRATCH + kpad::FORMAT).unwrap(), 22);
    }

    /// Intent: titles wait for `WPADGetStatus` to report the library ready (Xenoblade
    /// Chronicles X polled it forever while it returned 0).
    /// Pass: it returns 1.
    #[test]
    fn wpad_reports_ready() {
        let mut env = TestEnv::new();
        assert_eq!(env.call("padscore", "WPADGetStatus", &[]), 1);
    }

    /// Intent: a connect callback first hears "no controller" for every channel, then
    /// "connected" once a Pro Controller is plugged in, as Cemu's 5 ms tick does.
    /// Pass: the first tick queues (0, -1); after connecting, the next tick queues (0, 0).
    #[test]
    fn connect_callback_reports_disconnect_then_connect() {
        let mut env = TestEnv::new();
        env.call("padscore", "WPADSetConnectCallback", &[0, 0x0200_0000]);
        env.os.padscore_tick();
        assert_eq!(env.os.callbacks.pending(), 1);
        env.os.pro_controllers[0] = Some(GamePadState::default());
        env.os.padscore.next_tick = 0;
        env.os.padscore_tick();
        assert_eq!(env.os.callbacks.pending(), 2);
    }
}
