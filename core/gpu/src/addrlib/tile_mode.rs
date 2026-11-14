//! Latte tile modes (`GX2TileMode` values 0..=15 are hardware modes).

pub const TM_LINEAR_GENERAL: u32 = 0;
pub const TM_LINEAR_ALIGNED: u32 = 1;
pub const TM_1D_TILED_THIN1: u32 = 2;
pub const TM_1D_TILED_THICK: u32 = 3;
pub const TM_2D_TILED_THIN1: u32 = 4;
pub const TM_2D_TILED_THIN2: u32 = 5;
pub const TM_2D_TILED_THIN4: u32 = 6;
pub const TM_2D_TILED_THICK: u32 = 7;
pub const TM_2B_TILED_THIN1: u32 = 8;
pub const TM_2B_TILED_THIN2: u32 = 9;
pub const TM_2B_TILED_THIN4: u32 = 10;
pub const TM_2B_TILED_THICK: u32 = 11;
pub const TM_3D_TILED_THIN1: u32 = 12;
pub const TM_3D_TILED_THICK: u32 = 13;
pub const TM_3B_TILED_THIN1: u32 = 14;
pub const TM_3B_TILED_THICK: u32 = 15;
/// GX2-only: linear with no alignment requirements.
pub const TM_LINEAR_SPECIAL: u32 = 16;
/// GX2-only: let GX2 pick a tiled mode.
pub const TM_32_SPECIAL: u32 = 32;

pub const fn is_thick(tile_mode: u32) -> bool {
    matches!(
        tile_mode,
        TM_1D_TILED_THICK
            | TM_2D_TILED_THICK
            | TM_2B_TILED_THICK
            | TM_3D_TILED_THICK
            | TM_3B_TILED_THICK
    )
}

#[inline]
pub const fn thickness(tile_mode: u32) -> u32 {
    if is_thick(tile_mode) { 4 } else { 1 }
}

#[inline]
pub const fn is_macro_tiled(tile_mode: u32) -> bool {
    tile_mode >= 4 && tile_mode != TM_LINEAR_SPECIAL
}

pub const fn is_thick_and_macro_tiled(tile_mode: u32) -> bool {
    matches!(
        tile_mode,
        TM_2D_TILED_THICK | TM_2B_TILED_THICK | TM_3D_TILED_THICK | TM_3B_TILED_THICK
    )
}

pub const fn is_bank_swapped(tile_mode: u32) -> bool {
    matches!(
        tile_mode,
        TM_2B_TILED_THIN1
            | TM_2B_TILED_THIN2
            | TM_2B_TILED_THIN4
            | TM_2B_TILED_THICK
            | TM_3B_TILED_THIN1
            | TM_3B_TILED_THICK
    )
}

pub const fn non_bank_swapped(tile_mode: u32) -> u32 {
    match tile_mode {
        TM_2B_TILED_THIN1 => TM_2D_TILED_THIN1,
        TM_2B_TILED_THIN2 => TM_2D_TILED_THIN2,
        TM_2B_TILED_THIN4 => TM_2D_TILED_THIN4,
        TM_2B_TILED_THICK => TM_2D_TILED_THICK,
        TM_3B_TILED_THIN1 => TM_3D_TILED_THIN1,
        TM_3B_TILED_THICK => TM_3D_TILED_THICK,
        m => m,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Intent: tile-mode predicates drive how surfaces are (de)tiled: thick modes stack 4
    /// slices, 2D/3D modes are macro-tiled, "B" modes swap banks.
    /// Pass: values match the AMD addrlib tables for representative modes.
    #[test]
    fn tile_mode_predicates_match_addrlib() {
        assert!(is_thick(TM_2D_TILED_THICK) && !is_thick(TM_2D_TILED_THIN1));
        assert_eq!(thickness(TM_1D_TILED_THICK), 4);
        assert_eq!(thickness(TM_2D_TILED_THIN1), 1);
        assert!(is_macro_tiled(TM_2D_TILED_THIN1) && !is_macro_tiled(TM_1D_TILED_THIN1));
        assert!(is_bank_swapped(TM_2B_TILED_THIN1) && !is_bank_swapped(TM_2D_TILED_THIN1));
        assert_eq!(non_bank_swapped(TM_2B_TILED_THIN2), TM_2D_TILED_THIN2);
        assert!(!is_macro_tiled(TM_LINEAR_ALIGNED));
    }
}
