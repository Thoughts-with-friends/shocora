//! Control-flow, ALU and TEX instruction parsing.
#![allow(clippy::cast_possible_truncation)]

/// A control-flow instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cf {
    Tex {
        addr: u32,
        count: u32,
    },
    Alu(AluClause),
    LoopStart,
    LoopEnd,
    LoopBreak,
    Else {
        pop: u32,
    },
    Pop {
        pop: u32,
    },
    Jump {
        pop: u32,
    },
    Export(Export),
    /// `MEM_STREAM*_WRITE`: stores registers in a stream-out buffer.
    StreamWrite(StreamWrite),
    Unsupported(u32),
}

/// What follows an ALU clause (`CF_INST_ALU*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AluKind {
    Plain,
    PushBefore,
    PopAfter,
    Pop2After,
    Continue,
    Break,
    ElseAfter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AluClause {
    pub kind: AluKind,
    pub addr: u32,
    pub count: u32,
    pub kcache_bank: [u32; 2],
    pub kcache_addr: [u32; 2],
}

/// `EXPORT`: which registers go where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Export {
    /// 0 pixel, 1 position, 2 parameter.
    pub kind: u32,
    pub array_base: u32,
    pub gpr: u32,
    pub rel: bool,
    pub burst: u32,
    pub sel: [u32; 4],
}

/// `MEM_STREAM*_WRITE`: components of one register for a stream-out buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamWrite {
    pub buffer: u32,
    /// In dwords from the start of the vertex.
    pub array_base: u32,
    pub array_size: u32,
    pub component_mask: u32,
    pub gpr: u32,
}

const CF_NOP: u32 = 0x00;
const CF_TEX: u32 = 0x01;
const CF_VTX: u32 = 0x02;
const CF_VTX_TC: u32 = 0x03;
const CF_LOOP_START: u32 = 0x04;
const CF_LOOP_END: u32 = 0x05;
const CF_LOOP_START_DX10: u32 = 0x06;
const CF_LOOP_START_NO_AL: u32 = 0x07;
const CF_LOOP_BREAK: u32 = 0x09;
const CF_JUMP: u32 = 0x0A;
const CF_ELSE: u32 = 0x0D;
const CF_POP: u32 = 0x0E;
const CF_CALL_FS: u32 = 0x13;
const CF_RETURN: u32 = 0x14;
const CF_MEM_STREAM0_WRITE: u32 = 0x20;
const CF_MEM_STREAM3_WRITE: u32 = 0x23;
const CF_EXPORT: u32 = 0x27;
const CF_EXPORT_DONE: u32 = 0x28;

