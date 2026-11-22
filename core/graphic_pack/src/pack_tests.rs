//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!               ▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ pack_tests.rs            │   │ pass / fail        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tests of pack.rs         │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for pack.rs (a graphic pack). They run with `cargo test` only and are not part of
//! the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use std::path::Path;

use super::GraphicPack;

const RULES: &str = "[Definition]\ntitleIds = 0005000010116100,00050000101C4C00\nname = Graphics\n\
    path = \"Xenoblade Chronicles X/Graphics\"\ndescription = A|B\nversion = 6\n\
    [Default]\n$width = 1280\n$height = 720\n$aspect = 1\n\
    [Preset]\ncategory = Aspect\nname = 16:9\n\
    [Preset]\ncategory = Aspect\nname = 21:9\n$aspect = 2\n\
    [Preset]\ncategory = Resolution\nname = 1280x720 (Default)\ncondition = $aspect == 1\ndefault = 1\n\
    [Preset]\ncategory = Resolution\nname = 1920x1080\ncondition = $aspect == 1\n$width = 1920\n$height = 1080\n\
    [Preset]\ncategory = Resolution\nname = 2560x1080\ncondition = $aspect == 2\n$width = 2560\n$height = 1080\n";

/// Intent: a version 6 pack loads with Cemu's preset defaults and condition handling.
/// Pass: titles, description, default selections; choosing 21:9 hides the 16:9
/// resolutions and moves the selection to the only shown one.
#[test]
fn presets_follow_conditions() {
    let mut pack = GraphicPack::parse(Path::new("x/rules.txt"), RULES).unwrap();
    assert!(pack.contains_title_id(0x0005_0000_1011_6100));
    assert_eq!(pack.description, "A\nB");
    assert_eq!(pack.categories(), ["Aspect", "Resolution"]);
    assert_eq!(pack.active_preset("Resolution"), Some("1280x720 (Default)"));
    assert!(pack.set_active_preset("Resolution", "1920x1080", true));
    assert_eq!(pack.expr().eval("$width").unwrap(), 1920.0);
    pack.set_active_preset("Aspect", "21:9", true);
    assert_eq!(pack.active_preset("Resolution"), Some("2560x1080"));
    assert!(!pack.presets.iter().find(|p| p.name == "1920x1080").unwrap().visible);
    assert_eq!(pack.expr().eval("$width / $height > 2").unwrap(), 1.0);
}

/// Intent: packs Cemu refuses are refused.
/// Pass: legacy version, undefined preset variable, missing path all fail.
#[test]
fn rejects_what_cemu_rejects() {
    let p = Path::new("rules.txt");
    assert!(GraphicPack::parse(p, "[Definition]\ntitleIds=1\npath=a\nversion=2\n").is_err());
    assert!(GraphicPack::parse(p, "[Definition]\ntitleIds=1\nversion=6\n").is_err());
    assert!(
        GraphicPack::parse(
            p,
            "[Definition]\ntitleIds=1\npath=a\nversion=6\n[Preset]\nname=x\n$v=1\n"
        )
        .is_err()
    );
    assert!(
        GraphicPack::parse(p, "[Definition]\ntitleIds=*\npath=a\nversion=7\n").unwrap().universal
    );
}
