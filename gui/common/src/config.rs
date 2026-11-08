//! Front-end settings, stored as TOML.
use std::path::{Path, PathBuf};

use shocora_render::{GraphicsApi, UpscaleConfig};
use wiiu_title::update::AddOnKind;

use crate::input::{KeyBindings, PadBindings};

/// Persisted front-end settings.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Graphics API used at the next start.
    pub graphics_api: GraphicsApi,
    /// Name of the GPU used at the next start; a discrete one is chosen without it.
    pub gpu: Option<String>,
    /// Last booted title or RPX.
    pub last_title: Option<PathBuf>,
    /// Root of per-title save data.
    pub save_dir: PathBuf,
    /// Folder holding the system fonts (CafeStd.ttf, ...), when not found automatically.
    pub fonts_dir: Option<PathBuf>,
    pub key_bindings: KeyBindings,
    pub pad_bindings: PadBindings,
    /// Screen post-processing.
    pub upscale: UpscaleConfig,
    /// Vertical blanks per flip, whatever the title asks for. `1` is 60 fps, and
    /// double speed in a title made for 30 whose logic counts frames.
    pub swap_interval: Option<u32>,
    /// Backdrop of the start screen, as RGBA: alpha lets the picture behind it show.
    pub launcher_background: [u8; 4],
    /// Generate a picture between frames: a title made for 30 fps shows at 60.
    pub frame_generation: bool,
    /// Compile shaders in the background: no stalls, but what uses a new one appears late.
    pub background_shaders: bool,
    /// Picture behind the start screen's color.
    pub launcher_image: Option<PathBuf>,
    /// Language of the window: `eng` or `jpn`.
    pub language: String,
    /// Directories scanned for titles.
    pub rom_dirs: Vec<PathBuf>,
    /// The host a guest joins.
    pub netplay_host: String,
    /// This machine's address when hosting, optionally with `/prefix`. Open to this machine
    /// only until told otherwise.
    pub netplay_address: String,
    /// How many times larger than on the console the title's pictures are drawn.
    pub resolution_factor: u32,
    /// Least vertical blanks per flip: `2` caps a title at 30 fps.
    pub min_swap_interval: Option<u32>,
    /// Wii U system storage (`mlc01`) holding installed updates
    /// (`usr/title/0005000e/<id>`). Cemu's is searched after it.
    pub mlc_dir: Option<PathBuf>,
    /// Folders of updates for any titles, searched before `update/` next to the exe.
    /// Each one, or each of its subfolders, holds an update's `code/`, `content/`, `meta/`.
    pub update_dirs: Vec<PathBuf>,
    /// Folders of downloadable content, searched before `dlc/` next to the exe.
    pub dlc_dirs: Vec<PathBuf>,
    /// Let titles that refuse to start without an update start without one.
    pub skip_update_check: bool,
    /// Show the GamePad's screen in a window of its own.
    pub gamepad_window: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            graphics_api: GraphicsApi::default(),
            gpu: None,
            last_title: None,
            save_dir: PathBuf::from("save"),
            fonts_dir: None,
            key_bindings: KeyBindings::default(),
            pad_bindings: PadBindings::default(),
            upscale: UpscaleConfig::default(),
            swap_interval: None,
            launcher_background: [231, 234, 237, 255],
            frame_generation: false,
            background_shaders: true,
            launcher_image: None,
            language: "jpn".to_owned(),
            rom_dirs: Vec::new(),
            netplay_host: "127.0.0.1".to_owned(),
            netplay_address: "127.0.0.1".to_owned(),
            resolution_factor: 1,
            min_swap_interval: None,
            mlc_dir: None,
            update_dirs: Vec::new(),
            dlc_dirs: Vec::new(),
            skip_update_check: true,
            gamepad_window: false,
        }
    }
}

impl AppConfig {
    /// Default location: `shocora.toml` in the working directory.
    #[inline]
    pub fn default_path() -> PathBuf {
        PathBuf::from("shocora.toml")
    }

    /// Loads `path`, falling back to defaults when missing or invalid.
    pub fn load(path: &Path) -> Self {
        Self::read(path).unwrap_or_else(|error| {
            log::warn!("{error}; using defaults");
            Self::default()
        })
    }

    /// Reads `path`; a missing file means the defaults.
    ///
    /// # Errors
    /// Says what is wrong with a file that exists: it holds the user's settings, so it had
    /// better not be overwritten.
    pub fn read(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }

    /// # Errors
    /// Fails if the file cannot be written.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    pub fn emulator_config(&self) -> wiiu_emu::Config {
        wiiu_emu::Config {
            save_dir: self.save_dir.clone(),
            fonts_dir: self.fonts_dir.clone(),
            swap_interval: self.swap_interval,
            resolution_factor: self.resolution_factor,
            min_swap_interval: self.min_swap_interval,
            mlc_dirs: self
                .mlc_dir
                .iter()
                .cloned()
                .chain(wiiu_title::update::default_mlc_dirs())
                .collect(),
            update_dirs: library_dirs(&self.update_dirs, AddOnKind::Update),
            dlc_dirs: library_dirs(&self.dlc_dirs, AddOnKind::Dlc),
            skip_update_check: self.skip_update_check,
            ..wiiu_emu::Config::default()
        }
    }
}

/// `dirs`, then the folder of this kind next to the exe.
fn library_dirs(dirs: &[PathBuf], kind: AddOnKind) -> Vec<PathBuf> {
    dirs.iter().cloned().chain(wiiu_title::update::default_library_dirs(kind)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let config = AppConfig {
            graphics_api: GraphicsApi::OpenGl,
            rom_dirs: vec![PathBuf::from("D:\\Roms")],
            netplay_host: "10.0.0.2".to_owned(),
            netplay_address: "10.0.0.1/8".to_owned(),
            ..AppConfig::default()
        };
        let text = toml::to_string_pretty(&config).unwrap();
        assert!(text.contains("graphics_api = \"opengl\""));
        assert_eq!(toml::from_str::<AppConfig>(&text).unwrap(), config);
        // Missing fields fall back to defaults.
        assert_eq!(
            toml::from_str::<AppConfig>("graphics_api = \"vulkan\"").unwrap().save_dir,
            config.save_dir
        );
    }
}
