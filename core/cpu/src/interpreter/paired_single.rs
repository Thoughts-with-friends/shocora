//! Paired singles and quantized loads/stores (`psq_l`/`psq_st`).
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                         ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ paired-single      │   │ interpreter/paired_singl │   │ results            │
//! │ instr.             │──►│ e.rs                     │──►│                    │
//! │                    │   │ ──────────────────────── │   │                    │
//! │                    │   │ two-at-once float math   │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The Wii U's small SIMD: two float operations at once, heavily used for 3D math. Terms such
//! as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]

use super::{
    ExecResult, Fault,
    float::{arith, compare, round_single, with_sign},
    unimplemented,
};
use crate::{Bus, CpuState, Fpr, Instr, Opcode, StepEvent};

/// Quantization type field of a GQR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuantType {
    F32,
    U8,
    U16,
    S8,
    S16,
}

impl QuantType {
    const fn from_bits(bits: u32) -> Self {
        match bits & 7 {
            4 => Self::U8,
            5 => Self::U16,
            6 => Self::S8,
            7 => Self::S16,
            _ => Self::F32,
        }
    }

    const fn size(self) -> u32 {
        match self {
            Self::F32 => 4,
            Self::U16 | Self::S16 => 2,
            Self::U8 | Self::S8 => 1,
        }
    }
}

/// Load (or store) half of a GQR: type plus a 6-bit signed scale.
#[derive(Debug, Clone, Copy)]
struct Quant {
    ty: QuantType,
    scale: i32,
}

impl Quant {
    const fn load(gqr: u32) -> Self {
        Self {
            ty: QuantType::from_bits(gqr >> 16),
            scale: (((gqr >> 24) & 0x3F) << 26) as i32 >> 26,
        }
    }

    const fn store(gqr: u32) -> Self {
        Self { ty: QuantType::from_bits(gqr), scale: (((gqr >> 8) & 0x3F) << 26) as i32 >> 26 }
    }

    fn read<B: Bus>(self, bus: &mut B, ea: u32) -> Result<f64, Fault> {
        let raw = match self.ty {
            QuantType::F32 => return Ok(f64::from(f32::from_bits(bus.read_u32(ea)?))),
            QuantType::U8 => f64::from(bus.read_u8(ea)?),
            QuantType::S8 => f64::from(bus.read_u8(ea)? as i8),
            QuantType::U16 => f64::from(bus.read_u16(ea)?),
            QuantType::S16 => f64::from(bus.read_u16(ea)? as i16),
        };
        Ok(raw * 2_f64.powi(-self.scale))
    }

    fn write<B: Bus>(self, bus: &mut B, ea: u32, value: f64) -> Result<(), Fault> {
        let scaled = value * 2_f64.powi(self.scale);
        match self.ty {
            QuantType::F32 => bus.write_u32(ea, (value as f32).to_bits())?,
            QuantType::U8 => bus.write_u8(ea, scaled.clamp(0.0, 255.0) as u8)?,
            QuantType::S8 => bus.write_u8(ea, scaled.clamp(-128.0, 127.0) as i8 as u8)?,
            QuantType::U16 => bus.write_u16(ea, scaled.clamp(0.0, 65535.0) as u16)?,
            QuantType::S16 => bus.write_u16(ea, scaled.clamp(-32768.0, 32767.0) as i16 as u16)?,
        }
        Ok(())
    }
}

/// Decoded operands of the four quantized load/store addressing forms.
struct PsqOperands {
    ea: u32,
    single: bool,
    gqr: usize,
    update: bool,
}

