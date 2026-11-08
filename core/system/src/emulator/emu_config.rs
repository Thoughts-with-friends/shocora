use std::{path::PathBuf, time::Duration};

use wiiu_title::update::AddOnKind;

/// Host-side emulator settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Upper bound of guest instructions executed per [`Emulator::run_frame`](crate::Emulator::run_frame).
    pub instructions_per_frame: u64,
    /// Time between vertical blanks.
    pub frame_time: Duration,
    /// Host directory holding per-title save data (`<dir>/<title id>/`).
    pub save_dir: PathBuf,
    /// Translate guest code; off means the (slow, precise-fault) interpreter only.
    pub jit: bool,
    /// Measure host time per HLE function (`SHOCORA_PROFILE`).
    pub profile: bool,
    /// Keep real time: [Emulator::run_frame](crate::Emulator::run_frame) returns at the next
    /// vertical blank. Off, it returns as soon as no guest thread can run.
    pub paced: bool,
    /// Directory holding the system fonts (CafeStd.ttf, ...), if not found automatically.
    pub fonts_dir: Option<PathBuf>,
    /// Vertical blanks per flip, whatever the title asks for. A title whose logic
    /// counts frames runs faster with less than it was made for.
    pub swap_interval: Option<u32>,
    /// How many times larger than on the console the title's pictures are drawn.
    pub resolution_factor: u32,
    /// Least vertical blanks per flip: `2` caps the title at 30 fps, and titles asking for
    /// less than that keep their own rate.
    pub min_swap_interval: Option<u32>,
    /// Host directory holding saved states (`<dir>/<title id>/<slot>.state`).
    pub states_dir: PathBuf,
    /// Host directory holding per-title cheat codes (`<dir>/<title id>.txt`).
    pub cheats_dir: PathBuf,
    /// System storage (`mlc01`) searched in order for installed updates.
    pub mlc_dirs: Vec<PathBuf>,
    /// Folders of updates for any titles (each one, or each of its subfolders, holding
    /// `code/`, `content/`, `meta/`), searched before `mlc_dirs`.
    pub update_dirs: Vec<PathBuf>,
    /// Folders of downloadable content, laid out like `update_dirs`.
    pub dlc_dirs: Vec<PathBuf>,
    /// Let titles that refuse to start without an update start without one.
    pub skip_update_check: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            instructions_per_frame: 400_000_000,
            frame_time: Duration::from_micros(16_683),
            save_dir: PathBuf::from("save"),
            jit: true,
            profile: std::env::var_os("SHOCORA_PROFILE").is_some(),
            paced: true,
            fonts_dir: None,
            swap_interval: None,
            resolution_factor: 1,
            min_swap_interval: None,
            states_dir: PathBuf::from("states"),
            cheats_dir: PathBuf::from("cheats"),
            mlc_dirs: wiiu_title::update::default_mlc_dirs(),
            update_dirs: wiiu_title::update::default_library_dirs(AddOnKind::Update),
            dlc_dirs: wiiu_title::update::default_library_dirs(AddOnKind::Dlc),
            skip_update_check: true,
        }
    }
}
