//! The window's texts in the language chosen.
//!
//! English is built into the program: the default, and what a missing text falls back to.
//! Every other language is a file `translation.<code>.json` in a `lang` folder, read when the
//! program runs. Each file is a flat `key: text` table; its `language.name` is how the language
//! calls itself and its `language.code` the Google Translate code it stands for. Adding a
//! language is adding a file with the keys of `translation.eng.json` (a translation program or
//! an LLM can write one). A `translation.eng.json` in the folder overrides the built-in English.
//!
//! Any other language Google Translate knows can be chosen too: the English texts are then
//! machine-translated with the service in the settings, in the background, and kept in
//! `lang-cache/translation.<code>.json` so that it happens once. Until it is done, and for
//! each text that did not survive translation, the window stays English.
//!
//! The `lang` folder next to the program is used, or else the one in the working directory.
use std::{
    collections::{BTreeMap, HashMap},
    fmt::Display,
    path::{Path, PathBuf},
    sync::{
        LazyLock, PoisonError, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

use shocora_gui_common::translate::{self, Service};

/// Where translations are, next to the program or in the working directory.
const FOLDER: &str = "lang";
/// Where machine translations are kept, in the working directory (with the settings).
const CACHE: &str = "lang-cache";
/// The key holding a language's own name.
const NAME_KEY: &str = "language.name";
/// The key holding a language's Google Translate code.
const CODE_KEY: &str = "language.code";
/// The language texts fall back to, and the only one built in.
const FALLBACK: &str = "eng";
/// [`FALLBACK`]'s Google Translate code.
const FALLBACK_CODE: &str = "en";
const BUILT_IN_ENGLISH: &str = include_str!("../../../lang/translation.eng.json");

type Table = HashMap<String, &'static str>;

/// The `lang` folder in use: the one next to the program, or else the working directory's.
fn folder() -> PathBuf {
    let beside_program = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(FOLDER)))
        .filter(|folder| folder.is_dir());
    beside_program.unwrap_or_else(|| PathBuf::from(FOLDER))
}

fn file_of(folder: &Path, code: &str) -> PathBuf {
    folder.join(format!("translation.{code}.json"))
}

/// Parses a translation. The texts live for as long as the program does: `tr` hands them out
/// without copying, once per widget and frame, and languages change too rarely for it to count.
fn parse(json: &str) -> Result<Table, serde_json::Error> {
    let table: HashMap<String, String> = serde_json::from_str(json)?;
    Ok(table.into_iter().map(|(key, text)| (key, &*text.leak())).collect())
}

fn built_in_english() -> Table {
    parse(BUILT_IN_ENGLISH).unwrap_or_else(|error| {
        log::error!("built-in English: {error}");
        Table::new()
    })
}

/// The language `code` from `folder`. English comes from the folder when it is there and
/// makes sense, or else from the program; another language there not is empty, and shows in
/// English.
fn load(code: &str, folder: &Path) -> Table {
    let path = file_of(folder, code);
    let english = code == FALLBACK;
    let json = match std::fs::read_to_string(&path) {
        Ok(json) => json,
        Err(error) => {
            if !english {
                log::warn!("{}: {error}", path.display());
            }
            return if english { built_in_english() } else { Table::new() };
        }
    };
    parse(&json).unwrap_or_else(|error| {
        log::warn!("{}: {error}", path.display());
        if english { built_in_english() } else { Table::new() }
    })
}

/// `(code, name)` of English and the languages in `folder`, by name; a file without a name goes
/// by its code.
fn languages_in(folder: &Path) -> Vec<(String, String)> {
    let english_name = || {
        built_in_english().get(NAME_KEY).map_or_else(|| FALLBACK.to_owned(), |n| (*n).to_owned())
    };
    let Ok(entries) = std::fs::read_dir(folder) else {
        return vec![(FALLBACK.to_owned(), english_name())];
    };
    let mut found: Vec<(String, String)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let file = entry.file_name().into_string().ok()?;
            let code = file.strip_prefix("translation.")?.strip_suffix(".json")?.to_owned();
            if code.is_empty() {
                return None;
            }
            let json = std::fs::read_to_string(entry.path()).ok()?;
            let table: HashMap<String, String> = serde_json::from_str(&json)
                .inspect_err(|error| log::warn!("{file}: {error}"))
                .ok()?;
            let name = table.get(NAME_KEY).cloned().unwrap_or_else(|| code.clone());
            Some((code, name))
        })
        .collect();
    if !found.iter().any(|(code, _)| code == FALLBACK) {
        found.push((FALLBACK.to_owned(), english_name()));
    }
    found.sort_by(|a, b| a.1.cmp(&b.1));
    found
}

