//! What every shocora front end shares on top of the core crates:
//! [`console::Console`] (the emulator running on its own thread), the
//! persisted [`config::AppConfig`] and keyboard/controller → GamePad [`input`] mapping.
pub mod audio;
pub mod config;
pub mod console;
pub mod controller_motion;
pub mod graphic_packs;
pub mod input;
pub mod translate;
