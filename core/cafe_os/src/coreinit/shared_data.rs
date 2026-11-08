//! `OSGetSharedData`: the system fonts every title can map.
//!
//! Fonts are not part of shocora. They are looked for in `SHOCORA_FONTS`, the
//! configured folder, a `resources/sharedFonts` folder next to the working
//! directory or executable, and a Cemu installation nearby (`CafeCn.ttf`,
//! `CafeKr.ttf`, `CafeStd.ttf`, `CafeTw.ttf`). Failing that a host font stands
//! in: its script must match, or text turns into empty boxes. Button icons only
//! exist in the real fonts.
use std::path::{Path, PathBuf};

use wiiu_hle_macros::hle_export;
use wiiu_mem_addr::SHARED;

use crate::hle::{HleContext, HleError};

const FONT_FILES: [&str; 4] = ["CafeCn.ttf", "CafeKr.ttf", "CafeStd.ttf", "CafeTw.ttf"];
/// Fonts of a Windows machine per font id, the likeliest to exist last.
/// `CafeStd` is what Japanese titles draw their text with.
const HOST_FALLBACKS: [&[&str]; 4] = [
    &["C:/Windows/Fonts/msyh.ttc", "C:/Windows/Fonts/simsun.ttc", "C:/Windows/Fonts/arial.ttf"],
    &["C:/Windows/Fonts/malgun.ttf", "C:/Windows/Fonts/gulim.ttc", "C:/Windows/Fonts/arial.ttf"],
    &[
        "C:/Windows/Fonts/meiryo.ttc",
        "C:/Windows/Fonts/YuGothM.ttc",
        "C:/Windows/Fonts/msgothic.ttc",
        "C:/Windows/Fonts/arial.ttf",
    ],
    &["C:/Windows/Fonts/msjh.ttc", "C:/Windows/Fonts/mingliu.ttc", "C:/Windows/Fonts/arial.ttf"],
];
/// How far up from the working directory or executable to look for Cemu.
const CEMU_SEARCH_LEVELS: usize = 3;

/// Where the shared area is filled up to, and what is mapped.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct SharedData {
    cursor: u32,
    /// `(address, size)` per font id.
    fonts: [Option<(u32, u32)>; 4],
    /// Folder chosen in the settings.
    pub fonts_dir: Option<PathBuf>,
}

/// `<ancestor of start>/Cemu*/resources/sharedFonts`.
fn cemu_font_dirs(start: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for ancestor in start.ancestors().take(CEMU_SEARCH_LEVELS) {
        let Ok(entries) = std::fs::read_dir(ancestor) else { continue };
        for entry in entries.flatten() {
            let is_cemu = entry.file_name().to_string_lossy().to_lowercase().starts_with("cemu");
            let fonts = entry.path().join("resources/sharedFonts");
            if is_cemu && fonts.is_dir() {
                found.push(fonts);
            }
        }
    }
    found
}

fn font_directories(configured: Option<&Path>) -> Vec<PathBuf> {
    let mut directories: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("SHOCORA_FONTS") {
        directories.push(dir.into());
    }
    directories.extend(configured.map(Path::to_path_buf));
    let current = std::env::current_dir().ok();
    let executable = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.to_owned()));
    for base in current.iter().chain(&executable) {
        directories.push(base.join("resources/sharedFonts"));
    }
    for base in current.iter().chain(&executable) {
        directories.extend(cemu_font_dirs(base));
    }
    directories
}

/// A font collection as a plain font of its first face, which is all titles parse.
fn first_face(file: Vec<u8>) -> Option<Vec<u8>> {
    if !file.starts_with(b"ttcf") {
        return Some(file);
    }
    let word = |at: usize| -> Option<usize> {
        Some(u32::from_be_bytes(file.get(at..at + 4)?.try_into().ok()?) as usize)
    };
    let face = word(12)?;
    let tables = usize::from(u16::from_be_bytes(file.get(face + 4..face + 6)?.try_into().ok()?));
    let mut font = file.get(face..face + 12 + 16 * tables)?.to_vec();
    for table in 0..tables {
        let record = face + 12 + 16 * table;
        let (offset, length) = (word(record + 8)?, word(record + 12)?);
        let data = file.get(offset..offset + length)?;
        let start = u32::try_from(font.len()).ok()?;
        font.extend_from_slice(data);
        font.resize(font.len().next_multiple_of(4), 0);
        font[12 + 16 * table + 8..12 + 16 * table + 12].copy_from_slice(&start.to_be_bytes());
    }
    Some(font)
}