/// The hand-written translation in `folder` for the Google Translate code `code`: the file
/// whose `language.code` it is.
fn hand_written(code: &str, folder: &Path) -> Option<Table> {
    if code == FALLBACK_CODE {
        return Some(load(FALLBACK, folder));
    }
    languages_in(folder)
        .into_iter()
        .filter(|(file_code, _)| file_code != FALLBACK)
        .map(|(file_code, _)| load(&file_code, folder))
        .find(|table| table.get(CODE_KEY).is_some_and(|c| *c == code))
}

/// `code` as Google Translate names it: settings from before every language could be chosen
/// name a file (`eng`, `jpn`), whose `language.code` says which language it is.
pub fn normalize(code: &str) -> String {
    if translate::language_name(code).is_some() {
        return code.to_owned();
    }
    let table = load(code, &folder());
    table.get(CODE_KEY).map_or_else(|| FALLBACK_CODE.to_owned(), |c| (*c).to_owned())
}

/// `(code, English name)` of every language on offer: those Google Translate translates into.
pub fn languages() -> &'static [(&'static str, &'static str)] {
    &translate::LANGUAGES
}

/// Where the window's language stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Ready,
    /// Being machine-translated.
    Translating,
    /// Machine translation failed; the window is (partly) English.
    Failed(String),
}

static FALLBACK_TABLE: LazyLock<RwLock<Table>> =
    LazyLock::new(|| RwLock::new(load(FALLBACK, &folder())));
static CURRENT: LazyLock<RwLock<Table>> = LazyLock::new(|| RwLock::new(Table::new()));
static STATUS: RwLock<Status> = RwLock::new(Status::Ready);
/// Counts language changes: a machine translation finishing after another change is dropped.
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn set_status(status: Status) {
    *STATUS.write().unwrap_or_else(PoisonError::into_inner) = status;
}

/// Where the window's language stands.
pub fn status() -> Status {
    STATUS.read().unwrap_or_else(PoisonError::into_inner).clone()
}

fn cache_file(code: &str) -> PathBuf {
    file_of(Path::new(CACHE), code)
}

