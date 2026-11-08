//! Espresso timebase.
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Timebase frequency: bus clock (248.625 MHz) / 4.
pub const FREQUENCY: u64 = 62_156_250;

/// Seconds between the Unix epoch and the Wii U epoch (2000-01-01 00:00:00).
pub const WIIU_EPOCH_UNIX_SECONDS: u64 = 946_684_800;

/// Converts a duration to timebase ticks.
#[allow(clippy::cast_possible_truncation)]
#[inline]
pub const fn duration_to_ticks(duration: Duration) -> u64 {
    (duration.as_nanos() * FREQUENCY as u128 / 1_000_000_000) as u64
}

/// Converts timebase ticks to a duration.
#[allow(clippy::cast_possible_truncation)]
#[inline]
pub const fn ticks_to_duration(ticks: u64) -> Duration {
    Duration::from_nanos((ticks as u128 * 1_000_000_000 / FREQUENCY as u128) as u64)
}

/// How fast the guest's time runs compared to real time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GameSpeed {
    Half,
    #[default]
    Normal,
    Double,
    Triple,
    Quadruple,
}

impl GameSpeed {
    /// Every speed, slowest first.
    pub const ALL: [Self; 5] =
        [Self::Half, Self::Normal, Self::Double, Self::Triple, Self::Quadruple];

    /// The speed after this one when cycling through them.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Normal => Self::Double,
            Self::Double => Self::Triple,
            Self::Triple => Self::Quadruple,
            Self::Quadruple => Self::Half,
            Self::Half => Self::Normal,
        }
    }

    /// The speed as a multiple of half of real time, which keeps every speed a whole number.
    pub const fn halves(self) -> u32 {
        match self {
            Self::Half => 1,
            Self::Normal => 2,
            Self::Double => 4,
            Self::Triple => 6,
            Self::Quadruple => 8,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Half => "0.5x",
            Self::Normal => "1x",
            Self::Double => "2x",
            Self::Triple => "3x",
            Self::Quadruple => "4x",
        }
    }
}

/// Monotonic tick counter plus the wall-clock offset used by `OSGetTime`.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(into = "SavedTimeBase", from = "SavedTimeBase")]
pub struct TimeBase {
    /// When the current speed began.
    speed_start: Instant,
    /// Ticks since power-on at `speed_start`.
    ticks_at_speed_start: u64,
    speed: GameSpeed,
    /// Ticks since the Wii U epoch at power-on.
    epoch_ticks_at_boot: u64,
}

/// A [`TimeBase`] without its host clock: time goes on from `ticks` when loaded.
#[derive(serde::Serialize, serde::Deserialize)]
struct SavedTimeBase {
    ticks: u64,
    speed: GameSpeed,
    epoch_ticks_at_boot: u64,
}

impl From<TimeBase> for SavedTimeBase {
    fn from(base: TimeBase) -> Self {
        Self {
            ticks: base.ticks_since_boot(),
            speed: base.speed,
            epoch_ticks_at_boot: base.epoch_ticks_at_boot,
        }
    }
}

impl From<SavedTimeBase> for TimeBase {
    fn from(saved: SavedTimeBase) -> Self {
        Self {
            speed_start: Instant::now(),
            ticks_at_speed_start: saved.ticks,
            speed: saved.speed,
            epoch_ticks_at_boot: saved.epoch_ticks_at_boot,
        }
    }
}

impl TimeBase {
    pub fn new() -> Self {
        let since_unix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let since_wiiu = since_unix.saturating_sub(Duration::from_secs(WIIU_EPOCH_UNIX_SECONDS));
        Self {
            speed_start: Instant::now(),
            ticks_at_speed_start: 0,
            speed: GameSpeed::Normal,
            epoch_ticks_at_boot: duration_to_ticks(since_wiiu),
        }
    }

    /// Time runs at `speed` from now on, continuing from where it is.
    #[inline]
    pub fn set_speed(&mut self, speed: GameSpeed) {
        self.ticks_at_speed_start = self.ticks_since_boot();
        self.speed_start = Instant::now();
        self.speed = speed;
    }

    /// Ticks since power-on (`mftb`, `OSGetSystemTime`).
    #[inline]
    pub fn ticks_since_boot(&self) -> u64 {
        let elapsed = duration_to_ticks(self.speed_start.elapsed());
        self.ticks_at_speed_start + elapsed * u64::from(self.speed.halves()) / 2
    }

    /// Ticks since 2000-01-01 (`OSGetTime`).
    #[inline]
    pub fn ticks_since_epoch(&self) -> u64 {
        self.epoch_ticks_at_boot + self.ticks_since_boot()
    }
}

impl Default for TimeBase {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_conversions() {
        assert_eq!(duration_to_ticks(Duration::from_secs(1)), FREQUENCY);
        assert_eq!(ticks_to_duration(FREQUENCY * 3), Duration::from_secs(3));
        let tb = TimeBase::new();
        assert!(tb.ticks_since_epoch() > FREQUENCY * 60 * 60 * 24 * 365 * 20);
    }

    #[test]
    fn speeds_cycle_in_the_order_players_expect() {
        let mut speed = GameSpeed::Normal;
        let mut seen = Vec::new();
        for _ in 0..5 {
            speed = speed.next();
            seen.push(speed.label());
        }
        assert_eq!(seen, ["2x", "3x", "4x", "0.5x", "1x"]);
    }

    #[test]
    fn time_keeps_going_forward_through_speed_changes() {
        let mut tb = TimeBase::new();
        let mut last = tb.ticks_since_boot();
        for speed in [GameSpeed::Quadruple, GameSpeed::Half, GameSpeed::Normal] {
            tb.set_speed(speed);
            std::thread::sleep(Duration::from_millis(2));
            let now = tb.ticks_since_boot();
            assert!(now > last, "{speed:?}: {now} after {last}");
            last = now;
        }
    }

    #[test]
    fn faster_time_gains_ticks_faster() {
        let mut tb = TimeBase::new();
        tb.set_speed(GameSpeed::Quadruple);
        let wait = Duration::from_millis(20);
        std::thread::sleep(wait);
        // At least four times what passed; sleeping can only take longer.
        assert!(tb.ticks_since_boot() >= duration_to_ticks(wait) * 4);
    }
}
