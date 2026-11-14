//! PM4 type-3 opcodes.

pub const NOP: u8 = 0x10;
pub const DRAW_INDEX_2: u8 = 0x27;
pub const CONTEXT_CONTROL: u8 = 0x28;
pub const INDEX_TYPE: u8 = 0x2A;
pub const DRAW_INDEX: u8 = 0x2B;
pub const DRAW_INDEX_AUTO: u8 = 0x2D;
pub const DRAW_INDEX_IMMD: u8 = 0x2E;
pub const NUM_INSTANCES: u8 = 0x2F;
pub const INDIRECT_BUFFER: u8 = 0x32;
pub const STRMOUT_BUFFER_UPDATE: u8 = 0x34;
pub const WAIT_REG_MEM: u8 = 0x3C;
pub const MEM_WRITE: u8 = 0x3D;
pub const SURFACE_SYNC: u8 = 0x43;
pub const EVENT_WRITE: u8 = 0x46;
pub const EVENT_WRITE_EOP: u8 = 0x47;
pub const SET_CONFIG_REG: u8 = 0x68;
pub const SET_CONTEXT_REG: u8 = 0x69;
pub const SET_ALU_CONST: u8 = 0x6A;
pub const SET_BOOL_CONST: u8 = 0x6B;
pub const SET_LOOP_CONST: u8 = 0x6C;
pub const SET_RESOURCE: u8 = 0x6D;
pub const SET_SAMPLER: u8 = 0x6E;
pub const SET_CTL_CONST: u8 = 0x6F;

// Emulator-private opcodes (unused by hardware), emitted by the GX2 HLE for
// operations whose real implementation lives in GPU microcode or the display
// controller. Cemu takes the same approach.

/// Words of the surface descriptor the HLE packets carry:
/// `physical address, width, height, depth, pitch, tile mode, swizzle, format,
/// AA mode, (level << 16) | slice`.
pub const SURFACE_WORDS: usize = 10;

/// `[flags, color surface, depth surface, r, g, b, a, depth (f32 bits), stencil]`
pub const HLE_CLEAR: u8 = 0xE0;
/// `[surface, scan target (GX2ScanTarget)]`
pub const HLE_COPY_COLOR_BUFFER_TO_SCAN_BUFFER: u8 = 0xE1;
/// `[]` — flip all scan buffers with pending copies.
pub const HLE_SWAP_SCAN_BUFFERS: u8 = 0xE2;
/// `[0, state]` creates a context state from the current registers,
/// `[1, state]` switches to it (0 keeps the registers as they are).
pub const HLE_CONTEXT_STATE: u8 = 0xE3;
/// `[source surface, destination surface]`
pub const HLE_COPY_SURFACE: u8 = 0xE4;

/// `[slot (0..=7 color, 8 depth), surface]`: the bound render target.
pub const HLE_SET_TARGET: u8 = 0xE5;

/// `flags` of [`HLE_CLEAR`].
pub mod clear {
    pub const COLOR: u32 = 1;
    pub const DEPTH: u32 = 2;
    pub const STENCIL: u32 = 4;
}

/// `GX2ScanTarget` values.
pub mod scan_target {
    pub const TV: u32 = 1;
    pub const DRC: u32 = 4;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Intent: the emulator's own packets (`HLE_*`) use opcodes the Latte command processor
    /// does not, so they can never be confused with the title's packets.
    /// Pass: every HLE opcode is at or above 0xE0, every hardware opcode here below, and
    /// the HLE ones are distinct.
    #[test]
    fn hle_opcodes_stay_out_of_the_hardware_range() {
        let hle = [
            HLE_CLEAR,
            HLE_COPY_COLOR_BUFFER_TO_SCAN_BUFFER,
            HLE_SWAP_SCAN_BUFFERS,
            HLE_CONTEXT_STATE,
            HLE_COPY_SURFACE,
            HLE_SET_TARGET,
        ];
        let hardware =
            [NOP, DRAW_INDEX_2, SET_CONTEXT_REG, SET_ALU_CONST, SET_CTL_CONST, EVENT_WRITE_EOP];
        assert!(hle.iter().all(|op| *op >= 0xE0));
        assert!(hardware.iter().all(|op| *op < 0xE0));
        let mut sorted = hle.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), hle.len());
    }
}
