use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{TitleError, meta::TitleMeta};

/// Where the title's files live on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleSource {
    /// An installed/extracted title: `<root>/code`, `<root>/content`, `<root>/meta`.
    Directory(PathBuf),
    /// A standalone homebrew executable.
    Rpx(PathBuf),
}

/// Everything the system needs to boot a title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleInfo {
    pub source: TitleSource,
    pub meta: TitleMeta,
    /// Host path of the main executable.
    pub rpx_path: PathBuf,
    /// Host directories for `/vol/code`, `/vol/content` and `/vol/meta`.
    pub code_dir: PathBuf,
    pub content_dir: Option<PathBuf>,
    pub meta_dir: Option<PathBuf>,
    /// Installed update whose `code/`, `content/` and `meta/` lie over the title's.
    pub update_dir: Option<PathBuf>,
    /// Installed downloadable content, served at `/vol/aoc<id>`.
    pub dlc: Option<Dlc>,
}

/// A title's downloadable content on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dlc {
    /// Holds `code/`, `content/` and `meta/`.
    pub dir: PathBuf,
    /// `0005000c` and the low half of the title's ID.
    pub title_id: u64,
    pub version: u16,
}

impl TitleInfo {
    /// Applies the update installed at `dir`: its XML files and its RPX replace the
    /// title's (`/vol/*` then serve its files over the title's). The title keeps its own
    /// ID (`00050000…`, not the update's `0005000e…`): saves and cheats go by it.
    ///
    /// # Errors
    /// Fails if the update's XML is malformed.
    pub fn with_update(mut self, dir: &Path) -> Result<Self, TitleError> {
        let base_id = self.meta.title_id;
        let code_dir = dir.join("code");

        if let Some((text, path)) = read_if_file(code_dir.join("app.xml"))? {
            self.meta.apply_app_xml(&text, &path)?;
        }
        if let Some((text, path)) = read_if_file(code_dir.join("cos.xml"))? {
            self.meta.apply_cos_xml(&text, &path)?;
        }
        if let Some((text, path)) = read_if_file(dir.join("meta/meta.xml"))? {
            self.meta.apply_meta_xml(&text, &path)?;
        }

        let rpx_name = self
            .meta
            .rpx_name
            .clone()
            .or_else(|| self.rpx_path.file_name().map(|n| n.to_string_lossy().into_owned()));

        if let Some(rpx) = rpx_name.map(|name| code_dir.join(name)).filter(|p| p.is_file()) {
            self.rpx_path = rpx;
        }

        self.meta.title_id = base_id.or(self.meta.title_id);
        self.update_dir = Some(dir.to_owned());

        Ok(self)
    }

    /// Name to show in the UI.
    pub fn display_name(&self) -> String {
        self.meta.long_name_ja.clone().or_else(|| self.meta.long_name.clone()).unwrap_or_else(
            || {
                self.rpx_path
                    .file_stem()
                    .map_or_else(String::new, |s| s.to_string_lossy().into_owned())
            },
        )
    }
}

impl TitleSource {
    /// Recognises `path` and gathers title information.
    ///
    /// Accepts a title root, its `code/` directory, or an `.rpx` file.
    ///
    /// # Errors
    /// Fails if the path is neither, or the title's XML is malformed.
    pub fn open(path: &Path) -> Result<TitleInfo, TitleError> {
        if path.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("rpx")) {
            let code_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();

            // An .rpx inside an installed title boots that title.
            if code_dir.file_name().is_some_and(|n| n.eq_ignore_ascii_case("code")) {
                if let Some(root) = code_dir.parent() {
                    let mut info = Self::open_directory(root)?;
                    info.rpx_path = path.to_owned();
                    return Ok(info);
                }
            }

            return Ok(TitleInfo {
                source: Self::Rpx(path.to_owned()),
                meta: TitleMeta::default(),
                rpx_path: path.to_owned(),
                code_dir,
                content_dir: None,
                meta_dir: None,
                update_dir: None,
                dlc: None,
            });
        }

        if path.is_dir() {
            let root = if path.file_name().is_some_and(|n| n.eq_ignore_ascii_case("code")) {
                path.parent().unwrap_or(path)
            } else {
                path
            };
            return Self::open_directory(root);
        }
        Err(TitleError::Unrecognised(path.to_owned()))
    }

    fn open_directory(root: &Path) -> Result<TitleInfo, TitleError> {
        let code_dir = root.join("code");
        if !code_dir.is_dir() {
            return Err(TitleError::Unrecognised(root.to_owned()));
        }

        let existing = |name: &str| Some(root.join(name)).filter(|p| p.is_dir());
        let (content_dir, meta_dir) = (existing("content"), existing("meta"));

        let mut meta = TitleMeta::default();
        let read = read_if_file;

        if let Some((text, path)) = read(code_dir.join("app.xml"))? {
            meta.apply_app_xml(&text, &path)?;
        }
        if let Some((text, path)) = read(code_dir.join("cos.xml"))? {
            meta.apply_cos_xml(&text, &path)?;
        }
        if let Some(dir) = &meta_dir {
            if let Some((text, path)) = read(dir.join("meta.xml"))? {
                meta.apply_meta_xml(&text, &path)?;
            }
        }

        let rpx_path = match &meta.rpx_name {
            Some(name) if code_dir.join(name).is_file() => code_dir.join(name),
            _ => first_rpx(&code_dir)?.ok_or_else(|| TitleError::NoRpx(root.to_owned()))?,
        };

        Ok(TitleInfo {
            source: Self::Directory(root.to_owned()),
            meta,
            rpx_path,
            code_dir,
            content_dir,
            meta_dir,
            update_dir: None,
            dlc: None,
        })
    }
}

