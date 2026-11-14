//! ROM fixtures for unit tests: the titles and saves named in the developer's git-ignored
//! `shocora.toml` at the workspace root.
//!
//! Nothing read from a ROM is ever stored in the repository. A test that needs a ROM asks
//! for one with [`require!`]; without one (a fresh clone, CI) it prints why and passes.
//! What a test read is printed with [`report`], so
//! `cargo test -- --nocapture` shows what the emulator takes from the ROM.
//!
//! Only `std`, `serde` and `toml`: every core crate can use this as a dev-dependency
//! without a cycle.
use std::path::{Path, PathBuf};

/// The keys of `shocora.toml` the tests use.
#[derive(serde::Deserialize, Debug, Default)]
struct Config {
    last_title: Option<PathBuf>,
    #[serde(default)]
    rom_dirs: Vec<PathBuf>,
    save_dir: Option<PathBuf>,
}

/// The workspace root, where `shocora.toml` lives.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn config() -> Config {
    std::fs::read_to_string(workspace_root().join("shocora.toml"))
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

/// Whether `dir` is an installed title: `code/app.xml` and `meta/meta.xml` are there.
pub fn is_title(dir: &Path) -> bool {
    dir.join("code/app.xml").is_file() && dir.join("meta/meta.xml").is_file()
}

/// Every title of `shocora.toml`: `last_title` first, then those directly inside each of
/// `rom_dirs`.
pub fn titles() -> Vec<PathBuf> {
    let config = config();
    let mut titles: Vec<PathBuf> = config.last_title.into_iter().filter(|t| is_title(t)).collect();
    for dir in &config.rom_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        let mut found: Vec<PathBuf> =
            entries.flatten().map(|e| e.path()).filter(|p| is_title(p)).collect();
        found.sort();
        found.retain(|t| !titles.contains(t));
        titles.extend(found);
    }
    titles
}

/// The first title of [`titles`].
pub fn title() -> Option<PathBuf> {
    titles().into_iter().next()
}

/// The save directory of `shocora.toml` (`save_dir`, relative to the workspace root).
pub fn save_root() -> Option<PathBuf> {
    let dir = config().save_dir?;
    let dir = if dir.is_absolute() { dir } else { workspace_root().join(dir) };
    dir.is_dir().then_some(dir)
}

/// The 64-bit title id of `meta/meta.xml`, read without an XML parser.
pub fn title_id(title: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(title.join("meta/meta.xml")).ok()?;
    let start = text.find("<title_id")?;
    let value = &text[start..];
    let value = &value[value.find('>')? + 1..];
    u64::from_str_radix(value[..value.find('<')?].trim(), 16).ok()
}

/// The save data of `title`: `<save_dir>/<title id>`.
pub fn save_of(title: &Path) -> Option<PathBuf> {
    let dir = save_root()?.join(format!("{:016x}", title_id(title)?));
    dir.is_dir().then_some(dir)
}

/// The main executable of `title`: the `.rpx` in `code/`.
pub fn rpx_of(title: &Path) -> Option<PathBuf> {
    std::fs::read_dir(title.join("code"))
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("rpx")))
}

/// Copies the folder `from` to `to`, recursively.
///
/// # Panics
/// If a file cannot be copied: a test fixture is broken.
#[allow(clippy::expect_used, reason = "a broken fixture fails the test")]
pub fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create the copy");
    for entry in std::fs::read_dir(from).expect("read the folder").flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy a file");
        }
    }
}

/// A save folder for an emulator in a test: a fresh temporary folder named after `tag`,
/// holding a copy of `title`'s save when there is one. The real save is never written.
pub fn temp_save_dir(title: &Path, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("shocora-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    if let (Some(save), Some(id)) = (save_of(title), title_id(title)) {
        copy_dir(&save, &dir.join(format!("{id:016x}")));
    }
    dir
}

/// Says why a ROM test did not run.
pub fn skip(test: &str, missing: &str) {
    eprintln!("skipped {test}: {missing} (set last_title / save_dir in shocora.toml)");
}

/// Prints one thing a test read from the ROM.
pub fn report(label: &str, value: impl std::fmt::Display) {
    println!("[rom] {label}: {value}");
}

/// `require!(expr, "what")`: the value of an `Option`, or skip the test (it passes) when it
/// is `None`.
#[macro_export]
macro_rules! require {
    ($value:expr, $missing:literal) => {
        match $value {
            Some(value) => value,
            None => {
                $crate::skip(module_path!(), $missing);
                return;
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Intent: the helper reads `shocora.toml` and finds the configured title and save.
    /// Pass: without a configuration the lists are empty (no panic); with one, every title
    /// has `code/app.xml` and a title id, and its main executable is found.
    #[test]
    fn finds_the_configured_titles() {
        for title in titles() {
            assert!(is_title(&title));

            let id = title_id(&title).expect("meta.xml has a title id");
            report("title id", format!("{id:016x}"));
            report("rpx", rpx_of(&title).expect("code/ has an rpx").display());

            if let Some(save) = save_of(&title) {
                report("save", save.display());
            }
        }
    }
}
