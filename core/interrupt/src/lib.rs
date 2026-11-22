//! Interrupt sources shared by every Wii U component.
//!
//! Devices raise an [`Interrupt`] on a core's [`PendingInterrupts`]; the
//! system loop takes them and hands them to the OS layer, which runs the
//! matching handler (GX2 flip callbacks, alarms, IPC replies, ...).
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//! ▲▲▲▲▲▲▲▲▲▲▲▲▲▲▲▲▲▲  here
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ device events      │   │ lib.rs                   │   │ cores notified     │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ interrupts               │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! A way for devices to tap a CPU core on the shoulder ("vsync happened", "GPU done"). Terms
//! such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.

/// External interrupt sources the emulator models.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Interrupt {
    /// Decrementer reached zero (drives OSAlarm).
    Decrementer = 0,
    /// Latte finished scanning out a frame on the TV.
    GpuVsyncTv = 1,
    /// Latte finished scanning out a frame on the GamePad (DRC).
    GpuVsyncDrc = 2,
    /// Latte retired a command buffer timestamp / EOP event.
    GpuEndOfPipe = 3,
    /// Latte flipped a scan buffer.
    GpuFlip = 4,
    /// IOSU completed an IPC request.
    IpcReply = 5,
    /// The DSP finished an audio frame.
    DspFrame = 6,
    /// Inter-processor interrupt between Espresso cores.
    InterCore = 7,
}

impl Interrupt {
    pub const ALL: [Self; 8] = [
        Self::Decrementer,
        Self::GpuVsyncTv,
        Self::GpuVsyncDrc,
        Self::GpuEndOfPipe,
        Self::GpuFlip,
        Self::IpcReply,
        Self::DspFrame,
        Self::InterCore,
    ];

    const fn bit(self) -> u32 {
        1 << self as u8
    }
}

/// Interrupts raised on one core and not yet serviced.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PendingInterrupts(u32);

impl PendingInterrupts {
    #[inline]
    pub const fn raise(&mut self, interrupt: Interrupt) {
        self.0 |= interrupt.bit();
    }

    #[inline]
    pub const fn is_pending(&self, interrupt: Interrupt) -> bool {
        self.0 & interrupt.bit() != 0
    }

    #[inline]
    pub const fn any(&self) -> bool {
        self.0 != 0
    }

    /// Removes and returns the highest-priority (lowest numbered) pending interrupt.
    #[inline]
    pub fn take_next(&mut self) -> Option<Interrupt> {
        let next = Interrupt::ALL.into_iter().find(|i| self.is_pending(*i))?;
        self.0 &= !next.bit();
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_in_priority_order() {
        let mut pending = PendingInterrupts::default();
        pending.raise(Interrupt::IpcReply);
        pending.raise(Interrupt::Decrementer);
        assert_eq!(pending.take_next(), Some(Interrupt::Decrementer));
        assert_eq!(pending.take_next(), Some(Interrupt::IpcReply));
        assert_eq!(pending.take_next(), None);
        assert!(!pending.any());
    }
}
