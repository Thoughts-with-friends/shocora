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
mod emulator;
mod error;

pub use emulator::{EmuStats, Emulator, Status, emu_config::Config, state::STATE_SLOTS};
pub use error::EmuError;
pub use wiiu_gpu::{RenderCommand, Screen};
pub mod cheat;
pub use wiiu_peripherals::{GamePadState, GameSpeed, VpadButtons};
pub use wiiu_title::TitleInfo;
