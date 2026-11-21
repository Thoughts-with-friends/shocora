//! Which packs are enabled with which presets (Cemu's `graphic_pack_entries`), and the
//! packs that apply to a title at its boot.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::{ActiveRules, GraphicPack, ReplacedVolume, rules::load_all};

/// Saved choice for one pack.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PackSetting {
    pub enabled: bool,
    /// Selected preset by category (`""` for packs without categories).
    pub presets: BTreeMap<String, String>,
}

/// Choices by pack, keyed by [`pack_key`].
pub type PackSettings = BTreeMap<String, PackSetting>;

/// Key of a pack in the settings: its `rules.txt` relative to the graphic pack folder,
/// with `/` separators.
pub fn pack_key(root: &Path, pack: &GraphicPack) -> String {
    let rel = pack.rules_path.strip_prefix(root).unwrap_or(&pack.rules_path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Loads every pack under `root` and applies the saved choices (Cemu's `LoadGraphicPack`).
pub fn load_packs(root: &Path, settings: &PackSettings) -> Vec<GraphicPack> {
    let mut packs = load_all(root);
    for pack in &mut packs {
        apply_setting(pack, settings.get(&pack_key(root, pack)));
    }
    packs
}

/// Puts the saved choice (or the pack's defaults without one) into `pack`.
pub fn apply_setting(pack: &mut GraphicPack, setting: Option<&PackSetting>) {
    if let Some(setting) = setting {
        pack.enabled = setting.enabled;
        for (category, name) in &setting.presets {
            pack.set_active_preset(category, name, false);
        }
    }
    pack.update_preset_visibility();
    pack.validate_preset_selections();
}

/// The choice to save for `pack`; `None` when it is off and off by default.
pub fn pack_setting(pack: &GraphicPack) -> Option<PackSetting> {
    let presets: BTreeMap<String, String> = pack
        .categories()
        .into_iter()
        .filter_map(|c| pack.active_preset(&c).map(|name| (c.clone(), name.to_owned())))
        .collect();
    (pack.enabled || pack.default_enabled).then_some(PackSetting { enabled: pack.enabled, presets })
}

/// An enabled pack and what it does.
#[derive(Debug)]
pub struct AppliedPack {
    pub name: String,
    pub dir: PathBuf,
    pub fs_priority: i32,
    pub rules: ActiveRules,
}

impl AppliedPack {
    /// The pack's folder replacing files of `volume`: `content/`, `aoc/` or `code/`.
    pub fn replacement_dir(&self, volume: ReplacedVolume) -> PathBuf {
        self.dir.join(match volume {
            ReplacedVolume::Content => "content",
            ReplacedVolume::Aoc => "aoc",
            ReplacedVolume::Code => "code",
        })
    }
}

/// The enabled packs under `root` made for `title_id`, highest file priority first
/// (Cemu's `GraphicPack2::ActivateForCurrentTitle`). Packs whose rules fail are logged
/// and left out.
pub fn active_for_title(root: &Path, settings: &PackSettings, title_id: u64) -> Vec<AppliedPack> {
    let mut active: Vec<AppliedPack> = load_packs(root, settings)
        .into_iter()
        .filter(|pack| pack.enabled && pack.contains_title_id(title_id))
        .filter_map(|pack| match pack.activate() {
            Ok(rules) => Some(AppliedPack {
                name: pack.display_name().to_owned(),
                dir: pack.dir().to_owned(),
                fs_priority: pack.fs_priority,
                rules,
            }),
            Err(error) => {
                log::warn!("graphic pack {} not applied: {error}", pack.display_name());
                None
            }
        })
        .collect();
    // Stable: packs of the same priority keep the folder order.
    active.sort_by_key(|pack| std::cmp::Reverse(pack.fs_priority));
    active
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Intent: only enabled packs for the booted title apply, with the saved preset, in
    /// file-priority order.
    /// Pass: the other title's pack and the disabled one are left out; the higher
    /// priority comes first; the saved preset's width is in the rule.
    #[test]
    fn picks_enabled_packs_for_the_title() {
        let root = std::env::temp_dir().join(format!("shocora_gpset_{}", std::process::id()));
        let write = |dir: &str, text: &str| {
            let dir = root.join(dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("rules.txt"), text).unwrap();
        };
        let res = "[Definition]\ntitleIds = 0005000010116100\nname = Res\npath = T/Res\n\
                   version = 6\n[Default]\n$w = 1280\n[Preset]\nname = 720p\n[Preset]\nname = 1440p\n$w = 2560\n\
                   [TextureRedefine]\nwidth = 1280\noverwriteWidth = ($w)\n";
        write("A/Res", res);
        write(
            "A/Mod",
            "[Definition]\ntitleIds = 0005000010116100\nname = Mod\npath = T/Mod\nversion = 6\n\
             fsPriority = 200\n",
        );
        write("A/Off", "[Definition]\ntitleIds = 0005000010116100\npath = T/Off\nversion = 6\n");
        write("B/Other", "[Definition]\ntitleIds = 000500001010EC00\npath = T/O\nversion = 6\n");
        let on = |preset: Option<&str>| PackSetting {
            enabled: true,
            presets: preset.map(|p| (String::new(), p.to_owned())).into_iter().collect(),
        };
        let settings: PackSettings = [
            ("A/Res/rules.txt".to_owned(), on(Some("1440p"))),
            ("A/Mod/rules.txt".to_owned(), on(None)),
            ("B/Other/rules.txt".to_owned(), on(None)),
        ]
        .into_iter()
        .collect();
        let active = active_for_title(&root, &settings, 0x0005_0000_1011_6100);
        let names: Vec<&str> = active.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Mod", "Res"]);
        assert_eq!(active[1].rules.texture_rules[0].overwrite_width, 2560);
        std::fs::remove_dir_all(&root).ok();
    }
}
