//! Raw instruction word with named field accessors.
//!
//! PowerPC numbers bits from the MSB (bit 0) to the LSB (bit 31); the
//! accessors below hide that and return plain values.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                         ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ instruction word   │   │ instr.rs                 │   │ registers,         │
//! │                    │──►│ ──────────────────────── │──►│ constants          │
//! │                    │   │ instruction fields       │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Picks the parts out of an instruction: which registers it uses and which constants. Terms
//! such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.

/// One 32-bit Espresso instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instr(pub u32);

#[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
impl Instr {
    const fn bits(self, shift: u32, mask: u32) -> u32 {
        (self.0 >> shift) & mask
    }

    /// Primary opcode (bits 0..=5).
    #[inline]
    pub const fn opcd(self) -> u32 {
        self.0 >> 26
    }

    /// `rD` / `rS` / `frD` / `frS` / `crbD` / `BO` / `TO`.
    #[inline]
    pub const fn rd(self) -> usize {
        self.bits(21, 31) as usize
    }

    /// `rA` / `frA` / `crbA` / `BI`.
    #[inline]
    pub const fn ra(self) -> usize {
        self.bits(16, 31) as usize
    }

    /// `rB` / `frB` / `crbB` / `SH` / `NB`.
    #[inline]
    pub const fn rb(self) -> usize {
        self.bits(11, 31) as usize
    }

    /// `frC`.
    #[inline]
    pub const fn rc_reg(self) -> usize {
        self.bits(6, 31) as usize
    }

    /// The record bit (`.` suffix): update CR0/CR1.
    #[inline]
    pub const fn rc(self) -> bool {
        self.0 & 1 != 0
    }

    /// Overflow-enable bit of XO-form arithmetic.
    #[inline]
    pub const fn oe(self) -> bool {
        self.bits(10, 1) != 0
    }

    /// 10-bit extended opcode (bits 21..=30).
    #[inline]
    pub const fn xo10(self) -> u32 {
        self.bits(1, 0x3FF)
    }

    /// 9-bit extended opcode of XO-form arithmetic (bits 22..=30).
    #[inline]
    pub const fn xo9(self) -> u32 {
        self.bits(1, 0x1FF)
    }

    /// 6-bit extended opcode (paired-single indexed loads/stores).
    #[inline]
    pub const fn xo6(self) -> u32 {
        self.bits(1, 0x3F)
    }

    /// 5-bit extended opcode of A-form floating point (bits 26..=30).
    #[inline]
    pub const fn xo5(self) -> u32 {
        self.bits(1, 0x1F)
    }

    /// Sign-extended 16-bit immediate.
    #[inline]
    pub const fn simm(self) -> i32 {
        self.0 as u16 as i16 as i32
    }

    /// Zero-extended 16-bit immediate.
    #[inline]
    pub const fn uimm(self) -> u32 {
        self.0 & 0xFFFF
    }

    /// Sign-extended branch displacement of `b` (already multiplied by 4).
    #[inline]
    pub const fn li(self) -> i32 {
        ((self.0 & 0x03FF_FFFC) << 6) as i32 >> 6
    }

    /// Sign-extended branch displacement of `bc` (already multiplied by 4).
    #[inline]
    pub const fn bd(self) -> i32 {
        (self.0 & 0xFFFC) as u16 as i16 as i32
    }

    /// Absolute-address bit of branches.
    #[inline]
    pub const fn aa(self) -> bool {
        self.bits(1, 1) != 0
    }

    /// Link bit of branches.
    #[inline]
    pub const fn lk(self) -> bool {
        self.0 & 1 != 0
    }

    /// Rotate mask begin.
    #[inline]
    pub const fn mb(self) -> u32 {
        self.bits(6, 31)
    }

    /// Rotate mask end.
    #[inline]
    pub const fn me(self) -> u32 {
        self.bits(1, 31)
    }

    /// Shift amount (same field as `rB`).
    #[inline]
    pub const fn sh(self) -> u32 {
        self.bits(11, 31)
    }

    /// Destination CR field of compares / `mcrf`.
    #[inline]
    pub const fn crfd(self) -> usize {
        self.bits(23, 7) as usize
    }

    /// Source CR field of `mcrf` / `mcrfs`.
    #[inline]
    pub const fn crfs(self) -> usize {
        self.bits(18, 7) as usize
    }

    /// SPR / TBR number with its two 5-bit halves swapped back.
    #[inline]
    pub const fn spr(self) -> u32 {
        self.bits(16, 31) | (self.bits(11, 31) << 5)
    }

    /// Field mask of `mtcrf`.
    #[inline]
    pub const fn crm(self) -> u32 {
        self.bits(12, 0xFF)
    }

    /// Field mask of `mtfsf`.
    #[inline]
    pub const fn fm(self) -> u32 {
        self.bits(17, 0xFF)
    }

    /// Immediate of `mtfsfi`.
    #[inline]
    pub const fn fpscr_imm(self) -> u32 {
        self.bits(12, 0xF)
    }

    /// Segment register number of `mfsr`/`mtsr`.
    #[inline]
    pub const fn sr(self) -> usize {
        self.bits(16, 0xF) as usize
    }

    /// Sign-extended 12-bit displacement of `psq_l`/`psq_st`.
    #[inline]
    pub const fn psq_d(self) -> i32 {
        ((self.0 & 0xFFF) << 20) as i32 >> 20
    }

    /// `W` bit of `psq_l`/`psq_st` (load/store only `ps0`).
    #[inline]
    pub const fn psq_w(self) -> bool {
        self.bits(15, 1) != 0
    }

    /// GQR index of `psq_l`/`psq_st`.
    #[inline]
    pub const fn psq_i(self) -> usize {
        self.bits(12, 7) as usize
    }

    /// `W` bit of `psq_lx`/`psq_stx`.
    #[inline]
    pub const fn psqx_w(self) -> bool {
        self.bits(10, 1) != 0
    }

    /// GQR index of `psq_lx`/`psq_stx`.
    #[inline]
    pub const fn psqx_i(self) -> usize {
        self.bits(7, 7) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_fields() {
        // addi r3, r1, -8
        let i = Instr(0x3861_FFF8);
        assert_eq!((i.opcd(), i.rd(), i.ra(), i.simm()), (14, 3, 1, -8));
        // b -4
        assert_eq!(Instr(0x4BFF_FFFC).li(), -4);
        // mflr r0 = mfspr r0, 8
        assert_eq!(Instr(0x7C08_02A6).spr(), 8);
    }
}
