//! Architectural register file of one Espresso core.

/// SPR numbers the interpreter and the OS layer care about.
pub mod spr {
    pub const XER: u32 = 1;
    pub const LR: u32 = 8;
    pub const CTR: u32 = 9;
    pub const DEC: u32 = 22;
    pub const SRR0: u32 = 26;
    pub const SRR1: u32 = 27;
    pub const TBL_READ: u32 = 268;
    pub const TBU_READ: u32 = 269;
    pub const PVR: u32 = 287;
    pub const GQR0: u32 = 912;
    pub const GQR7: u32 = 919;
    pub const HID2: u32 = 920;
    /// Unique processor ID: the core number (0..=2).
    pub const UPIR: u32 = 1007;
}

/// Espresso processor version register value.
pub const ESPRESSO_PVR: u32 = 0x7001_0201;

/// A floating-point register: two paired-single slots.
///
/// Scalar double-precision instructions only use `ps0`. Values are kept as
/// `f64`; raw bit patterns (e.g. `fctiwz` results) are stored with
/// `f64::from_bits` and survive untouched.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub struct Fpr {
    pub ps0: f64,
    pub ps1: f64,
}

impl Fpr {
    /// Both slots set to `value` (single-precision results).
    #[inline]
    pub const fn splat(value: f64) -> Self {
        Self { ps0: value, ps1: value }
    }
}

/// Register file of one core.
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
pub struct CpuState {
    pub gpr: [u32; 32],
    pub fpr: [Fpr; 32],
    /// Address of the next instruction to execute.
    pub pc: u32,
    pub lr: u32,
    pub ctr: u32,
    /// Condition register: eight 4-bit fields, CR0 in the top nibble.
    pub cr: u32,
    pub xer_so: bool,
    pub xer_ov: bool,
    pub xer_ca: bool,
    /// Byte count for `lswx`/`stswx` (XER bits 25..=31).
    pub xer_byte_count: u8,
    pub fpscr: u32,
    pub msr: u32,
    /// Graphics quantization registers used by `psq_l`/`psq_st`.
    pub gqr: [u32; 8],
    pub sr: [u32; 16],
    /// Address reserved by `lwarx` and the word read there, cleared by `stwcx.`.
    pub reservation: Option<(u32, u32)>,
    /// Every other SPR, indexed by SPR number.
    pub spr: Vec<u32>,
    /// The running thread's `OSSetThreadSpecific` slots. They travel with the registers, so
    /// a core reads them without asking the scheduler, as Cafe OS reads its current thread.
    pub thread_specific: [u32; 16],
    /// The running thread's copy of the TLS image (0 until `__tls_get_addr` makes one).
    pub tls_block: u32,
}

impl Clone for CpuState {
    fn clone(&self) -> Self {
        let mut copy = Self::new(0);
        copy.clone_from(self);
        copy
    }

    /// Copies into the SPR storage already there: threads switch cores' registers often,
    /// and a fresh allocation each time cost more than the copy.
    fn clone_from(&mut self, source: &Self) {
        let Self {
            gpr,
            fpr,
            pc,
            lr,
            ctr,
            cr,
            xer_so,
            xer_ov,
            xer_ca,
            xer_byte_count,
            fpscr,
            msr,
            gqr,
            sr,
            reservation,
            spr,
            thread_specific,
            tls_block,
        } = source;
        self.gpr = *gpr;
        self.fpr = *fpr;
        self.pc = *pc;
        self.lr = *lr;
        self.ctr = *ctr;
        self.cr = *cr;
        self.xer_so = *xer_so;
        self.xer_ov = *xer_ov;
        self.xer_ca = *xer_ca;
        self.xer_byte_count = *xer_byte_count;
        self.fpscr = *fpscr;
        self.msr = *msr;
        self.gqr = *gqr;
        self.sr = *sr;
        self.reservation = *reservation;
        self.spr.clone_from(spr);
        self.thread_specific = *thread_specific;
        self.tls_block = *tls_block;
    }
}

