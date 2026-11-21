//! Cemu's community graphic packs downloaded from GitHub and kept up to date (Cemu's
//! `DownloadGraphicPacksWindow`).
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub use wiiu_graphic_pack::{PackSetting, PackSettings, load_packs, pack_key, pack_setting};

/// Latest release of the community packs.
pub const RELEASE_API_URL: &str =
    "https://api.github.com/repos/cemu-project/cemu_graphic_packs/releases/latest";
/// Folder under the graphic pack folder that downloads replace.
pub const DOWNLOADED_DIR: &str = "downloadedGraphicPacks";
/// Files larger than this in the archive are skipped, as Cemu does.
const MAX_FILE_SIZE: u64 = 128 * 1024 * 1024;
/// Generic client name: GitHub's API refuses requests without one.
const USER_AGENT: &str = "shocora";

/// A release of the community packs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Name, which includes the version (`Cemu Graphic Packs: v1234`).
    pub name: String,
    pub download_url: String,
}

/// Version of the installed download, from its `version.txt`.
pub fn installed_version(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join(DOWNLOADED_DIR).join("version.txt")).ok()?;
    text.lines().next().map(str::to_owned)
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(USER_AGENT)
        .timeout_global(Some(std::time::Duration::from_secs(300)))
        .build()
        .into()
}

/// Asks GitHub for the latest release.
///
/// # Errors
/// Fails without a connection or on an unexpected reply.
pub fn latest_release() -> Result<Release, String> {
    let body = agent()
        .get(RELEASE_API_URL)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    parse_release(&body)
}

fn parse_release(body: &str) -> Result<Release, String> {
    let json: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let name = json["name"].as_str().ok_or("release has no name")?.to_owned();
    let download_url = json["assets"][0]["browser_download_url"]
        .as_str()
        .ok_or("release has no download")?
        .to_owned();
    Ok(Release { name, download_url })
}

/// Downloads `url` reporting `0.0..=1.0` progress.
fn download(url: &str, progress: &dyn Fn(f32)) -> Result<Vec<u8>, String> {
    let mut response = agent().get(url).call().map_err(|e| e.to_string())?;
    let total: Option<u64> = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let mut reader = response.body_mut().with_config().limit(1 << 31).reader();
    let mut data = Vec::new();
    let mut buf = vec![0; 1 << 16];
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        if let Some(total) = total.filter(|t| *t > 0) {
            #[allow(clippy::cast_precision_loss)]
            progress(data.len() as f32 / total as f32);
        }
    }
    Ok(data)
}

/// Replaces the downloaded packs under `root` with the archive `zip` and records `version`.
/// Entries escaping the folder (`..`, absolute paths) are skipped.
///
/// # Errors
/// Fails if the archive is invalid or the folder cannot be written.
pub fn install(
    root: &Path,
    zip: &[u8],
    version: &str,
    progress: &dyn Fn(f32),
) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).map_err(|e| e.to_string())?;
    let target = root.join(DOWNLOADED_DIR);
    // Empty the folder rather than removing it: recreating it at once sometimes fails.
    if let Ok(entries) = std::fs::read_dir(&target) {
        for entry in entries.flatten() {
            let path = entry.path();
            let removed = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            if let Err(e) = removed {
                log::warn!("could not remove {}: {e}", path.display());
            }
        }
    }
    std::fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    let count = archive.len();
    for i in 0..count {
        #[allow(clippy::cast_precision_loss)]
        progress(i as f32 / count.max(1) as f32);
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        let Some(name) = file.enclosed_name() else {
            log::warn!("graphic pack archive: skipped unsafe path {:?}", file.name());
            continue;
        };
        let path = target.join(name);
        if file.is_dir() {
            std::fs::create_dir_all(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            continue;
        }
        if file.size() == 0 || file.size() > MAX_FILE_SIZE {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let mut data = Vec::with_capacity(usize::try_from(file.size()).unwrap_or(0));
        file.read_to_end(&mut data).map_err(|e| e.to_string())?;
        std::fs::write(&path, data).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    std::fs::write(target.join("version.txt"), version).map_err(|e| e.to_string())?;
    progress(1.0);
    Ok(())
}

/// Where a check or download stands.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum UpdateState {
    #[default]
    Idle,
    Checking,
    /// The installed packs are the latest release.
    UpToDate(String),
    /// A newer release than the installed one: the user decides.
    Available(Release),
    Downloading(f32),
    Extracting(f32),
    /// Installed: the packs must be loaded again.
    Installed(String),
    Failed(String),
}

impl UpdateState {
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Checking | Self::Downloading(_) | Self::Extracting(_))
    }
}

