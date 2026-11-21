//! Cemu graphic packs (`rules.txt` version 3 and later): parsing, presets, the rules an
//! enabled pack applies, replaced files and code patches.
//!
//! * [`ini`], [`expr`] — the file syntax and its arithmetic.
//! * [`pack`] — one pack and its presets.
//! * [`rules`] — what an enabled pack does to the running title.
//! * [`settings`] — the user's choices and the packs that apply to a title.
pub mod expr;
pub mod ini;
pub mod pack;
pub mod patch;
pub mod rules;
pub mod settings;

pub use pack::{GraphicPack, PackError, Preset, VarType};
pub use rules::{
    ActiveRules, ReplacedVolume, TextureDesc, TextureOverwrite, TextureRule, load_all,
    overwrite_for,
};
pub use settings::{
    AppliedPack, PackSetting, PackSettings, active_for_title, apply_setting, load_packs, pack_key,
    pack_setting,
};
