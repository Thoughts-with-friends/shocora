//! A title's update and its downloadable content, and titles that refuse to start without
//! an update.
//!
//! Both are looked for, in this order (`update` stands for `dlc` with downloadable content):
//!
//! ```text
//!  <title>/update/                    next to the title's own code/, content/ and meta/
//!  <exe>/update/  and folders chosen  folders of updates for any number of titles, searched
//!                 in the settings     through their subfolders (an `mlc01` copy works too)
//!  <mlc>/usr/title/0005000e/<id>/     the console's storage (`mlc01`) as Cemu lays it out
//!                 0005000c/<id>/      (downloadable content)
//! ```
//!
//! Each holds `code/`, `content/` and `meta/`. A folder only counts when its `code/app.xml`
//! names the update (or content) of this very title: dumps of the wrong title, or ones still
//! encrypted, are passed over with the reason. In a folder of many titles' updates, the
//! other titles' are passed over silently.
use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::meta::TitleMeta;

/// Folder inside a title's folder that holds its update.
pub const BUNDLED_DIR: &str = "update";

/// What lies over a title's files, or next to them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOnKind {
    Update,
    /// Downloadable content (`AOC`).
    Dlc,
}

impl AddOnKind {
    /// High half of the title ID of every add-on of this kind.
    const fn title_type(self) -> u64 {
        match self {
            Self::Update => 0x0005_000E,
            Self::Dlc => 0x0005_000C,
        }
    }

    /// Title ID of this add-on of `title_id`.
    #[inline]
    pub const fn title_id(self, title_id: u64) -> u64 {
        (self.title_type() << 32) | (title_id & 0xFFFF_FFFF)
    }

    /// Name of the folder that holds add-ons of this kind, in a title's folder or by the exe.
    #[inline]
    pub const fn dir_name(self) -> &'static str {
        match self {
            Self::Update => BUNDLED_DIR,
            Self::Dlc => "dlc",
        }
    }
}

/// Title ID of the update of `title_id`.
#[inline]
pub const fn update_title_id(title_id: u64) -> u64 {
    AddOnKind::Update.title_id(title_id)
}

/// `<exe>/update` or `<exe>/dlc`: where add-ons dropped next to the emulator are found.
pub fn default_library_dirs(kind: AddOnKind) -> Vec<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(kind.dir_name())))
        .into_iter()
        .collect()
}

/// Where Cemu keeps its system storage on this machine, if the platform has a known place.
pub fn default_mlc_dirs() -> Vec<PathBuf> {
    let env = |name| std::env::var_os(name).map(PathBuf::from);
    let dir = if cfg!(windows) {
        env("APPDATA").map(|d| d.join("Cemu"))
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|d| d.join("Library/Application Support/Cemu"))
    } else {
        env("XDG_DATA_HOME")
            .or_else(|| env("HOME").map(|d| d.join(".local/share")))
            .map(|d| d.join("Cemu"))
    };
    dir.map(|d| d.join("mlc01")).into_iter().collect()
}

/// Why a folder that looks like an update is not used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UpdateProblem {
    #[error("code/app.xml cannot be read as XML (an encrypted dump?)")]
    Unreadable,
    #[error("code/app.xml names no title")]
    NoTitleId,
    #[error("it holds title {found:016X}, not {wanted:016X}")]
    OtherTitle { found: u64, wanted: u64 },
}

/// Where the add-on was found, and the folders passed over.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateSearch {
    pub found: Option<PathBuf>,
    /// `title_version` of what was found, from its `code/app.xml`.
    pub version: Option<u32>,
    pub rejected: Vec<(PathBuf, UpdateProblem)>,
}

/// Looks for the update of `title_id`: in `<root>/update` first (if the title has a folder),
/// then in each of `mlc_dirs`.
#[inline]
pub fn find_update(title_id: u64, root: Option<&Path>, mlc_dirs: &[PathBuf]) -> UpdateSearch {
    find_add_on(AddOnKind::Update, title_id, root, &[], mlc_dirs)
}

