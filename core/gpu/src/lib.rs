//! Latte: the Wii U GPU (an R7xx derivative).
//!
//! ```text
//!  GX2 (cafe_os) ──PM4 words──► command::pm4::parse ──► command::executor
//!                                                          │      │
//!                                              regs::RegisterFile  │ guest memory
//!                                                                  ▼
//!                            latte (state → draws) + shader (R600 → WGSL)
//!                                                                  │
//!                                          RenderCommand list ──► RenderBackend
//! ```
//!
//! This crate never talks to a graphics API. It produces API-neutral
//! [`RenderCommand`]s that a host [`RenderBackend`] executes.
pub mod addrlib;
pub mod command;
pub mod display;
mod error;
mod gpu;
pub mod latte;
pub mod regs;
pub mod render;
pub mod shader;
pub mod tiling;

pub use error::GpuError;
pub use gpu::{Gpu, GpuState, GpuStats};
pub use render::{RenderBackend, RenderCommand, Screen};
