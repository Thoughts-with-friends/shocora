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
//! │ Wii Remote / Pro   │   │ padscore/kpad_exports.rs │   │ button & stick     │
//! │ Controller         │──►│ ──────────────────────── │──►│ state              │
//! │                    │   │ WPAD/KPAD libraries:     │   │                    │
//! │                    │   │ kpad exports             │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "kpad exports" share of padscore.rs (WPAD/KPAD libraries), moved here so that no file is
//! longer than 500 lines. It works exactly as if it were still written in that file. The OS
//! library that reports Wii Remotes and the Pro Controller to games. Your PC controller is
//! presented as one of them. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;

#[hle_export(lib = "padscore", name = "WPADGetDataFormat")]
pub(super) fn wpad_get_data_format(ctx: &mut HleContext<'_>, channel: u32) -> u32 {
    if controller(ctx, channel).is_some() {
        ctx.os.padscore.channels[channel as usize].format
    } else {
        format::CORE
    }
}

#[hle_export(lib = "padscore", name = "WPADIsMotorEnabled")]
pub(super) const fn wpad_is_motor_enabled(_ctx: &mut HleContext<'_>) -> bool {
    true
}

/// Rumble is not forwarded to host controllers.
#[hle_export(lib = "padscore", name = "WPADControlMotor")]
pub(super) const fn wpad_control_motor(
    _ctx: &mut HleContext<'_>,
    _channel: u32,
    _command: u32,
) -> u32 {
    0
}

#[hle_export(lib = "padscore", name = "WPADControlDpd")]
pub(super) fn wpad_control_dpd(
    ctx: &mut HleContext<'_>,
    channel: u32,
    _command: u32,
    callback: u32,
) -> i32 {
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
pub(super) fn swap_callback(
    ctx: &mut HleContext<'_>,
    channel: u32,
    callback: u32,
    field: fn(&mut Channel) -> &mut u32,
) -> u32 {
    channel_mut(ctx, channel).map_or(0, |state| std::mem::replace(field(state), callback))
}

#[hle_export(lib = "padscore", name = "WPADSetExtensionCallback")]
pub(super) fn wpad_set_extension_callback(
    ctx: &mut HleContext<'_>,
    channel: u32,
    callback: u32,
) -> u32 {
    swap_callback(ctx, channel, callback, |c| &mut c.extension_callback)
}

#[hle_export(lib = "padscore", name = "KPADSetConnectCallback")]
pub(super) fn kpad_set_connect_callback(
    ctx: &mut HleContext<'_>,
    channel: u32,
    callback: u32,
) -> u32 {
    swap_callback(ctx, channel, callback, |c| &mut c.connect_callback)
}

#[hle_export(lib = "padscore", name = "WPADSetConnectCallback")]
pub(super) fn wpad_set_connect_callback(
    ctx: &mut HleContext<'_>,
    channel: u32,
    callback: u32,
) -> u32 {
    swap_callback(ctx, channel, callback, |c| &mut c.connect_callback)
}

#[hle_export(lib = "padscore", name = "KPADSetSamplingCallback")]
pub(super) fn kpad_set_sampling_callback(
    ctx: &mut HleContext<'_>,
    channel: u32,
    callback: u32,
) -> u32 {
    swap_callback(ctx, channel, callback, |c| &mut c.sampling_callback)
}

#[hle_export(lib = "padscore", name = "WPADSetSamplingCallback")]
pub(super) fn wpad_set_sampling_callback(
    ctx: &mut HleContext<'_>,
    channel: u32,
    callback: u32,
) -> u32 {
    swap_callback(ctx, channel, callback, |c| &mut c.sampling_callback)
}

#[hle_export(lib = "padscore", name = "WPADSetCallbackByKPAD")]
pub(super) fn wpad_set_callback_by_kpad(ctx: &mut HleContext<'_>, state: u32) -> u32 {
    ctx.os.padscore.callback_by_kpad = state != 0;
    0
}

/// Reports whether MotionPlus is attached (never, for a Pro Controller) and calls the
/// optional callback with `(channel, status)` before returning.
#[hle_export(lib = "padscore", name = "WPADIsMplsAttached")]
pub(super) fn wpad_is_mpls_attached(
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
pub(super) fn wpad_get_acc_gravity_unit(
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
pub(super) fn kpad_init(ctx: &mut HleContext<'_>) -> u32 {
    kpad_init_ex(ctx, 0, 0)
}

#[hle_export(lib = "padscore", name = "KPADInitEx")]
pub(super) fn kpad_init_ex(ctx: &mut HleContext<'_>, ring_buffer: u32, length: u32) -> u32 {
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
pub(super) fn kpad_get_max_controllers(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.padscore.max_controllers
}

#[hle_export(lib = "padscore", name = "KPADSetMaxControllers")]
pub(super) fn kpad_set_max_controllers(ctx: &mut HleContext<'_>, count: u32) -> i32 {
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
pub(super) fn kpad_enable_dpd(ctx: &mut HleContext<'_>, channel: u32) -> u32 {
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
pub(super) const fn kpad_get_mpls_work_size(_ctx: &mut HleContext<'_>) -> u32 {
    KPAD_MPLS_WORK_SIZE
}

/// Delay and pulse arrive in seconds; Cemu truncates them to whole milliseconds counts,
/// which disables the repeat for usual values. Here they are converted properly.
#[hle_export(lib = "padscore", name = "KPADSetBtnRepeat")]
pub(super) fn kpad_set_btn_repeat(
    ctx: &mut HleContext<'_>,
    channel: u32,
    delay: f32,
    pulse: f32,
) -> u32 {
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
pub(super) fn kpad_get_unified_wpad_status(
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
pub(super) const fn ticks_ms(ms: u32) -> u64 {
    wiiu_peripherals::timebase::FREQUENCY / 1000 * ms as u64
}

/// Fills `buffer` with one sample of `channel`; returns the samples written and the error.
pub(super) fn kpad_read(
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
pub(super) fn kpad_read_ex(
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
pub(super) fn kpad_read_plain(
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