fn psq_operands(op: Opcode, i: Instr, s: &CpuState) -> PsqOperands {
    use Opcode as O;
    match op {
        O::PsqL | O::PsqSt | O::PsqLu | O::PsqStu => {
            let update = matches!(op, O::PsqLu | O::PsqStu);
            let base = if update { s.gpr[i.ra()] } else { s.ra_or_zero(i.ra()) };
            PsqOperands {
                ea: base.wrapping_add(i.psq_d() as u32),
                single: i.psq_w(),
                gqr: i.psq_i(),
                update,
            }
        }
        _ => {
            let update = matches!(op, O::PsqLux | O::PsqStux);
            let base = if update { s.gpr[i.ra()] } else { s.ra_or_zero(i.ra()) };
            PsqOperands {
                ea: base.wrapping_add(s.gpr[i.rb()]),
                single: i.psqx_w(),
                gqr: i.psqx_i(),
                update,
            }
        }
    }
}

fn set_ps(s: &mut CpuState, d: usize, ps0: f64, ps1: f64) {
    s.fpr[d] = Fpr { ps0: round_single(ps0), ps1: round_single(ps1) };
}

pub(super) fn execute<B: Bus>(op: Opcode, i: Instr, s: &mut CpuState, bus: &mut B) -> ExecResult {
    use Opcode as O;
    let (d, ra, rb, rc) = (i.rd(), i.ra(), i.rb(), i.rc_reg());
    let (a, b, c) = (s.fpr[ra], s.fpr[rb], s.fpr[rc]);
    match op {
        O::PsqL | O::PsqLu | O::PsqLx | O::PsqLux => {
            let ops = psq_operands(op, i, s);
            let quant = Quant::load(s.gqr[ops.gqr]);
            let ps0 = quant.read(bus, ops.ea)?;
            let ps1 = if ops.single {
                1.0
            } else {
                quant.read(bus, ops.ea.wrapping_add(quant.ty.size()))?
            };
            s.fpr[d] = Fpr { ps0, ps1 };
            if ops.update {
                s.gpr[ra] = ops.ea;
            }
        }
        O::PsqSt | O::PsqStu | O::PsqStx | O::PsqStux => {
            let ops = psq_operands(op, i, s);
            let quant = Quant::store(s.gqr[ops.gqr]);
            let value = s.fpr[d];
            quant.write(bus, ops.ea, value.ps0)?;
            if !ops.single {
                quant.write(bus, ops.ea.wrapping_add(quant.ty.size()), value.ps1)?;
            }
            if ops.update {
                s.gpr[ra] = ops.ea;
            }
        }
        O::PsAdd
        | O::PsSub
        | O::PsMul
        | O::PsDiv
        | O::PsMadd
        | O::PsMsub
        | O::PsNmadd
        | O::PsNmsub
        | O::PsSel
        | O::PsRes
        | O::PsRsqrte => {
            set_ps(s, d, arith(op, a.ps0, b.ps0, c.ps0), arith(op, a.ps1, b.ps1, c.ps1));
        }
        O::PsSum0 => set_ps(s, d, a.ps0 + b.ps1, c.ps1),
        O::PsSum1 => set_ps(s, d, c.ps0, a.ps0 + b.ps1),
        O::PsMuls0 => set_ps(s, d, a.ps0 * c.ps0, a.ps1 * c.ps0),
        O::PsMuls1 => set_ps(s, d, a.ps0 * c.ps1, a.ps1 * c.ps1),
        O::PsMadds0 => set_ps(s, d, a.ps0.mul_add(c.ps0, b.ps0), a.ps1.mul_add(c.ps0, b.ps1)),
        O::PsMadds1 => set_ps(s, d, a.ps0.mul_add(c.ps1, b.ps0), a.ps1.mul_add(c.ps1, b.ps1)),
        O::PsMerge00 => s.fpr[d] = Fpr { ps0: a.ps0, ps1: b.ps0 },
        O::PsMerge01 => s.fpr[d] = Fpr { ps0: a.ps0, ps1: b.ps1 },
        O::PsMerge10 => s.fpr[d] = Fpr { ps0: a.ps1, ps1: b.ps0 },
        O::PsMerge11 => s.fpr[d] = Fpr { ps0: a.ps1, ps1: b.ps1 },
        O::PsMr => s.fpr[d] = b,
        O::PsNeg | O::PsAbs | O::PsNabs => {
            s.fpr[d] = Fpr { ps0: with_sign(b.ps0, op), ps1: with_sign(b.ps1, op) };
        }
        O::PsCmpu0 | O::PsCmpo0 => compare(s, i.crfd(), a.ps0, b.ps0),
        O::PsCmpu1 | O::PsCmpo1 => compare(s, i.crfd(), a.ps1, b.ps1),
        O::DcbzL => {
            let line = s.ra_or_zero(ra).wrapping_add(s.gpr[rb]) & !31;
            for offset in (0..32).step_by(8) {
                bus.write_u64(line + offset, 0)?;
            }
        }
        _ => return unimplemented(op),
    }
    let records = !matches!(
        op,
        O::PsqL
            | O::PsqLu
            | O::PsqLx
            | O::PsqLux
            | O::PsqSt
            | O::PsqStu
            | O::PsqStx
            | O::PsqStux
            | O::DcbzL
    );
    if records && i.rc() {
        s.update_cr1();
    }
    Ok(StepEvent::Continue)
}