/// Shows the window in the language `code` (a Google Translate code; older settings' file
/// codes are understood) from now on, returning the code it was taken as. A language without
/// a file is machine-translated with `service` in the background; `repaint` is called when
/// that is done.
pub fn set_language(code: &str, service: &Service, repaint: impl Fn() + Send + 'static) -> String {
    let code = normalize(code);
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let folder = folder();
    let english = load(FALLBACK, &folder);
    *FALLBACK_TABLE.write().unwrap_or_else(PoisonError::into_inner) = english.clone();
    if let Some(table) = hand_written(&code, &folder) {
        *CURRENT.write().unwrap_or_else(PoisonError::into_inner) = table;
        set_status(Status::Ready);
        return code;
    }
    // What was translated before, and what is left: texts added since, or nothing yet.
    let mut table = std::fs::read_to_string(cache_file(&code))
        .ok()
        .and_then(|json| parse(&json).ok())
        .unwrap_or_default();
    table.insert(CODE_KEY.to_owned(), code.clone().leak());
    table.insert(NAME_KEY.to_owned(), translate::language_name(&code).unwrap_or(FALLBACK_CODE));
    let missing: Vec<(String, &'static str)> = english
        .iter()
        .filter(|(key, _)| !table.contains_key(*key))
        .map(|(key, text)| (key.clone(), *text))
        .collect();
    *CURRENT.write().unwrap_or_else(PoisonError::into_inner) = table.clone();
    if missing.is_empty() {
        set_status(Status::Ready);
        return code;
    }
    set_status(Status::Translating);
    let service = service.clone();
    let target = code.clone();
    std::thread::spawn(move || {
        let texts: Vec<&str> = missing.iter().map(|(_, text)| *text).collect();
        let result = translate::translate_all(&service, &texts, &target);
        if GENERATION.load(Ordering::SeqCst) != generation {
            return;
        }
        match result {
            Ok(translated) => {
                for ((key, english), text) in missing.into_iter().zip(translated) {
                    // A text that did not survive stays English rather than being asked for
                    // at every start; translating again starts over.
                    table.insert(key, text.map_or(english, |text| &*text.leak()));
                }
                let sorted: BTreeMap<&str, &str> =
                    table.iter().map(|(key, text)| (key.as_str(), *text)).collect();
                let written = std::fs::create_dir_all(CACHE).and_then(|()| {
                    let json = serde_json::to_string_pretty(&sorted).unwrap_or_default();
                    std::fs::write(cache_file(&target), json)
                });
                if let Err(error) = written {
                    log::warn!("{}: {error}", cache_file(&target).display());
                }
                *CURRENT.write().unwrap_or_else(PoisonError::into_inner) = table;
                set_status(Status::Ready);
            }
            Err(error) => {
                log::warn!("translating the window into {target}: {error}");
                set_status(Status::Failed(error));
            }
        }
        repaint();
    });
    code
}

/// [`set_language`] with the machine translation of `code` made anew.
pub fn retranslate(code: &str, service: &Service, repaint: impl Fn() + Send + 'static) -> String {
    let _ = std::fs::remove_file(cache_file(&normalize(code)));
    set_language(code, service, repaint)
}

/// The text `key` stands for.
pub fn tr(key: &'static str) -> &'static str {
    let current = CURRENT.read().unwrap_or_else(PoisonError::into_inner);
    if let Some(text) = current.get(key) {
        return text;
    }
    drop(current);
    FALLBACK_TABLE.read().unwrap_or_else(PoisonError::into_inner).get(key).copied().unwrap_or(key)
}

/// The text `key` stands for, with its `{name}` places filled in.
pub fn trf(key: &'static str, values: &[(&str, &dyn Display)]) -> String {
    values.iter().fold(tr(key).to_owned(), |text, (name, value)| {
        text.replace(&format!("{{{name}}}"), &value.to_string())
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// The repository's `lang` folder.
    fn repository() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(FOLDER)
    }

    /// The `{name}` places of a text.
    fn places(text: &str) -> BTreeSet<&str> {
        text.split('{').skip(1).filter_map(|rest| rest.split_once('}')).map(|(n, _)| n).collect()
    }

    #[test]
    fn every_language_has_the_texts_and_places_of_english() {
        let english = load(FALLBACK, &repository());
        assert!(!english.is_empty());
        let keys = |table: &Table| table.keys().cloned().collect::<BTreeSet<_>>();
        let languages = languages_in(&repository());
        assert!(languages.len() >= 2, "{languages:?}");
        for (code, _) in languages {
            let other = load(&code, &repository());
            assert_eq!(keys(&english), keys(&other), "{code}");
            for (key, text) in &english {
                assert_eq!(places(text), places(other[key]), "{code}: {key}");
            }
        }
    }

    /// Every `tr("key")` and `trf("key"` in the sources, and whether the files agree.
    #[test]
    fn the_sources_and_the_files_use_the_same_keys() {
        let mut used = BTreeSet::new();
        for source in sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src")) {
            for call in ["tr(", "trf("] {
                for piece in source.split(call).skip(1) {
                    // A long call has its key on the next line.
                    let Some(rest) = piece.trim_start().strip_prefix('"') else { continue };
                    let key = rest.split('"').next().unwrap_or_default();
                    used.insert(key.to_owned());
                }
            }
        }
        // The language's own name is read by the language list, not by `tr`.
        used.insert(NAME_KEY.to_owned());
        let english = load(FALLBACK, &repository());
        let known = english.keys().cloned().collect::<BTreeSet<_>>();
        assert!(used.len() > 50, "only {} keys found", used.len());
        assert_eq!(used.difference(&known).collect::<Vec<_>>(), Vec::<&String>::new(), "no text");
        assert_eq!(known.difference(&used).collect::<Vec<_>>(), Vec::<&String>::new(), "unused");
    }

    /// Every source file under `dir` but this one, whose tests ask for keys that do not exist.
    fn sources(dir: &Path) -> Vec<String> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap().map(Result::unwrap) {
            let path = entry.path();
            if path.is_dir() {
                found.extend(sources(&path));
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.file_name().is_some_and(|n| n != "lang.rs")
            {
                found.push(std::fs::read_to_string(&path).unwrap());
            }
        }
        found
    }

    #[test]
    fn places_are_filled_in() {
        let english = load(FALLBACK, &repository());
        *FALLBACK_TABLE.write().unwrap() = english.clone();
        *CURRENT.write().unwrap() = english;
        let text = trf("graphics.memory_ran_out", &[("factor", &8), ("lower", &6)]);
        assert!(text.contains("8x") && text.contains("6x"), "{text}");
        assert_eq!(tr("no.such.key"), "no.such.key");
    }

    #[test]
    fn a_new_file_is_a_new_language_and_nonsense_is_none() {
        let folder = std::env::temp_dir().join(format!("shocora-lang-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            file_of(&folder, "fra"),
            r#"{"language.name": "Français", "menu.file": "Fichier"}"#,
        )
        .unwrap();
        std::fs::write(file_of(&folder, "xxx"), "not json").unwrap();
        std::fs::write(folder.join("notes.json"), "{}").unwrap();
        // English is always there, built in.
        assert_eq!(
            languages_in(&folder),
            vec![
                ("eng".to_owned(), "English".to_owned()),
                ("fra".to_owned(), "Français".to_owned())
            ]
        );
        assert_eq!(load("fra", &folder)["menu.file"], "Fichier");
        assert!(load("xxx", &folder).is_empty());
        assert_eq!(load("eng", &folder)["menu.file"], "File");
        std::fs::write(file_of(&folder, "eng"), r#"{"menu.file": "Files"}"#).unwrap();
        assert_eq!(load("eng", &folder)["menu.file"], "Files");
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