/// The font and, when it is a stand-in, a warning for the user.
fn font_bytes(id: usize, configured: Option<&Path>) -> Option<(Vec<u8>, Option<String>)> {
    let name = FONT_FILES[id];
    for dir in font_directories(configured) {
        if let Ok(bytes) = std::fs::read(dir.join(name)) {
            log::info!("system font {name} from {}", dir.display());
            return Some((bytes, None));
        }
    }
    for path in HOST_FALLBACKS[id] {
        if let Some(bytes) = std::fs::read(path).ok().and_then(first_face) {
            let warning = format!(
                "system font {name} not found, using {path}: button icons will be missing. \
                 Choose a folder with the Cafe fonts in View > System fonts folder \
                 (Cemu has them in resources/sharedFonts)."
            );
            return Some((bytes, Some(warning)));
        }
    }
    log::warn!("system font {name} not found; see SHOCORA_FONTS");
    None
}
#[hle_export(lib = "coreinit", name = "OSGetSharedData")]
fn os_get_shared_data(
    ctx: &mut HleContext<'_>,
    area: u32,
    _flags: u32,
    pointer_out: u32,
    size_out: u32,
) -> Result<bool, HleError> {
    let id = area as usize;
    if id >= FONT_FILES.len() {
        return Ok(false);
    }
    let entry = match ctx.os.coreinit.shared_data.fonts[id] {
        Some(entry) => Some(entry),
        None => {
            let found = font_bytes(id, ctx.os.coreinit.shared_data.fonts_dir.as_deref());
            let loaded = found.and_then(|(bytes, warning)| {
                if let Some(warning) = warning {
                    log::warn!("{warning}");
                    ctx.os.console.push(warning);
                }
                let state = &mut ctx.os.coreinit.shared_data;
                let size = u32::try_from(bytes.len()).ok()?;
                let address = (SHARED.start + state.cursor).next_multiple_of(0x100);
                if u64::from(address) + u64::from(size) > SHARED.end() {
                    log::warn!("shared area exhausted: font {id} does not fit");
                    return None;
                }
                state.cursor = address + size - SHARED.start;
                Some((address, size, bytes))
            });
            match loaded {
                Some((address, size, bytes)) => {
                    ctx.memory.write_bytes(address, &bytes)?;
                    ctx.os.coreinit.shared_data.fonts[id] = Some((address, size));
                    Some((address, size))
                }
                None => None,
            }
        }
    };
    let Some((address, size)) = entry else { return Ok(false) };
    ctx.memory.write_u32(pointer_out, address)?;
    ctx.memory.write_u32(size_out, size)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A collection of one face with a single 5-byte table.
    fn collection() -> Vec<u8> {
        let mut file = b"ttcf\0\x02\0\0\0\0\0\x01\0\0\0\x10".to_vec();
        file.extend([0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        file.extend(*b"glyf");
        file.extend([0xAA; 4]);
        file.extend(44_u32.to_be_bytes());
        file.extend(5_u32.to_be_bytes());
        file.extend(b"hello");
        file
    }

    #[test]
    fn first_face_of_a_collection_becomes_a_plain_font() {
        let font = first_face(collection()).unwrap();
        assert_eq!(&font[..4], [0, 1, 0, 0]);
        assert_eq!(&font[12..16], b"glyf");
        assert_eq!(u32::from_be_bytes(font[20..24].try_into().unwrap()), 28);
        assert_eq!(u32::from_be_bytes(font[24..28].try_into().unwrap()), 5);
        assert_eq!(&font[28..33], b"hello");
        assert_eq!(font.len() % 4, 0, "tables stay aligned");
    }

    #[test]
    fn plain_fonts_pass_through_and_broken_collections_are_refused() {
        assert_eq!(first_face(vec![0, 1, 0, 0, 9]).unwrap(), [0, 1, 0, 0, 9]);
        assert!(first_face(b"ttcf\0\x02\0\0\0\0\0\x01\xFF\xFF\xFF\xFF".to_vec()).is_none());
        let mut truncated = collection();
        truncated.truncate(40);
        assert!(first_face(truncated).is_none());
    }

    #[test]
    fn cemu_installations_are_found_next_to_shocora() {
        let root = std::env::temp_dir().join(format!("shocora-fonts-{}", std::process::id()));
        let fonts = root.join("Cemu_2.6/resources/sharedFonts");
        std::fs::create_dir_all(&fonts).unwrap();
        std::fs::create_dir_all(root.join("shocora")).unwrap();
        assert_eq!(cemu_font_dirs(&root.join("shocora")), [fonts]);
        std::fs::remove_dir_all(root).unwrap();
    }
}
