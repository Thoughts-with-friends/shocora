use super::{EXECUTE_FRAME, ctx, frame, status};
use crate::hle::test_env::{SCRATCH, TestEnv};

const WORK: u32 = SCRATCH;
const STREAM: u32 = SCRATCH + 0x1000;
const OUT: u32 = SCRATCH + 0x2000;
const CALLBACK: u32 = 0x0200_1000;
const IMAGE: u32 = SCRATCH + 0x8000;

/// SPS (1280 x 720 baseline, level 3.1), PPS, then one IDR slice.
const STREAM_BYTES: [u8; 28] = [
    0, 0, 0, 1, 0x67, 0x42, 0xC0, 0x1F, 0xDA, 0x01, 0x40, 0x16, 0xE4, //
    0, 0, 0, 1, 0x68, 0xCE, 0x3C, 0x80, //
    0, 0, 0, 1, 0x65, 0x88, 0x84,
];

/// Intent: `H264DECMemoryRequirement` sizes the work memory per level like the console and
/// refuses streams the decoder cannot play.
/// Pass: level 31 (720p) asks for `0x4650 << 10` plus the 0x447 header; profile 88 fails.
#[test]
fn sizes_work_memory_by_level() {
    let mut env = TestEnv::new();
    let result = env.call("h264", "H264DECMemoryRequirement", &[66, 31, 1280, 720, OUT]);
    assert_eq!(result, status::SUCCESS);
    assert_eq!(env.memory.read_u32(OUT).unwrap(), (0x4650 << 10) + 0x447);
    let refused = env.call("h264", "H264DECMemoryRequirement", &[88, 31, 1280, 720, OUT]);
    assert_eq!(refused, status::INVALID_PARAM);
}

/// Intent: the header helpers find the SPS and its picture size in guest memory.
/// Pass: the decode start is the four-byte start code at 0 and the size is 1280 x 720.
#[test]
fn finds_the_start_point_and_image_size() {
    let mut env = TestEnv::new();
    env.memory.write_bytes(STREAM, &STREAM_BYTES).unwrap();
    let len = STREAM_BYTES.len() as u32;
    assert_eq!(env.call("h264", "H264DECFindDecstartpoint", &[STREAM, len, OUT]), 0);
    assert_eq!(env.memory.read_u32(OUT).unwrap(), 0);
    let args = [STREAM, len, 0, OUT, OUT + 4];
    assert_eq!(env.call("h264", "H264DECGetImageSize", &args), status::SUCCESS);
    assert_eq!(env.memory.read_u32(OUT).unwrap(), 1280);
    assert_eq!(env.memory.read_u32(OUT + 4).unwrap(), 720);
}

/// Intent: `H264DECExecute` runs the title's output callback with one frame record, then
/// returns "frame produced" through the return stub to the original caller.
/// Pass: the callback gets r3 = the callback struct (count 1, frame pointer, user value),
/// the frame has result 100, the stream's size, a 256-aligned pitch and the image
/// pointer; after the stub, r3 is `0x80 | 100` and `lr` is the caller's again.
#[test]
fn execute_hands_a_frame_to_the_output_callback() {
    let mut env = TestEnv::new();
    env.memory.write_bytes(STREAM, &STREAM_BYTES).unwrap();
    env.call("h264", "H264DECInitParam", &[0x1000, WORK]);
    env.call("h264", "H264DECSetParam", &[WORK, 1, CALLBACK]);
    env.call("h264", "H264DECSetParam", &[WORK, 0x7000_0001, 0x1234]);
    env.call("h264", "H264DECOpen", &[WORK]);
    env.call("h264", "H264DECBegin", &[WORK]);
    env.cpu.fpr[1].ps0 = 2.5;
    env.call("h264", "H264DECSetBitstream", &[WORK, STREAM, STREAM_BYTES.len() as u32]);

    env.cpu.lr = 0x0200_0040;
    let arg = env.call("h264", "H264DECExecute", &[WORK, IMAGE]);
    assert_eq!(env.cpu.pc, CALLBACK);
    assert_eq!(arg, WORK + ctx::CALLBACK_ARG);
    let read = |env: &TestEnv, at: u32| env.memory.read_u32(at).unwrap();
    assert_eq!(read(&env, arg), 1);
    assert_eq!(read(&env, arg + 8), 0x1234);
    let record = read(&env, read(&env, arg + 4));
    assert_eq!(read(&env, record + frame::RESULT), 100);
    assert_eq!(env.memory.read_f64(record + frame::TIMESTAMP).unwrap(), 2.5);
    assert_eq!(read(&env, record + frame::WIDTH), 1280);
    assert_eq!(read(&env, record + frame::HEIGHT), 720);
    assert_eq!(read(&env, record + frame::BYTES_PER_ROW), 1280);
    assert_eq!(read(&env, record + frame::IMAGE), IMAGE);

    let result = env.call("shocora", "__shocora_H264OutputReturn", &[0]);
    assert_eq!(result, EXECUTE_FRAME);
    assert_eq!(env.cpu.lr, 0x0200_0040);
}

/// Intent: after a second `H264DECBegin` (a seek), frames before the next IDR slice are
/// refused, as Cemu does.
/// Pass: a non-IDR slice returns 0x400 and the callback is not entered.
#[test]
fn a_restart_waits_for_an_idr_slice() {
    let mut env = TestEnv::new();
    let p_slice = [0, 0, 0, 1, 0x41, 0x9A, 0x00];
    env.memory.write_bytes(STREAM, &p_slice).unwrap();
    env.call("h264", "H264DECInitParam", &[0x1000, WORK]);
    env.call("h264", "H264DECSetParam", &[WORK, 1, CALLBACK]);
    env.call("h264", "H264DECBegin", &[WORK]);
    env.call("h264", "H264DECBegin", &[WORK]);
    env.call("h264", "H264DECSetBitstream", &[WORK, STREAM, p_slice.len() as u32]);
    env.cpu.pc = 0;
    assert_eq!(env.call("h264", "H264DECExecute", &[WORK, IMAGE]), 0x400);
    assert_ne!(env.cpu.pc, CALLBACK);
}
