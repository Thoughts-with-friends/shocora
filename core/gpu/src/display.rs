//! The display controller: flips.
use crate::{RenderCommand, Screen, latte::Latte};

/// Display controller state.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default, Clone)]
pub struct Display {
    flips: u64,
}

impl Display {
    /// Shows every scan buffer copied since the last flip.
    #[inline]
    pub fn swap(&mut self, latte: &mut Latte) {
        latte.commands.push(RenderCommand::Flip);
        latte.end_frame();
        self.flips += 1;
    }

    /// Queues a solid clear of `screen` (used on boot and shutdown).
    #[inline]
    pub fn clear(latte: &mut Latte, screen: Screen, color: [f32; 4]) {
        latte.commands.push(RenderCommand::ClearScreen { screen, color });
    }

    /// Number of flips since boot.
    #[inline]
    pub const fn flips(&self) -> u64 {
        self.flips
    }
}