/// Checks for and installs pack updates on a thread of its own.
#[derive(Debug, Clone, Default)]
pub struct Updater {
    state: Arc<Mutex<UpdateState>>,
}

impl Updater {
    pub fn state(&self) -> UpdateState {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn set(state: &Arc<Mutex<UpdateState>>, value: UpdateState) {
        if let Ok(mut s) = state.lock() {
            *s = value;
        }
    }

    /// Asks for the latest release. Without installed packs it is downloaded at once;
    /// otherwise a newer one is offered ([`UpdateState::Available`]) as Cemu does.
    pub fn check(&self, root: PathBuf) {
        if self.state().is_busy() {
            return;
        }
        Self::set(&self.state, UpdateState::Checking);
        let state = Arc::clone(&self.state);
        std::thread::spawn(move || match latest_release() {
            Err(e) => Self::set(&state, UpdateState::Failed(e)),
            Ok(release) => match installed_version(&root) {
                Some(v) if v.eq_ignore_ascii_case(&release.name) => {
                    Self::set(&state, UpdateState::UpToDate(release.name));
                }
                Some(_) => Self::set(&state, UpdateState::Available(release)),
                None => Self::run_install(&state, &root, &release),
            },
        });
    }

    /// Downloads and installs `release`.
    pub fn install(&self, root: PathBuf, release: Release) {
        if self.state().is_busy() {
            return;
        }
        Self::set(&self.state, UpdateState::Downloading(0.0));
        let state = Arc::clone(&self.state);
        std::thread::spawn(move || Self::run_install(&state, &root, &release));
    }

    pub fn dismiss(&self) {
        if !self.state().is_busy() {
            Self::set(&self.state, UpdateState::Idle);
        }
    }

    fn run_install(state: &Arc<Mutex<UpdateState>>, root: &Path, release: &Release) {
        Self::set(state, UpdateState::Downloading(0.0));
        let data = match download(&release.download_url, &|p| {
            Self::set(state, UpdateState::Downloading(p));
        }) {
            Ok(data) => data,
            Err(e) => return Self::set(state, UpdateState::Failed(e)),
        };
        Self::set(state, UpdateState::Extracting(0.0));
        let result =
            install(root, &data, &release.name, &|p| Self::set(state, UpdateState::Extracting(p)));
        Self::set(
            state,
            match result {
                Ok(()) => UpdateState::Installed(release.name.clone()),
                Err(e) => UpdateState::Failed(e),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, data) in entries {
                w.start_file(*name, options).unwrap();
                w.write_all(data).unwrap();
            }
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    /// Intent: installing replaces the old download, writes version.txt, and refuses
    /// archive entries that would land outside the folder (zip slip).
    /// Pass: the pack is there, the old file and the `../` entry are not.
    #[test]
    fn install_replaces_and_stays_inside() {
        let root = std::env::temp_dir().join(format!("shocora_gpdl_{}", std::process::id()));
        let old = root.join(DOWNLOADED_DIR).join("Old/rules.txt");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, "old").unwrap();
        let zip = zip_of(&[("XCX/Graphics/rules.txt", b"[Definition]"), ("../escape.txt", b"x")]);
        install(&root, &zip, "Cemu Graphic Packs: v1", &|_| {}).unwrap();
        assert!(root.join(DOWNLOADED_DIR).join("XCX/Graphics/rules.txt").is_file());
        assert!(!old.exists());
        assert!(!root.join("escape.txt").exists());
        assert_eq!(installed_version(&root).as_deref(), Some("Cemu Graphic Packs: v1"));
        std::fs::remove_dir_all(&root).ok();
    }

    /// Intent: GitHub's release JSON gives the name and the first asset's URL.
    /// Pass: both are read; a reply without assets is an error.
    #[test]
    fn reads_release_json() {
        let r = parse_release(
            r#"{"name":"v991","assets":[{"browser_download_url":"https://x/y.zip"}]}"#,
        )
        .unwrap();
        assert_eq!(r.name, "v991");
        assert_eq!(r.download_url, "https://x/y.zip");
        assert!(parse_release(r#"{"name":"v1","assets":[]}"#).is_err());
    }

    /// Intent: the real download works end to end (network).
    /// Pass: the latest release installs and loads more than 100 packs.
    #[test]
    #[ignore = "downloads from GitHub"]
    fn downloads_the_latest_release() {
        let root = std::env::temp_dir().join("shocora_gp_download");
        let release = latest_release().unwrap();
        println!("release {}", release.name);
        let data = download(&release.download_url, &|_| {}).unwrap();
        install(&root, &data, &release.name, &|_| {}).unwrap();
        let packs = load_packs(&root, &PackSettings::new());
        println!("packs {}", packs.len());
        assert!(packs.len() > 100);
    }
}
