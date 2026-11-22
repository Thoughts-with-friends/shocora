//! WGSL generation.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                                        ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ parsed shader      │   │ shader/emit.rs           │   │ WGSL source text   │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ WGSL generation          │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Writes the translated shader as WGSL text. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.
#![allow(clippy::too_many_lines, clippy::format_push_string)]

use std::{collections::BTreeSet, fmt::Write as _};

use super::{
    TEXTURE_UNITS, TextureDim, TextureKind, TextureSignature, TranslatedShader, WGSL_PRELUDE,
    cf::{
        AluClause, AluGroup, AluKind, AluOp, Cf, Export, StreamWrite, TexOp, VFETCH, parse_alu,
        parse_cf, parse_tex,
    },
    fetch::{Attribute, format},
};

const CHANNEL: [char; 4] = ['x', 'y', 'z', 'w'];

/// ALU source selects.
mod sel {
    pub const KCACHE0: u32 = 128;
    pub const KCACHE1: u32 = 160;
    pub const ZERO: u32 = 248;
    pub const ONE: u32 = 249;
    pub const ONE_INT: u32 = 250;
    pub const MINUS_ONE_INT: u32 = 251;
    pub const HALF: u32 = 252;
    pub const LITERAL: u32 = 253;
    pub const PV: u32 = 254;
    pub const PS: u32 = 255;
    pub const CFILE: u32 = 256;
}

/// TEX opcodes.
mod tex {
    pub const LD: u32 = 0x03;
    pub const GET_TEXTURE_RESINFO: u32 = 0x04;
    pub const GET_COMP_TEX_LOD: u32 = 0x06;
    pub const GET_GRADIENTS_H: u32 = 0x07;
    pub const GET_GRADIENTS_V: u32 = 0x08;
    pub const SET_GRADIENTS_H: u32 = 0x0B;
    pub const SET_GRADIENTS_V: u32 = 0x0C;
    pub const SET_CUBEMAP_INDEX: u32 = 0x0E;
    pub const FETCH4: u32 = 0x0F;
    pub const SAMPLE: u32 = 0x10;
    pub const SAMPLE_L: u32 = 0x11;
    pub const SAMPLE_LB: u32 = 0x12;
    pub const SAMPLE_LZ: u32 = 0x13;
    pub const SAMPLE_G: u32 = 0x14;
    pub const SAMPLE_C: u32 = 0x18;
    pub const SAMPLE_C_L: u32 = 0x19;
    pub const SAMPLE_C_LZ: u32 = 0x1B;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Vertex,
    Pixel,
}

impl Stage {
    const fn prefix(self) -> &'static str {
        match self {
            Self::Vertex => "vs",
            Self::Pixel => "ps",
        }
    }
}

/// Shared state of a translation.
/// Vertex shader entry point of `RECTS` draws.
pub const RECT_ENTRY_POINT: &str = "main_rect";
/// Marks an index as a generated corner of the rect at the arena word in the low bits.
pub const RECT_CORNER_FLAG: u32 = 0x8000_0000;
/// Which vertex of the rect's second triangle the index stands for.
pub const RECT_CORNER_SHIFT: u32 = 29;
const RECT_WORD_MASK: u32 = (1 << RECT_CORNER_SHIFT) - 1;

struct Translator<'a> {
    stage: Stage,
    code: &'a [u8],
    textures: &'a [TextureSignature; TEXTURE_UNITS],
    body: String,
    max_gpr: u32,
    relative_gpr: bool,
    next_temp: u32,
    used_samplers: BTreeSet<(u8, u8)>,
    used_textures: BTreeSet<u8>,
    color_exports: u8,
    writes_depth: bool,
    /// Params exported by a vertex shader.
    param_exports: BTreeSet<u32>,
    /// Ids of the loops open at this point. Each loop has an id of its own, because two
    /// loops one after another share a scope and cannot both declare `loop_sp0`.
    open_loops: Vec<u32>,
    loops_emitted: u32,
    uses_stack: bool,
    /// Writes stream-out buffers (`MEM_STREAM*_WRITE`).
    stream_out: bool,
    /// See [`TranslatedShader::ubo_vec4s`].
    ubo_vec4s: [u32; 16],
}

/// Register state a pixel shader depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelInputs<'a> {
    /// `SPI_PS_IN_CONTROL_0`.
    pub control0: u32,
    /// `SPI_PS_IN_CONTROL_1`.
    pub control1: u32,
    /// `SPI_PS_INPUT_CNTL_N`.
    pub input_controls: &'a [u32],
    /// Semantic of each parameter the vertex shader exports (`SPI_VS_OUT_ID_N`), by index.
    pub vs_out_semantics: &'a [u32],
    /// Parameter indices the vertex shader actually exports.
    pub vs_exports: u32,
    /// `SX_ALPHA_TEST_CONTROL` compare function when alpha test is on.
    pub alpha_function: Option<u32>,
    /// Sample type of each bound color target (`Uint`/`Sint` need integer outputs).
    pub target_kinds: [TextureKind; 8],
}

#[cfg(test)]
#[path = "emit_tests.rs"]
mod tests;

mod control;
mod entry;
mod fetches;
mod operands;
#[allow(unused_imports)]
pub use control::*;
#[allow(unused_imports)]
pub use entry::*;
#[allow(unused_imports)]
pub use fetches::*;
#[allow(unused_imports)]
pub use operands::*;