impl CpuState {
    /// Reset state of core `core_id`.
    pub fn new(core_id: u32) -> Self {
        let mut spr = vec![0; 1024];
        spr[spr::UPIR as usize] = core_id;
        spr[spr::PVR as usize] = ESPRESSO_PVR;
        // HID2: paired singles enabled, locked cache enabled.
        spr[spr::HID2 as usize] = 0xA000_0000;
        Self {
            gpr: [0; 32],
            fpr: [Fpr::default(); 32],
            pc: 0,
            lr: 0,
            ctr: 0,
            cr: 0,
            xer_so: false,
            xer_ov: false,
            xer_ca: false,
            xer_byte_count: 0,
            fpscr: 0,
            msr: 0,
            gqr: [0; 8],
            sr: [0; 16],
            reservation: None,
            spr,
            thread_specific: [0; 16],
            tls_block: 0,
        }
    }

    #[inline]
    pub fn core_id(&self) -> u32 {
        self.spr[spr::UPIR as usize]
    }

    pub fn xer(&self) -> u32 {
        (u32::from(self.xer_so) << 31)
            | (u32::from(self.xer_ov) << 30)
            | (u32::from(self.xer_ca) << 29)
            | u32::from(self.xer_byte_count)
    }

    #[allow(clippy::cast_possible_truncation)]
    pub const fn set_xer(&mut self, value: u32) {
        self.xer_so = value & (1 << 31) != 0;
        self.xer_ov = value & (1 << 30) != 0;
        self.xer_ca = value & (1 << 29) != 0;
        self.xer_byte_count = (value & 0x7F) as u8;
    }

    /// CR bit `bit`, numbered PowerPC-style (0 = MSB = CR0[LT]).
    #[inline]
    pub const fn cr_bit(&self, bit: usize) -> bool {
        (self.cr >> (31 - bit)) & 1 != 0
    }

    pub const fn set_cr_bit(&mut self, bit: usize, value: bool) {
        let mask = 1 << (31 - bit);
        if value {
            self.cr |= mask;
        } else {
            self.cr &= !mask;
        }
    }

    /// 4-bit CR field `field` (0 = CR0).
    #[inline]
    pub const fn cr_field(&self, field: usize) -> u32 {
        (self.cr >> (28 - 4 * field)) & 0xF
    }

    #[inline]
    pub const fn set_cr_field(&mut self, field: usize, value: u32) {
        let shift = 28 - 4 * field;
        self.cr = (self.cr & !(0xF << shift)) | ((value & 0xF) << shift);
    }

    /// Sets CR0 from a signed comparison of `result` against zero, plus XER[SO].
    #[allow(clippy::cast_possible_wrap)]
    pub const fn update_cr0(&mut self, result: u32) {
        let value = match (result as i32).signum() {
            -1 => 0b1000,
            1 => 0b0100,
            _ => 0b0010,
        };
        self.set_cr_field(0, value | self.xer_so as u32);
    }

    /// Sets CR1 from FPSCR[FX, FEX, VX, OX] (the `.` form of FP instructions).
    #[inline]
    pub const fn update_cr1(&mut self) {
        self.set_cr_field(1, self.fpscr >> 28);
    }

    /// `gpr[ra]`, or 0 when `ra` is r0 (the `(rA|0)` addressing rule).
    #[inline]
    pub const fn ra_or_zero(&self, ra: usize) -> u32 {
        if ra == 0 { 0 } else { self.gpr[ra] }
    }
}

impl Default for CpuState {
    fn default() -> Self {
        Self::new(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cr_field_and_bit_numbering() {
        let mut s = CpuState::new(1);
        s.set_cr_field(0, 0b1000);
        assert!(s.cr_bit(0));
        s.set_cr_field(7, 0b0001);
        assert!(s.cr_bit(31));
        assert_eq!(s.cr, 0x8000_0001);
        s.xer_so = true;
        s.update_cr0(0);
        assert_eq!(s.cr_field(0), 0b0011);
        assert_eq!(s.core_id(), 1);
    }

    #[test]
    fn xer_round_trip() {
        let mut s = CpuState::default();
        s.set_xer(0xA000_0012);
        assert!(s.xer_so && !s.xer_ov && s.xer_ca);
        assert_eq!(s.xer(), 0xA000_0012);
    }
}