/// Contents of `path` if it is a file.
fn read_if_file(path: PathBuf) -> Result<Option<(String, PathBuf)>, TitleError> {
    if path.is_file() {
        Ok(Some((fs::read_to_string(&path).map_err(TitleError::io(&path))?, path)))
    } else {
        Ok(None)
    }
}

fn first_rpx(dir: &Path) -> Result<Option<PathBuf>, TitleError> {
    let mut candidates: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(TitleError::io(dir))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("rpx")))
        .collect();

    candidates.sort();
    Ok(candidates.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shocora-title-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn opens_installed_title() {
        let root = temp_dir("installed");

        fs::create_dir_all(root.join("code")).unwrap();
        fs::create_dir_all(root.join("content")).unwrap();
        fs::write(root.join("code/other.rpx"), b"").unwrap();
        fs::write(root.join("code/game.rpx"), b"").unwrap();
        fs::write(root.join("code/cos.xml"), "<app><argstr>game.rpx</argstr></app>").unwrap();

        let info = TitleSource::open(&root).unwrap();
        assert_eq!(info.rpx_path, root.join("code/game.rpx"));
        assert_eq!(info.content_dir, Some(root.join("content")));
        assert_eq!(TitleSource::open(&root.join("code")).unwrap().rpx_path, info.rpx_path);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn update_replaces_rpx_and_version() {
        let root = temp_dir("base");

        fs::create_dir_all(root.join("code")).unwrap();
        fs::write(root.join("code/game.rpx"), b"").unwrap();
        fs::write(
            root.join("code/app.xml"),
            "<app><title_id type=\"hexBinary\">00050000101C9300</title_id></app>",
        )
        .unwrap();

        let update = temp_dir("update");
        fs::create_dir_all(update.join("code")).unwrap();
        fs::write(update.join("code/game.rpx"), b"").unwrap();
        fs::write(
            update.join("code/app.xml"),
            "<app><title_id type=\"hexBinary\">0005000E101C9300</title_id>             <title_version type=\"hexBinary\">00D0</title_version></app>",
        )
        .unwrap();

        let info = TitleSource::open(&root).unwrap().with_update(&update).unwrap();
        assert_eq!(info.rpx_path, update.join("code/game.rpx"));
        assert_eq!(info.meta.title_version, Some(208));
        assert_eq!(info.meta.title_id, Some(0x0005_0000_101C_9300), "the update renamed the title");
        assert_eq!(info.update_dir.as_deref(), Some(update.as_path()));

        let _ = (fs::remove_dir_all(root), fs::remove_dir_all(update));
    }

    #[test]
    fn opens_bare_rpx() {
        let dir = temp_dir("bare");
        let rpx = dir.join("hello.rpx");
        fs::write(&rpx, b"").unwrap();

        let info = TitleSource::open(&rpx).unwrap();
        assert_eq!(info.source, TitleSource::Rpx(rpx));
        assert_eq!(info.display_name(), "hello");
        assert!(TitleSource::open(&dir.join("missing.rpx")).is_err());

        let _ = fs::remove_dir_all(&dir);
    }
}

/// Tests against the developer's own title (`shocora.toml`); they pass without one.
#[cfg(test)]
mod rom_tests {
    use wiiu_test_roms::{report, require};

    use super::TitleSource;

    /// Intent: `TitleSource::open` turns a title folder into everything the loader needs:
    /// the executable to boot and the folders mounted as `/vol/code`, `/vol/content` and
    /// `/vol/meta`.
    /// Pass: the `.rpx` exists, `content/` and `meta/` are found, and the name shown in the
    /// library is not empty.
    #[test]
    fn opens_an_installed_title() {
        let title = require!(wiiu_test_roms::title(), "no title");
        let info = TitleSource::open(&title).unwrap();
        assert!(info.rpx_path.is_file());
        assert!(info.content_dir.as_ref().is_some_and(|d| d.is_dir()));
        assert!(info.meta_dir.as_ref().is_some_and(|d| d.is_dir()));
        assert!(!info.display_name().is_empty());
        report("display name", info.display_name());
        report("rpx", info.rpx_path.display());
        report("code dir", info.code_dir.display());
        report("update dir", format!("{:?}", info.update_dir));
    }
}