/// Looks for the add-on of `title_id`: in `<root>/<kind>` first (if the title has a folder),
/// then anywhere below each of `library_dirs`, then in each of `mlc_dirs`.
pub fn find_add_on(
    kind: AddOnKind,
    title_id: u64,
    root: Option<&Path>,
    library_dirs: &[PathBuf],
    mlc_dirs: &[PathBuf],
) -> UpdateSearch {
    let id = kind.title_id(title_id);
    let (high, low) = (id >> 32, id & 0xFFFF_FFFF);

    // Cemu writes lowercase; a copy made by hand may not be.
    let names = [
        (format!("{high:08x}"), format!("{low:08x}")),
        (format!("{high:08X}"), format!("{low:08X}")),
    ];

    // `true`: a folder shared by many titles.
    let mut candidates: Vec<(PathBuf, bool)> =
        root.map(|root| (root.join(kind.dir_name()), false)).into_iter().collect();

    for dir in library_dirs {
        candidates.extend(library_entries(dir, &names[0].1).into_iter().map(|dir| (dir, true)));
    }
    for mlc in mlc_dirs {
        candidates
            .extend(names.iter().map(|(h, l)| (mlc.join("usr/title").join(h).join(l), false)));
    }

    let mut search = UpdateSearch::default();
    let mut seen: Vec<PathBuf> = Vec::new();

    for (dir, shared) in candidates {
        if !dir.join("code").is_dir() || seen.contains(&dir) {
            continue;
        }
        seen.push(dir.clone());

        match verify_add_on(&dir, kind, title_id) {
            Ok(meta) => {
                search.version = meta.title_version;
                search.found = Some(dir);
                break;
            }
            // Other titles' add-ons are expected in a folder shared by many titles.
            Err(UpdateProblem::OtherTitle { .. }) if shared => {}
            Err(problem) => search.rejected.push((dir, problem)),
        }
    }
    search
}

/// How deep below a library folder add-ons are looked for: enough for an `mlc01` copied
/// whole (`mlc01/usr/title/0005000e/<id>`) inside a folder or two of the user's own.
const LIBRARY_DEPTH: usize = 8;

/// Every folder at or below `dir` that holds a `code/` folder, those whose path names the
/// title (`id`, its low half in hex) first. Below such a folder nothing is looked for:
/// it is a title, an update or downloadable content, and its `content/` can be huge.
fn library_entries(dir: &Path, id: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![(dir.to_owned(), 0)];

    while let Some((dir, depth)) = pending.pop() {
        if dir.join("code").is_dir() {
            found.push(dir);
            continue;
        }
        if depth >= LIBRARY_DEPTH {
            continue;
        }

        let children = fs::read_dir(&dir).into_iter().flatten().filter_map(Result::ok);
        pending.extend(
            children
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                .map(|entry| (entry.path(), depth + 1)),
        );
    }

    let names_title = |path: &Path| path.to_string_lossy().to_ascii_lowercase().contains(id);
    found.sort_by_cached_key(|path| (!names_title(path), path.clone()));
    found
}

/// Whether `dir` holds the update of `title_id`, as its `code/app.xml` says.
///
/// # Errors
/// Says what is wrong with the folder.
#[inline]
pub fn verify(dir: &Path, title_id: u64) -> Result<(), UpdateProblem> {
    verify_add_on(dir, AddOnKind::Update, title_id).map(drop)
}

/// Whether `dir` holds the add-on of `title_id`, as its `code/app.xml` says; what that says.
///
/// # Errors
/// Says what is wrong with the folder.
pub fn verify_add_on(
    dir: &Path,
    kind: AddOnKind,
    title_id: u64,
) -> Result<TitleMeta, UpdateProblem> {
    let path = dir.join("code/app.xml");
    let text = fs::read(&path).map_err(|_| UpdateProblem::Unreadable)?;

    let mut meta = TitleMeta::default();
    meta.apply_app_xml(&String::from_utf8_lossy(&text), &path)
        .map_err(|_| UpdateProblem::Unreadable)?;

    let wanted = kind.title_id(title_id);
    match meta.title_id {
        Some(found) if found == wanted => Ok(meta),
        Some(found) => Err(UpdateProblem::OtherTitle { found, wanted }),
        None => Err(UpdateProblem::NoTitleId),
    }
}

/// A file a title's base version checks before starting, and what it holds once updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionGate {
    /// Path inside `/vol/content`.
    pub file: &'static str,
    /// Contents that let the title start.
    pub contents: &'static [u8],
}

/// *Breath of the Wild* (JPN, USA, EUR): the disc's `System/Version.txt` says `0.9.0`,
/// and below 1.0 the title only says that it must be updated. The last update (v208)
/// says `1.5.0`.
const BREATH_OF_THE_WILD: (&[u64], VersionGate) = (
    &[0x0005_0000_101C_9300, 0x0005_0000_101C_9400, 0x0005_0000_101C_9500],
    VersionGate { file: "/System/Version.txt", contents: b"1.5.0" },
);

const GATES: &[(&[u64], VersionGate)] = &[BREATH_OF_THE_WILD];

