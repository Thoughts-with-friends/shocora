//! Boots the developer's own title (`shocora.toml`) for a few seconds; passes without one.
//!
//! Run with `cargo test -p wiiu_emu --test rom_boot -- --nocapture` to see what the title
//! did. The save is a copy in a temporary folder: the real one is never written.
#![allow(clippy::unwrap_used, reason = "test")]
use wiiu_emu::{Config, Emulator, Status};
use wiiu_test_roms::{report, require};

/// Frames to run: three seconds at 60 Hz.
const FRAMES: u32 = 180;

/// Intent: the whole chain up to the title's own code: open the title, mount its volumes
/// and save, load the `.rpx`, start the main thread on three cores and run it with HLE
/// system calls, for three seconds.
/// Pass: the title is still running afterwards (no guest crash, no exit), its code ran
/// (instructions and HLE calls counted), and it read from its volumes (an `FS` call is
/// among the HLE calls).
#[test]
fn boots_the_title() {
    let title = require!(wiiu_test_roms::title(), "no title");
    let save_dir = wiiu_test_roms::temp_save_dir(&title, "rom-boot");

    let config = Config { save_dir: save_dir.clone(), profile: true, ..Config::default() };
    let mut emu = Emulator::new(config).unwrap();
    emu.load_title(&title).unwrap();

    for _ in 0..FRAMES {
        emu.run_frame();
        if *emu.status() != Status::Running {
            break;
        }
    }

    let status = emu.status().clone();
    let stats = emu.stats();
    let profile = emu.hle_profile(12);

    report("status", format!("{status:?}"));
    report("frames / instructions", format!("{} / {}", stats.frames, stats.instructions));
    report("HLE calls", stats.hle_calls);
    report(
        "GPU packets / draws / flips",
        format!("{} / {} / {}", stats.gpu.packets, stats.gpu.draws, stats.gpu.flips),
    );

    for (name, calls) in &profile {
        report("  called", format!("{calls:>9}  {name}"));
    }

    for line in emu.take_console().iter().take(8) {
        report("  OSReport", line.trim_end());
    }

    let all = emu.hle_profile(usize::MAX);
    emu.stop();

    let _ = std::fs::remove_dir_all(&save_dir);

    assert_eq!(status, Status::Running);
    assert!(stats.instructions > 0 && stats.hle_calls > 0);
    assert!(all.iter().any(|(name, _)| name.contains("::FS")), "the title read its volumes");
}