#[cfg(test)]
mod tests {
    use crate::{
        CpuState,
        interpreter::{asm::a, step, test_bus::TestBus},
    };

    fn run_one(s: &mut CpuState, bus: &mut TestBus, raw: u32) {
        bus.load_program(0x100, &[raw]);
        s.pc = 0x100;
        step(s, bus).unwrap();
    }

    #[test]
    fn quantized_s16_load_with_scale() {
        let mut bus = TestBus::new();
        let mut s = CpuState::default();
        bus.mem[0x2000..0x2004].copy_from_slice(&[0x01, 0x00, 0xFF, 0x00]); // 256, -256
        s.gpr[3] = 0x2000;
        s.gqr[2] = (8 << 24) | (7 << 16); // load: s16, scale 8
        // psq_l f1, 0(r3), 0, qr2
        run_one(&mut s, &mut bus, (56 << 26) | (1 << 21) | (3 << 16) | (2 << 12));
        assert_eq!((s.fpr[1].ps0, s.fpr[1].ps1), (1.0, -1.0));
    }

    #[test]
    fn float_pair_store_round_trip() {
        let mut bus = TestBus::new();
        let mut s = CpuState::default();
        s.gpr[3] = 0x2000;
        s.fpr[1].ps0 = 0.5;
        s.fpr[1].ps1 = 2.0;
        run_one(&mut s, &mut bus, (60 << 26) | (1 << 21) | (3 << 16)); // psq_st f1, 0(r3), 0, qr0
        run_one(&mut s, &mut bus, (56 << 26) | (2 << 21) | (3 << 16)); // psq_l f2, 0(r3), 0, qr0
        assert_eq!(s.fpr[2], s.fpr[1]);
    }

    #[test]
    fn arithmetic_and_merges() {
        let mut bus = TestBus::new();
        let mut s = CpuState::default();
        s.fpr[2] = crate::Fpr { ps0: 1.0, ps1: 2.0 };
        s.fpr[3] = crate::Fpr { ps0: 10.0, ps1: 20.0 };
        run_one(&mut s, &mut bus, a(4, 1, 2, 3, 0, 21)); // ps_add f1, f2, f3
        assert_eq!((s.fpr[1].ps0, s.fpr[1].ps1), (11.0, 22.0));
        run_one(&mut s, &mut bus, (4 << 26) | (1 << 21) | (2 << 16) | (3 << 11) | (624 << 1)); // ps_merge11
        assert_eq!((s.fpr[1].ps0, s.fpr[1].ps1), (2.0, 20.0));
        run_one(&mut s, &mut bus, a(4, 1, 2, 3, 3, 10)); // ps_sum0 f1, f2, f3, f3
        assert_eq!((s.fpr[1].ps0, s.fpr[1].ps1), (21.0, 20.0));
    }
}
