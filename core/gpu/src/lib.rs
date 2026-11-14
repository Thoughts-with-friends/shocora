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
mod worker;

pub use error::GpuError;
pub use gpu::{GpuState, GpuStats};
pub use render::{RenderBackend, RenderCommand, Screen};
pub use worker::{Gpu, WriteNotifier};

#[cfg(test)]
mod tests {
    use super::render::{HostFormat, ResolutionScale, SurfaceKey};

    /// Intent: resolution scaling picks the factor for the TV picture and leaves data
    /// targets (square ones such as shadow maps) at console size.
    /// Pass: 4x on a 1280-wide TV stays 4x; 20x is capped by the 10K width limit; a
    /// 1280 x 720 target scales, a 1024 x 1024 one does not; below 4x comes 3x.
    #[test]
    fn resolution_scale_spares_square_targets() {
        let scale = ResolutionScale::for_tv(4, 1280);
        assert_eq!(scale.factor, 4);
        assert_eq!(ResolutionScale::for_tv(20, 1280).factor, 8);
        let key = |width, height| SurfaceKey {
            address: 0,
            width,
            height,
            layers: 1,
            format: HostFormat::Rgba8Unorm,
        };
        assert_eq!(scale.of(&key(1280, 720)), 4);
        assert_eq!(scale.of(&key(1024, 1024)), 1);
        assert_eq!(ResolutionScale::lower_factor(4), 3);
    }
}
