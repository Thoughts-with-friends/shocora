//! Latte register file, addressed in dwords.

/// First dword of each register space targeted by the `SET_*` packets.
pub mod base {
    pub const CONFIG: u32 = 0x2000;
    pub const ALU_CONST: u32 = 0xC000;
    pub const CONTEXT: u32 = 0xA000;
    pub const RESOURCE: u32 = 0xE000;
    pub const SAMPLER: u32 = 0xF000;
    pub const CTL_CONST: u32 = 0xF3FC;
    pub const LOOP_CONST: u32 = 0xF880;
    pub const BOOL_CONST: u32 = 0xF980;
}

/// Registers in the config space.
pub mod config {
    pub const VGT_PRIMITIVE_TYPE: u32 = 0x2256;
    pub const SQ_CONFIG: u32 = 0x2300;
    pub const TD_PS_SAMPLER0_BORDER_RED: u32 = 0x2900;
    pub const TD_VS_SAMPLER0_BORDER_RED: u32 = 0x2980;
}

/// Registers in the context space.
pub mod context {
    pub const DB_DEPTH_SIZE: u32 = 0xA000;
    pub const DB_DEPTH_VIEW: u32 = 0xA001;
    pub const DB_DEPTH_BASE: u32 = 0xA003;
    pub const DB_DEPTH_INFO: u32 = 0xA004;
    pub const DB_HTILE_DATA_BASE: u32 = 0xA005;
    pub const DB_STENCIL_CLEAR: u32 = 0xA00A;
    pub const DB_DEPTH_CLEAR: u32 = 0xA00B;
    pub const CB_COLOR0_BASE: u32 = 0xA010;
    pub const CB_COLOR0_SIZE: u32 = 0xA018;
    pub const CB_COLOR0_VIEW: u32 = 0xA020;
    pub const CB_COLOR0_INFO: u32 = 0xA028;
    pub const CB_TARGET_MASK: u32 = 0xA08E;
    pub const CB_SHADER_MASK: u32 = 0xA08F;
    pub const PA_SC_GENERIC_SCISSOR_TL: u32 = 0xA090;
    pub const PA_SC_GENERIC_SCISSOR_BR: u32 = 0xA091;
    pub const SQ_VTX_SEMANTIC_0: u32 = 0xA0E0;
    pub const VGT_MULTI_PRIM_IB_RESET_INDX: u32 = 0xA103;
    pub const SX_ALPHA_TEST_CONTROL: u32 = 0xA104;
    pub const CB_BLEND_RED: u32 = 0xA105;
    pub const DB_STENCILREFMASK: u32 = 0xA10C;
    pub const DB_STENCILREFMASK_BF: u32 = 0xA10D;
    pub const SX_ALPHA_REF: u32 = 0xA10E;
    pub const PA_CL_VPORT_XSCALE: u32 = 0xA10F;
    pub const SPI_VS_OUT_ID_0: u32 = 0xA185;
    pub const SPI_PS_INPUT_CNTL_0: u32 = 0xA191;
    pub const SPI_VS_OUT_CONFIG: u32 = 0xA1B1;
    pub const SPI_PS_IN_CONTROL_0: u32 = 0xA1B3;
    pub const SPI_INPUT_Z: u32 = 0xA1B6;
    pub const CB_BLEND0_CONTROL: u32 = 0xA1E0;
    pub const CB_SHADER_CONTROL: u32 = 0xA1E8;
    pub const DB_DEPTH_CONTROL: u32 = 0xA200;
    pub const CB_COLOR_CONTROL: u32 = 0xA202;
    pub const DB_SHADER_CONTROL: u32 = 0xA203;
    pub const PA_CL_CLIP_CNTL: u32 = 0xA204;
    pub const PA_SU_SC_MODE_CNTL: u32 = 0xA205;
    pub const PA_CL_VTE_CNTL: u32 = 0xA206;
    pub const PA_CL_VS_OUT_CNTL: u32 = 0xA207;
    pub const SQ_PGM_START_PS: u32 = 0xA210;
    pub const SQ_PGM_RESOURCES_PS: u32 = 0xA214;
    pub const SQ_PGM_EXPORTS_PS: u32 = 0xA215;
    pub const SQ_PGM_START_VS: u32 = 0xA216;
    pub const SQ_PGM_RESOURCES_VS: u32 = 0xA21A;
    pub const SQ_PGM_START_ES: u32 = 0xA220;
    pub const SQ_PGM_START_FS: u32 = 0xA225;
    pub const SQ_VTX_SEMANTIC_CLEAR: u32 = 0xA238;
    pub const PA_SU_POINT_SIZE: u32 = 0xA280;
    pub const PA_SU_POINT_MINMAX: u32 = 0xA281;
    pub const VGT_GS_MODE: u32 = 0xA290;
    pub const VGT_PRIMITIVEID_EN: u32 = 0xA2A1;
    pub const VGT_MULTI_PRIM_IB_RESET_EN: u32 = 0xA2A5;
    pub const VGT_INSTANCE_STEP_RATE_0: u32 = 0xA2A8;
    pub const VGT_STRMOUT_EN: u32 = 0xA2AC;
    /// Four registers per stream-out buffer from here: size, stride, base, offset.
    pub const VGT_STRMOUT_BUFFER_SIZE_0: u32 = 0xA2B4;
    pub const VGT_STRMOUT_VTX_STRIDE_0: u32 = 0xA2B5;
    pub const VGT_STRMOUT_BUFFER_BASE_0: u32 = 0xA2B6;
    pub const VGT_STRMOUT_BUFFER_OFFSET_0: u32 = 0xA2B7;
    pub const PA_SU_POLY_OFFSET_CLAMP: u32 = 0xA37F;
    pub const PA_SU_POLY_OFFSET_FRONT_SCALE: u32 = 0xA380;
}

