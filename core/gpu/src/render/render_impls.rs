//! Part of [`super`]: see its documentation.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                                        ▲▲▲    ▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ GPU decisions      │   │ render/render_impls.rs   │   │ commands for wgpu  │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ work for the renderer:   │   │                    │
//! │                    │   │ render impls             │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "render impls" share of render.rs (work for the renderer), moved here so that no file is
//! longer than 500 lines. It works exactly as if it were still written in that file. The list
//! of jobs handed to shocora's renderer (uploads, draws, copies), independent of
//! DirectX/Vulkan/OpenGL. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;

impl ResolutionScale {
    /// The factors on offer, smallest first.
    pub const FACTORS: [u32; 7] = [1, 2, 3, 4, 5, 6, 8];
    /// Widest picture the host draws: "10K".
    pub const MAX_WIDTH: u32 = 10_240;
    /// Smaller targets hold data rather than pictures: light levels, lookup tables.
    pub(super) const MIN_SIDE: u32 = 32;

    /// The scale for a title showing a TV picture `tv_width` wide, for a `requested` factor.
    #[inline]
    pub const fn for_tv(requested: u32, tv_width: u32) -> Self {
        let most = Self::MAX_WIDTH / if tv_width == 0 { 1 } else { tv_width };
        let factor = if requested < most { requested } else { most };
        Self { factor: if factor == 0 { 1 } else { factor }, limit_width: tv_width }
    }

    /// The factor on offer below `factor`, for when video memory runs out.
    #[inline]
    pub fn lower_factor(factor: u32) -> u32 {
        Self::FACTORS.into_iter().rev().find(|f| *f < factor).unwrap_or(1)
    }

    /// Whether the target `key` is drawn larger. Square targets and arrays are mostly shadow
    /// maps, which would eat the memory of everything else at these factors.
    pub const fn applies(&self, key: &SurfaceKey) -> bool {
        self.factor > 1
            && key.layers == 1
            && key.width != key.height
            && key.width <= self.limit_width
            && key.width >= Self::MIN_SIDE
            && key.height >= Self::MIN_SIDE
    }

    /// The factor of the target `key`.
    #[inline]
    pub const fn of(&self, key: &SurfaceKey) -> u32 {
        if self.applies(key) { self.factor } else { 1 }
    }
}

impl PartialEq for SurfaceReply {
    fn eq(&self, _: &Self) -> bool {
        false
    }
}

impl PartialEq for ReadbackReply {
    fn eq(&self, _: &Self) -> bool {
        false
    }
}