/// Reads little-endian 32-bit words of a program.
#[inline]
pub fn word_at(code: &[u8], index: u32) -> u32 {
    let at = index as usize * 4;
    code.get(at..at + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Parses the CF program; stops at `END_OF_PROGRAM`.
pub fn parse_cf(code: &[u8]) -> Vec<Cf> {
    let mut list = Vec::new();
    let limit = (code.len() / 8) as u32;
    for index in 0..limit {
        let word0 = word_at(code, index * 2);
        let word1 = word_at(code, index * 2 + 1);
        let inst = (word1 >> 23) & 0x7F;
        if inst >= 0x40 {
            let kind = match (word1 >> 26) & 0xF {
                0x8 => AluKind::Plain,
                0x9 => AluKind::PushBefore,
                0xA => AluKind::PopAfter,
                0xB => AluKind::Pop2After,
                0xD => AluKind::Continue,
                0xE => AluKind::Break,
                0xF => AluKind::ElseAfter,
                other => {
                    list.push(Cf::Unsupported(0x40 | other));
                    continue;
                }
            };
            list.push(Cf::Alu(AluClause {
                kind,
                addr: word0 & 0x3F_FFFF,
                count: ((word1 >> 18) & 0x7F) + 1,
                kcache_bank: [(word0 >> 22) & 0xF, (word0 >> 26) & 0xF],
                kcache_addr: [(word1 >> 2) & 0xFF, (word1 >> 10) & 0xFF],
            }));
            continue;
        }
        let pop = word1 & 7;
        let end = (word1 >> 21) & 1 != 0;
        match inst {
            CF_NOP | CF_CALL_FS => {}
            CF_TEX => list.push(Cf::Tex {
                addr: word0,
                count: (((word1 >> 10) & 7) | ((word1 >> 16) & 8)) + 1,
            }),
            CF_VTX | CF_VTX_TC => list.push(Cf::Unsupported(inst)),
            CF_LOOP_START | CF_LOOP_START_DX10 | CF_LOOP_START_NO_AL => list.push(Cf::LoopStart),
            CF_LOOP_END => list.push(Cf::LoopEnd),
            CF_LOOP_BREAK => list.push(Cf::LoopBreak),
            CF_JUMP => list.push(Cf::Jump { pop }),
            CF_ELSE => list.push(Cf::Else { pop }),
            CF_POP => list.push(Cf::Pop { pop }),
            CF_RETURN => {
                if end {
                    break;
                }
            }
            CF_EXPORT | CF_EXPORT_DONE => list.push(Cf::Export(Export {
                kind: (word0 >> 13) & 3,
                array_base: word0 & 0x1FFF,
                gpr: (word0 >> 15) & 0x7F,
                rel: (word0 >> 22) & 1 != 0,
                burst: ((word1 >> 17) & 0xF) + 1,
                sel: [word1 & 7, (word1 >> 3) & 7, (word1 >> 6) & 7, (word1 >> 9) & 7],
            })),
            CF_MEM_STREAM0_WRITE..=CF_MEM_STREAM3_WRITE => {
                list.push(Cf::StreamWrite(StreamWrite {
                    buffer: inst - CF_MEM_STREAM0_WRITE,
                    array_base: word0 & 0x1FFF,
                    array_size: (word1 & 0xFFF) + 1,
                    component_mask: (word1 >> 12) & 0xF,
                    gpr: (word0 >> 15) & 0x7F,
                }));
            }
            other => list.push(Cf::Unsupported(other)),
        }
        if end {
            break;
        }
    }
    list
}

/// A source operand of an ALU instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Source {
    pub sel: u32,
    pub rel: bool,
    pub chan: u32,
    pub neg: bool,
    pub abs: bool,
}

/// One ALU micro-instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AluOp {
    pub is_op3: bool,
    pub opcode: u32,
    pub src: [Source; 3],
    pub index_mode: u32,
    pub dest: u32,
    pub dest_rel: bool,
    pub dest_elem: u32,
    pub clamp: bool,
    pub write: bool,
    pub update_exec: bool,
    pub omod: u32,
    /// x, y, z, w or 4 for the transcendental unit.
    pub unit: u32,
}

/// Instructions executed together, seeing the registers as they were before.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AluGroup {
    pub ops: Vec<AluOp>,
    pub literals: [u32; 4],
}

/// Ops only the transcendental unit can execute.
const fn is_trans_op(opcode: u32) -> bool {
    matches!(opcode, 0x61..=0x6F | 0x73..=0x79)
}

const LITERAL: u32 = 0xFD;

/// Parses clause `clause` into instruction groups.
pub fn parse_alu(code: &[u8], clause: &AluClause) -> Vec<AluGroup> {
    let mut groups = Vec::new();
    let mut group = AluGroup::default();
    let mut used_units = 0_u32;
    let mut literal_mask = 0_u32;
    let mut at = 0;
    while at < clause.count {
        let word0 = word_at(code, (clause.addr + at) * 2);
        let word1 = word_at(code, (clause.addr + at) * 2 + 1);
        at += 1;
        let last = word0 >> 31 != 0;
        let source = |sel_shift: u32| Source {
            sel: (word0 >> sel_shift) & 0x1FF,
            rel: (word0 >> (sel_shift + 9)) & 1 != 0,
            chan: (word0 >> (sel_shift + 10)) & 3,
            neg: (word0 >> (sel_shift + 12)) & 1 != 0,
            abs: false,
        };
        let is_op3 = (word1 >> 13) & 0x1F >= 8;
        let mut op = AluOp {
            is_op3,
            src: [source(0), source(13), Source::default()],
            index_mode: (word0 >> 26) & 7,
            dest: (word1 >> 21) & 0x7F,
            dest_rel: (word1 >> 28) & 1 != 0,
            dest_elem: (word1 >> 29) & 3,
            clamp: word1 >> 31 != 0,
            ..AluOp::default()
        };
        if is_op3 {
            op.opcode = (word1 >> 13) & 0x1F;
            op.write = true;
            op.src[2] = Source {
                sel: word1 & 0x1FF,
                rel: (word1 >> 9) & 1 != 0,
                chan: (word1 >> 10) & 3,
                neg: (word1 >> 12) & 1 != 0,
                abs: false,
            };
        } else {
            op.opcode = (word1 >> 7) & 0x7FF;
            op.src[0].abs = word1 & 1 != 0;
            op.src[1].abs = (word1 >> 1) & 1 != 0;
            op.update_exec = (word1 >> 2) & 1 != 0;
            op.write = (word1 >> 4) & 1 != 0;
            op.omod = (word1 >> 5) & 3;
        }
        let operands = if is_op3 { 3 } else { 2 };
        for src in &op.src[..operands] {
            if src.sel == LITERAL {
                literal_mask |= 1 << src.chan;
            }
        }
        // The transcendental unit takes what only it can do, and what finds its vector unit taken.
        let unit = if (!is_op3 && is_trans_op(op.opcode)) || used_units & (1 << op.dest_elem) != 0 {
            4
        } else {
            op.dest_elem
        };
        used_units |= 1 << unit;
        op.unit = unit;
        group.ops.push(op);
        if last {
            if literal_mask != 0 {
                group.literals[0] = word_at(code, (clause.addr + at) * 2);
                group.literals[1] = word_at(code, (clause.addr + at) * 2 + 1);
                if literal_mask & 0b1100 != 0 {
                    group.literals[2] = word_at(code, (clause.addr + at + 1) * 2);
                    group.literals[3] = word_at(code, (clause.addr + at + 1) * 2 + 1);
                    at += 2;
                } else {
                    at += 1;
                }
            }
            groups.push(std::mem::take(&mut group));
            used_units = 0;
            literal_mask = 0;
        }
    }
    groups
}

