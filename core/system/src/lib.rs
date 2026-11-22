//! The Wii U system: wires every component into a runnable [`Emulator`].
//!
//! ```text
//!  Emulator
//!   ├─ cores[3]: wiiu_cpu::CpuState ──step──► emulator::bus::CoreBus ──► Memory
//!   │      └─ StepEvent::HleCall ──► wiiu_cafe_os::dispatch(HleContext)
//!   ├─ os:   wiiu_cafe_os::CafeOs   (HLE libraries, heaps, console)
//!   ├─ iosu: wiiu_iosu::Iosu        (FSA over the title's mounts)
//!   └─ gpu:  wiiu_gpu::Gpu          (PM4 → RenderCommand)
//! ```
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!               ▲▲▲▲▲▲    ▲▲▲     ▲▲▲▲▲▲     ▲▲▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ a game             │   │ lib.rs                   │   │ frames, sound      │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ the whole Wii U          │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Wires every part into one runnable emulator. Terms such as HLE, JIT or shader are explained
//! in `docs/guide/wiiu_term.md`.
mod emulator;
mod error;

pub use emulator::{
    EmuStats, Emulator, OutputFilter, Status, emu_config::Config, state::STATE_SLOTS,
};
pub use error::EmuError;
pub use wiiu_gpu::{RenderCommand, Screen};
pub mod cheat;
pub use wiiu_peripherals::{GamePadState, GameSpeed, Motion, Pose, Stick, TiltMotion, VpadButtons};
pub use wiiu_title::TitleInfo;

#[cfg(test)]
mod tests {
    use super::*;

    /// Intent: a default `Config` can build an emulator that has nothing loaded yet.
    /// Pass: `Emulator::new` succeeds, there is no title, and the status is not running.
    #[test]
    fn a_new_emulator_has_no_title() {
        let emu = Emulator::new(Config::default()).unwrap();
        assert!(emu.title().is_none());
        assert_ne!(*emu.status(), Status::Running);
    }
}
