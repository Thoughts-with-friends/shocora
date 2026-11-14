//! End-to-end: RPX → loader → interpreter → HLE → process exit.
#![allow(clippy::unwrap_used, reason = "test helpers")]
use wiiu_emu::{Config, Emulator, RenderCommand, Status};
use wiiu_rpl::builder::RplBuilder;

const BL: u32 = 0x4800_0001;

fn run(rpx: &[u8]) -> Emulator {
    let mut emu = Emulator::new(Config::default()).unwrap();
    emu.load_rpx("test.rpx", rpx).unwrap();

    for _ in 0..10 {
        emu.run_frame();
        if *emu.status() != Status::Running {
            break;
        }
    }

    emu
}

#[test]
fn hello_world_through_os_report_and_exit() {
    let rpx = RplBuilder::new()
        .text(&[
            0x3C60_0000, // lis r3, fmt@ha
            0x3863_0000, // addi r3, r3, fmt@l
            0x3880_002A, // li r4, 42
            BL,          // bl OSReport
            0x3860_0007, // li r3, 7
            BL,          // bl exit
            0x4800_0000, // b .
        ])
        .data(b"value=%d\n\0")
        .load_data_address(0, 4, 0)
        .call_import(12, "coreinit", "OSReport")
        .call_import(20, "coreinit", "exit")
        .compress_text()
        .build();

    let mut emu = run(&rpx);
    assert_eq!(*emu.status(), Status::Exited(7));
    assert_eq!(emu.take_console(), ["value=42\n"]);
}

#[test]
fn reset_boots_the_same_program_again_and_tells_the_renderer() {
    let rpx = RplBuilder::new()
        .text(&[
            0x3860_0007, // li r3, 7
            BL,          // bl exit
            0x4800_0000, // b .
        ])
        .call_import(4, "coreinit", "exit")
        .build();

    let mut emu = run(&rpx);
    assert_eq!(*emu.status(), Status::Exited(7));
    emu.take_render_commands();

    assert!(emu.reset().unwrap());
    assert_eq!(*emu.status(), Status::Running);
    assert!(matches!(emu.take_render_commands().first(), Some(RenderCommand::Reset)));

    for _ in 0..10 {
        emu.run_frame();
    }
    assert_eq!(*emu.status(), Status::Exited(7));

    emu.stop();
    assert!(!emu.reset().unwrap());
}

#[test]
fn returning_from_entry_ends_the_process() {
    let rpx = RplBuilder::new()
        .text(&[
            0x9421_FFF0, // stwu r1, -16(r1)
            0x7C08_02A6, // mflr r0
            0x9001_0014, // stw r0, 20(r1)
            BL,          // bl OSGetCoreId
            0x8001_0014, // lwz r0, 20(r1)
            0x7C08_03A6, // mtlr r0
            0x3821_0010, // addi r1, r1, 16
            0x4E80_0020, // blr
        ])
        .call_import(12, "coreinit", "OSGetCoreId")
        .build();

    let emu = run(&rpx);
    assert_eq!(*emu.status(), Status::Exited(1));
    assert!(emu.stats().hle_calls >= 2);
}

#[test]
fn unimplemented_imports_return_zero() {
    let rpx = RplBuilder::new()
        .text(&[
            0x3860_0005, // li r3, 5
            BL,          // bl NotImplemented → r3 = 0
            BL,          // bl exit(r3)
        ])
        .call_import(4, "nn_ac", "ACGetStatus")
        .call_import(8, "coreinit", "exit")
        .build();

    assert_eq!(*run(&rpx).status(), Status::Exited(0));
}

#[test]
fn threads_run_and_join() {
    // .data: OSThread at +0, exit value at +0x700, stack [+0x1000, +0x2000).
    let rpx = RplBuilder::new()
        .text(&[
            0x3C60_0000, // 00 lis r3, thread@ha
            0x3863_0000, // 04 addi r3, r3, thread@l
            0x4800_0005, // 08 bl 0x0C
            0x7C88_02A6, // 0C mflr r4
            0x3884_005C, // 10 addi r4, r4, entry - 0x0C
            0x38A0_0000, // 14 li r5, 0 (argc)
            0x38C0_0000, // 18 li r6, 0 (argv)
            0x3CE0_0000, // 1C lis r7, stack_top@ha
            0x38E7_0000, // 20 addi r7, r7, stack_top@l
            0x3900_1000, // 24 li r8, 0x1000 (stack size)
            0x3920_0010, // 28 li r9, 16 (priority)
            0x3940_0002, // 2C li r10, 2 (core 1)
            BL,          // 30 bl OSCreateThread
            0x3C60_0000, // 34 lis r3, thread@ha
            0x3863_0000, // 38 addi r3, r3, thread@l
            BL,          // 3C bl OSResumeThread
            0x3C60_0000, // 40 lis r3, thread@ha
            0x3863_0000, // 44 addi r3, r3, thread@l
            0x3C80_0000, // 48 lis r4, exit@ha
            0x3884_0000, // 4C addi r4, r4, exit@l
            BL,          // 50 bl OSJoinThread
            0x3C60_0000, // 54 lis r3, exit@ha
            0x3863_0000, // 58 addi r3, r3, exit@l
            0x8063_0000, // 5C lwz r3, 0(r3)
            BL,          // 60 bl exit
            0x4800_0000, // 64 b .
            0x3860_0005, // 68 entry: li r3, 5
            0x4E80_0020, // 6C blr (returns into the thread-exit stub)
        ])
        .data(&[0; 0x2000])
        .load_data_address(0x00, 0x04, 0)
        .load_data_address(0x1C, 0x20, 0x2000)
        .load_data_address(0x34, 0x38, 0)
        .load_data_address(0x40, 0x44, 0)
        .load_data_address(0x48, 0x4C, 0x700)
        .load_data_address(0x54, 0x58, 0x700)
        .call_import(0x30, "coreinit", "OSCreateThread")
        .call_import(0x3C, "coreinit", "OSResumeThread")
        .call_import(0x50, "coreinit", "OSJoinThread")
        .call_import(0x60, "coreinit", "exit")
        .build();
    assert_eq!(*run(&rpx).status(), Status::Exited(5));
}

#[test]
fn bad_instruction_crashes_with_disassembly() {
    let rpx = RplBuilder::new().text(&[0x0000_0000]).build();
    let emu = run(&rpx);
    let Status::Crashed(message) = emu.status() else {
        panic!("expected crash, got {:?}", emu.status())
    };
    assert!(message.contains("invalid instruction"), "{message}");
}