/// A TEX clause instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TexOp {
    pub opcode: u32,
    pub texture: u32,
    pub sampler: u32,
    pub src: u32,
    pub dest: u32,
    pub dst_sel: [u32; 4],
    pub src_sel: [u32; 4],
    pub normalized: [bool; 4],
    pub offset: [i32; 3],
}

/// `VFETCH` of a TEX clause.
pub const VFETCH: u32 = 0;

/// Parses a TEX clause.
pub fn parse_tex(code: &[u8], addr: u32, count: u32) -> Vec<TexOp> {
    let signed5 = |v: u32| (((v & 0x1F) << 27).cast_signed()) >> 27;
    (0..count)
        .map(|n| {
            let first = addr * 2 + n * 4;
            let (word0, word1, word2) =
                (word_at(code, first), word_at(code, first + 1), word_at(code, first + 2));
            if word0 & 0x1F == VFETCH {
                // Uses the VTX encoding: the buffer is indexed by one source component.
                return TexOp {
                    opcode: VFETCH,
                    texture: (word0 >> 8) & 0xFF,
                    sampler: 0,
                    src: (word0 >> 16) & 0x7F,
                    dest: word1 & 0x7F,
                    dst_sel: [
                        (word1 >> 9) & 7,
                        (word1 >> 12) & 7,
                        (word1 >> 15) & 7,
                        (word1 >> 18) & 7,
                    ],
                    src_sel: [(word0 >> 24) & 3, 0, 0, 0],
                    normalized: [true; 4],
                    offset: [0; 3],
                };
            }
            TexOp {
                opcode: word0 & 0x1F,
                texture: (word0 >> 8) & 0xFF,
                sampler: (word2 >> 15) & 0x1F,
                src: (word0 >> 16) & 0x7F,
                dest: word1 & 0x7F,
                dst_sel: [
                    (word1 >> 9) & 7,
                    (word1 >> 12) & 7,
                    (word1 >> 15) & 7,
                    (word1 >> 18) & 7,
                ],
                src_sel: [
                    (word2 >> 20) & 7,
                    (word2 >> 23) & 7,
                    (word2 >> 26) & 7,
                    (word2 >> 29) & 7,
                ],
                normalized: [
                    (word1 >> 28) & 1 != 0,
                    (word1 >> 29) & 1 != 0,
                    (word1 >> 30) & 1 != 0,
                    word1 >> 31 != 0,
                ],
                offset: [signed5(word2), signed5(word2 >> 5), signed5(word2 >> 10)],
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Intent: the control-flow parser finds the export that ends a shader program and
    /// decodes which register goes where (the translator's map of outputs).
    /// Pass: an `EXPORT_DONE` of GPR 3 to position slot 60, burst 1, swizzle `xyzw`
    /// parses into exactly that one `Cf::Export`.
    #[test]
    fn parses_the_final_export() {
        let word0 = 60 | (1 << 13) | (3 << 15);
        let word1 = (0x28 << 23) | (1 << 21) | (1 << 3) | (2 << 6) | (3 << 9);
        let code: Vec<u8> = [word0, word1].iter().flat_map(|w: &u32| w.to_le_bytes()).collect();
        let list = parse_cf(&code);
        assert_eq!(list.len(), 1);
        let Cf::Export(export) = &list[0] else { panic!("{list:?}") };
        assert_eq!((export.kind, export.array_base, export.gpr), (1, 60, 3));
        assert_eq!((export.burst, export.sel), (1, [0, 1, 2, 3]));
    }
}
