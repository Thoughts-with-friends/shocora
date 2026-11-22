//! Calendar conversion for `OSTicksToCalendarTime`.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//! ▲▲▲▲▲▲▲▲▲▲▲▲▲▲▲▲▲▲  here
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ clock ticks        │   │ rtc.rs                   │   │ date and time      │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ calendar                 │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Converts clock ticks into a calendar date. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.
#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap, clippy::cast_sign_loss)]

use crate::timebase::FREQUENCY;

/// Broken-down time in the layout of `OSCalendarTime`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CalendarTime {
    pub second: i32,
    pub minute: i32,
    pub hour: i32,
    /// Day of month, 1-based.
    pub day: i32,
    /// Month, 0-based.
    pub month: i32,
    pub year: i32,
    /// Day of week, 0 = Sunday.
    pub weekday: i32,
    /// Day of year, 0-based.
    pub yearday: i32,
    pub millisecond: i32,
    pub microsecond: i32,
}

/// Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
const fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + (month <= 2) as i64;
    (year, month, day)
}

const fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

impl CalendarTime {
    /// Converts ticks since the Wii U epoch (2000-01-01).
    pub const fn from_ticks(ticks: i64) -> Self {
        let freq = FREQUENCY as i64;
        let total_seconds = ticks.div_euclid(freq);
        let sub_ticks = ticks.rem_euclid(freq);
        let micros = sub_ticks * 1_000_000 / freq;

        // 2000-01-01 is day 10957 after the Unix epoch.
        let days = total_seconds.div_euclid(86_400) + 10_957;
        let secs_of_day = total_seconds.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);

        const CUMULATIVE: [i32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
        let mut yearday = CUMULATIVE[(month - 1) as usize] + day as i32 - 1;
        if month > 2 && is_leap(year) {
            yearday += 1;
        }

        Self {
            second: (secs_of_day % 60) as i32,
            minute: (secs_of_day / 60 % 60) as i32,
            hour: (secs_of_day / 3600) as i32,
            day: day as i32,
            month: month as i32 - 1,
            year: year as i32,
            // 1970-01-01 was a Thursday.
            weekday: (days + 4).rem_euclid(7) as i32,
            yearday,
            millisecond: (micros / 1000) as i32,
            microsecond: (micros % 1000) as i32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_and_leap_day() {
        let epoch = CalendarTime::from_ticks(0);
        assert_eq!((epoch.year, epoch.month, epoch.day, epoch.weekday), (2000, 0, 1, 6));

        // 2024-02-29 12:34:56.789
        let days: i64 =
            (2000..2024).map(|y| if is_leap(y) { 366 } else { 365 }).sum::<i64>() + 31 + 28;
        let seconds = days * 86_400 + 12 * 3600 + 34 * 60 + 56;
        let ticks = seconds * FREQUENCY as i64 + FREQUENCY as i64 * 7895 / 10000;
        let t = CalendarTime::from_ticks(ticks);
        assert_eq!((t.year, t.month, t.day, t.hour, t.minute, t.second), (2024, 1, 29, 12, 34, 56));
        assert_eq!((t.yearday, t.weekday, t.millisecond), (59, 4, 789));
    }
}