/// `SET_RESOURCE` offsets (seven dwords per unit) relative to [`base::RESOURCE`].
pub mod resource {
    pub const PS_TEXTURE: u32 = 0;
    pub const PS_UNIFORM_BLOCK: u32 = 0x250;
    pub const VS_TEXTURE: u32 = 0x460;
    pub const VS_UNIFORM_BLOCK: u32 = 0x7E0;
    pub const ATTRIBUTE_BLOCK: u32 = 0x8C0;
    pub const GS_TEXTURE: u32 = 0x930;
    pub const GS_UNIFORM_BLOCK: u32 = 0xCB0;
}

/// Number of dword registers.
pub const REGISTER_COUNT: usize = 0x1_0000;

/// All GPU registers.
#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq, Eq)]
pub struct RegisterFile {
    regs: Vec<u32>,
}

impl std::fmt::Debug for RegisterFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let set = self.regs.iter().filter(|&&r| r != 0).count();
        write!(f, "RegisterFile({set} non-zero)")
    }
}

impl Default for RegisterFile {
    fn default() -> Self {
        Self { regs: vec![0; REGISTER_COUNT] }
    }
}

impl RegisterFile {
    #[inline]
    pub fn get(&self, index: u32) -> u32 {
        self.regs.get(index as usize).copied().unwrap_or(0)
    }

    #[inline]
    pub fn set(&mut self, index: u32, value: u32) {
        if let Some(reg) = self.regs.get_mut(index as usize) {
            *reg = value;
        }
    }

    /// Writes consecutive registers starting at `index` (out-of-range writes are dropped).
    #[inline]
    pub fn set_range(&mut self, index: u32, values: &[u32]) {
        let start = (index as usize).min(REGISTER_COUNT);
        let end = (start + values.len()).min(REGISTER_COUNT);
        self.regs[start..end].copy_from_slice(&values[..end - start]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_writes_clip() {
        let mut regs = RegisterFile::default();
        regs.set_range(base::CONTEXT + 4, &[1, 2]);
        assert_eq!(regs.get(base::CONTEXT + 5), 2);
        regs.set_range(0xFFFF, &[7, 8, 9]);
        assert_eq!(regs.get(0xFFFF), 7);
        assert_eq!(regs.get(0x1_0000), 0);
    }
}