/// The gate of `title_id`, for titles known to refuse to start without an update.
#[inline]
pub fn version_gate(title_id: u64) -> Option<VersionGate> {
    GATES.iter().find(|(ids, _)| ids.contains(&title_id)).map(|&(_, gate)| gate)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write_app_xml(dir: &Path, contents: &[u8]) {
        fs::create_dir_all(dir.join("code")).unwrap();
        fs::write(dir.join("code/app.xml"), contents).unwrap();
    }

    fn app_xml(title_id: u64) -> Vec<u8> {
        format!("<app><title_id type=\"hexBinary\">{title_id:016X}</title_id></app>").into_bytes()
    }

    const BASE: u64 = 0x0005_0000_101C_9300;

    #[test]
    fn finds_updates_in_mlc_layout() {
        let temp = std::env::temp_dir().join(format!("shocora-mlc-{}", std::process::id()));
        let mlc = temp.join("mlc01");
        let _ = fs::remove_dir_all(&temp);

        let update = mlc.join("usr/title/0005000e/101c9300");
        write_app_xml(&update, &app_xml(update_title_id(BASE)));
        assert_eq!(update_title_id(BASE), 0x0005_000E_101C_9300);

        let search = find_update(BASE, None, &[PathBuf::from("missing"), mlc.clone()]);
        assert_eq!(search.found, Some(update));
        assert_eq!(find_update(0x0005_0000_1010_EC00, None, &[mlc]).found, None);

        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn the_title_folder_comes_first_and_wrong_dumps_are_passed_over() {
        let temp = std::env::temp_dir().join(format!("shocora-bundled-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);

        let (root, mlc) = (temp.join("title"), temp.join("mlc01"));
        let installed = mlc.join("usr/title/0005000e/101c9300");
        write_app_xml(&installed, &app_xml(update_title_id(BASE)));

        // A system title dumped next to the game, then one still encrypted.
        write_app_xml(&root.join(BUNDLED_DIR), &app_xml(0x0005_0010_1006_0000));

        let search = find_update(BASE, Some(&root), std::slice::from_ref(&mlc));
        assert_eq!(search.found, Some(installed));
        assert!(matches!(search.rejected[..], [(_, UpdateProblem::OtherTitle { .. })]));
        write_app_xml(&root.join(BUNDLED_DIR), &[0x8F, 0x00, 0xC3, 0x12]);

        let search = find_update(BASE, Some(&root), &[]);
        assert_eq!(search.rejected[0].1, UpdateProblem::Unreadable);

        write_app_xml(&root.join(BUNDLED_DIR), &app_xml(update_title_id(BASE)));

        let search = find_update(BASE, Some(&root), &[mlc]);
        assert_eq!(search.found, Some(root.join(BUNDLED_DIR)));

        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn finds_add_ons_dropped_in_a_shared_folder() {
        let temp = std::env::temp_dir().join(format!("shocora-library-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        let (updates, dlc) = (temp.join("update"), temp.join("dlc"));

        // Another title's update first by name: passed over without a word.
        write_app_xml(&updates.join("1010ec00"), &app_xml(update_title_id(0x0005_0000_1010_EC00)));
        write_app_xml(&updates.join("any name"), &app_xml(update_title_id(BASE)));

        // Deep in a copied `mlc01`: found, and before the other one, as its path names the title.
        let nested = updates.join("backup/mlc01/usr/title/0005000e/101c9300");
        write_app_xml(&nested, &app_xml(update_title_id(BASE)));

        let dlc_id = AddOnKind::Dlc.title_id(BASE);
        write_app_xml(&dlc, &format!(
            "<app><title_id type=\"hexBinary\">{dlc_id:016X}</title_id>             <title_version type=\"hexBinary\">0050</title_version></app>"
        ).into_bytes());

        let found = find_add_on(AddOnKind::Update, BASE, None, std::slice::from_ref(&updates), &[]);
        assert_eq!(found.found, Some(nested));
        assert!(found.rejected.is_empty());

        let found = find_add_on(AddOnKind::Dlc, BASE, None, std::slice::from_ref(&dlc), &[]);
        assert_eq!((found.found, found.version), (Some(dlc), Some(0x50)));
        assert_eq!(dlc_id, 0x0005_000C_101C_9300);

        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn knows_gated_titles() {
        assert_eq!(version_gate(0x0005_0000_101C_9300).unwrap().contents, b"1.5.0");
        assert_eq!(version_gate(0x0005_0000_1010_EC00), None);
    }
}

/// Tests against the developer's own title (`shocora.toml`); they pass without one.
#[cfg(test)]
mod rom_tests {
    use wiiu_test_roms::{report, require};

    use super::version_gate;

    /// Intent: for a title that refuses to start below a version, the gate names a file in
    /// its content, and the emulator can tell from it whether an update is applied.
    /// Pass: titles without a gate pass at once; otherwise the gate file exists in the
    /// base game's `content/` and its contents are printed (the base game holds an older
    /// version than the gate's).
    #[test]
    fn version_gate_file_is_in_the_content() {
        let title = require!(wiiu_test_roms::title(), "no title");
        let id = require!(wiiu_test_roms::title_id(&title), "no title id");
        let Some(gate) = version_gate(id) else {
            report("version gate", "none for this title");
            return;
        };
        let file = title.join("content").join(gate.file.trim_start_matches('/'));
        let contents = std::fs::read(&file).unwrap();
        report("version gate file", gate.file);
        report("base game holds", String::from_utf8_lossy(&contents).trim());
        report("gate wants", String::from_utf8_lossy(gate.contents));
    }
}
