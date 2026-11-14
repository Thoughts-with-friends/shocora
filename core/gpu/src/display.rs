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

#[cfg(test)]
mod tests {
    use super::*;

    /// Intent: a swap tells the host renderer to present what was copied to the scan
    /// buffers, and counts the flip (the FPS counter).
    /// Pass: `swap` queues `Flip` and the count goes to 1; `clear` queues a screen clear.
    #[test]
    fn swap_queues_a_flip_and_counts_it() {
        let mut latte = Latte::default();
        let mut display = Display::default();
        display.swap(&mut latte);
        assert_eq!(display.flips(), 1);
        assert!(latte.commands.iter().any(|c| matches!(c, RenderCommand::Flip)));
        Display::clear(&mut latte, Screen::Tv, [0.0; 4]);
        assert!(matches!(latte.commands.last(), Some(RenderCommand::ClearScreen { .. })));
    }
}
