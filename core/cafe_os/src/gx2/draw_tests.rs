//! Tests of `draw.rs`, which is over 500 lines.
use wiiu_gpu::command::{opcode, pm4::type3_header};

use crate::hle::test_env::{SCRATCH, TestEnv};

/// Intent: `GX2SetVertexUniformReg` becomes a `SET_ALU_CONST` packet: vertex constants
/// live at 0x400 after the 256 pixel-shader ones.
/// Pass: the command stream ends with the header for 1 + 4 words, offset 0x400 + 8,
/// and the four values in order.
#[test]
fn vertex_uniform_registers_become_alu_constants() {
    let mut env = TestEnv::new();
    for i in 0..4 {
        env.memory.write_u32(SCRATCH + 4 * i, 0x3F80_0000 + i).unwrap();
    }
    env.call("gx2", "GX2SetVertexUniformReg", &[8, 4, SCRATCH]);
    let words = &env.os.gx2.commands;
    let packet = &words[words.len() - 6..];
    assert_eq!(packet[0], type3_header(opcode::SET_ALU_CONST, 5));
    assert_eq!(packet[1], 0x408);
    assert_eq!(&packet[2..], [0x3F80_0000, 0x3F80_0001, 0x3F80_0002, 0x3F80_0003]);
}
