//! `OSAlarm`: one-shot and periodic timers that call a guest handler.
//!
//! Deadlines use `OSGetTime` ticks (since 2000-01-01). Due alarms are turned
//! into [`GuestCall`]s by [`Alarms::fire_due`]; the handler receives the alarm
//! and an `OSContext` pointer.
use std::collections::BTreeMap;

use wiiu_hle_macros::hle_export;

use crate::hle::{HleContext, HleError, callback::GuestCall};

mod offset {
    pub const MAGIC: u32 = 0x00;
    pub const NAME: u32 = 0x04;
    pub const HANDLER: u32 = 0x0C;
    pub const TAG: u32 = 0x10;
    pub const USER_DATA: u32 = 0x38;
}

/// `sizeof(OSAlarm)`.
const ALARM_SIZE: u32 = 0x58;
/// `"aLrM"`.
const ALARM_MAGIC: u32 = 0x614C_724D;

/// Host-side state of one armed alarm.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArmedAlarm {
    pub handler: u32,
    pub deadline: u64,
    /// 0 for one-shot alarms.
    pub period: u64,
    pub tag: u32,
}

/// All armed alarms, keyed by `OSAlarm` address.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct Alarms {
    pub armed: BTreeMap<u32, ArmedAlarm>,
    /// `OSContext` passed to handlers (allocated on first use).
    pub context: u32,
}

impl Alarms {
    /// Calls for every alarm due at `now`; re-arms periodic ones.
    pub fn fire_due(&mut self, now: u64) -> Vec<GuestCall> {
        let context = self.context;
        let mut calls = Vec::new();
        self.armed.retain(|alarm, state| {
            if state.deadline > now {
                return true;
            }
            calls.push(GuestCall { function: state.handler, args: vec![*alarm, context] });
            if state.period == 0 {
                return false;
            }
            // Skip periods missed while the guest was not running.
            let missed = (now - state.deadline) / state.period;
            state.deadline += (missed + 1) * state.period;
            true
        });
        calls
    }

    /// Earliest deadline of any armed alarm.
    #[inline]
    pub fn next_deadline(&self) -> Option<u64> {
        self.armed.values().map(|a| a.deadline).min()
    }
}

fn init(ctx: &mut HleContext<'_>, alarm: u32, name: u32) -> Result<(), HleError> {
    ctx.memory.fill(alarm, ALARM_SIZE, 0)?;
    ctx.memory.write_u32(alarm + offset::MAGIC, ALARM_MAGIC)?;
    ctx.memory.write_u32(alarm + offset::NAME, name)?;
    ctx.os.coreinit.alarms.armed.remove(&alarm);
    Ok(())
}

fn arm(
    ctx: &mut HleContext<'_>,
    alarm: u32,
    deadline: u64,
    period: u64,
    handler: u32,
) -> Result<(), HleError> {
    if ctx.os.coreinit.alarms.context == 0 {
        ctx.os.coreinit.alarms.context = ctx.os.registry.allocate_data(0x320, 8);
    }
    ctx.memory.write_u32(alarm + offset::HANDLER, handler)?;
    let tag = ctx.memory.read_u32(alarm + offset::TAG)?;
    ctx.os.coreinit.alarms.armed.insert(alarm, ArmedAlarm { handler, deadline, period, tag });
    Ok(())
}

#[hle_export(lib = "coreinit", name = "OSCreateAlarm")]
fn os_create_alarm(ctx: &mut HleContext<'_>, alarm: u32) -> Result<(), HleError> {
    init(ctx, alarm, 0)
}

#[hle_export(lib = "coreinit", name = "OSCreateAlarmEx")]
fn os_create_alarm_ex(ctx: &mut HleContext<'_>, alarm: u32, name: u32) -> Result<(), HleError> {
    init(ctx, alarm, name)
}

/// Fires once, `delay` ticks from now.
#[hle_export(lib = "coreinit", name = "OSSetAlarm")]
fn os_set_alarm(
    ctx: &mut HleContext<'_>,
    alarm: u32,
    delay: u64,
    handler: u32,
) -> Result<bool, HleError> {
    let deadline = ctx.os.timebase.ticks_since_epoch().saturating_add(delay);
    arm(ctx, alarm, deadline, 0, handler)?;
    Ok(true)
}

/// Fires at absolute time `start`, then every `period` ticks.
#[hle_export(lib = "coreinit", name = "OSSetPeriodicAlarm")]
fn os_set_periodic_alarm(
    ctx: &mut HleContext<'_>,
    alarm: u32,
    start: u64,
    period: u64,
    handler: u32,
) -> Result<bool, HleError> {
    let now = ctx.os.timebase.ticks_since_epoch();
    let deadline = if start == 0 { now + period } else { start };
    arm(ctx, alarm, deadline, period.max(1), handler)?;
    Ok(true)
}

#[hle_export(lib = "coreinit", name = "OSCancelAlarm")]
fn os_cancel_alarm(ctx: &mut HleContext<'_>, alarm: u32) -> bool {
    ctx.os.coreinit.alarms.armed.remove(&alarm).is_some()
}

#[hle_export(lib = "coreinit", name = "OSCancelAlarms")]
fn os_cancel_alarms(ctx: &mut HleContext<'_>, tag: u32) {
    ctx.os.coreinit.alarms.armed.retain(|_, a| a.tag != tag);
}

#[hle_export(lib = "coreinit", name = "OSSetAlarmTag")]
fn os_set_alarm_tag(ctx: &mut HleContext<'_>, alarm: u32, tag: u32) -> Result<(), HleError> {
    ctx.memory.write_u32(alarm + offset::TAG, tag)?;
    if let Some(armed) = ctx.os.coreinit.alarms.armed.get_mut(&alarm) {
        armed.tag = tag;
    }
    Ok(())
}

#[hle_export(lib = "coreinit", name = "OSSetAlarmUserData")]
fn os_set_alarm_user_data(ctx: &mut HleContext<'_>, alarm: u32, data: u32) -> Result<(), HleError> {
    Ok(ctx.memory.write_u32(alarm + offset::USER_DATA, data)?)
}

#[hle_export(lib = "coreinit", name = "OSGetAlarmUserData")]
fn os_get_alarm_user_data(ctx: &mut HleContext<'_>, alarm: u32) -> Result<u32, HleError> {
    Ok(ctx.memory.read_u32(alarm + offset::USER_DATA)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periodic_alarms_skip_missed_periods() {
        let mut alarms = Alarms { context: 0x99, ..Alarms::default() };
        alarms.armed.insert(0x10, ArmedAlarm { handler: 0x100, deadline: 50, period: 0, tag: 0 });
        alarms.armed.insert(0x20, ArmedAlarm { handler: 0x200, deadline: 100, period: 10, tag: 0 });
        assert_eq!(alarms.fire_due(40), []);
        let fired = alarms.fire_due(125);
        assert_eq!(fired.len(), 2);
        assert_eq!(fired[1], GuestCall { function: 0x200, args: vec![0x20, 0x99] });
        assert!(!alarms.armed.contains_key(&0x10), "one-shot alarms disarm");
        assert_eq!(alarms.next_deadline(), Some(130));
    }
}
