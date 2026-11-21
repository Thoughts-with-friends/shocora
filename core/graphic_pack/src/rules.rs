//! What an enabled pack does to a running title: the sections of `rules.txt` read with
//! the selected presets (Cemu's `GraphicPack2::Activate`), and the files it brings.
use std::path::{Path, PathBuf};

use crate::{
    expr::{Expr, ExprError},
    ini,
    pack::{GraphicPack, PackError},
};

/// Which textures a [`TextureRule`] applies to, by their place in guest memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mem1Filter {
    #[default]
    Both,
    Inside,
    Outside,
}

/// `[TextureRedefine]`: textures matching the filter get the overwrite settings.
/// `-1` (or empty lists) means "any" in filters and "unchanged" in overwrites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureRule {
    pub width: i32,
    pub height: i32,
    pub depth: i32,
    pub mem1: Mem1Filter,
    pub formats: Vec<i32>,
    pub formats_excluded: Vec<i32>,
    pub tilemodes: Vec<i32>,
    pub tilemodes_excluded: Vec<i32>,
    pub overwrite_width: i32,
    pub overwrite_height: i32,
    pub overwrite_depth: i32,
    pub overwrite_format: i32,
    /// In 1/64 steps.
    pub overwrite_lod_bias: i32,
    pub overwrite_relative_lod_bias: i32,
    /// `log2` of the anisotropy (0 = 1x .. 4 = 16x).
    pub overwrite_anisotropy: i32,
}

impl Default for TextureRule {
    fn default() -> Self {
        Self {
            width: -1,
            height: -1,
            depth: -1,
            mem1: Mem1Filter::Both,
            formats: Vec::new(),
            formats_excluded: Vec::new(),
            tilemodes: Vec::new(),
            tilemodes_excluded: Vec::new(),
            overwrite_width: -1,
            overwrite_height: -1,
            overwrite_depth: -1,
            overwrite_format: -1,
            overwrite_lod_bias: -1,
            overwrite_relative_lod_bias: -1,
            overwrite_anisotropy: -1,
        }
    }
}

/// A texture as the rules see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureDesc {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub format: u32,
    pub tile_mode: u32,
    pub in_mem1: bool,
}

/// What the rules change about a texture (Cemu's `overwriteInfo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextureOverwrite {
    /// New size, when a rule changes it.
    pub size: Option<(u32, u32, u32)>,
    pub format: Option<u32>,
    pub lod_bias: Option<i32>,
    pub relative_lod_bias: Option<i32>,
    pub anisotropy: Option<i32>,
}

impl TextureOverwrite {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl TextureRule {
    /// Whether the rule applies to `tex` (Cemu's checks in the `LatteTexture` constructor).
    #[allow(clippy::cast_possible_wrap)]
    pub fn matches(&self, tex: &TextureDesc) -> bool {
        let listed = |list: &[i32], v: u32| list.contains(&(v as i32));
        (self.formats.is_empty() || listed(&self.formats, tex.format))
            && !listed(&self.formats_excluded, tex.format)
            && (self.tilemodes.is_empty() || listed(&self.tilemodes, tex.tile_mode))
            && !listed(&self.tilemodes_excluded, tex.tile_mode)
            && (self.width == -1 || self.width == tex.width as i32)
            && (self.height == -1 || self.height == tex.height as i32)
            && (self.depth == -1 || self.depth == tex.depth as i32)
            && match self.mem1 {
                Mem1Filter::Both => true,
                Mem1Filter::Inside => tex.in_mem1,
                Mem1Filter::Outside => !tex.in_mem1,
            }
    }
}

/// Applies every matching rule of every active pack in order: later ones win, and a
/// resolution overwrite keeps the sides it does not set (Cemu's loop).
#[allow(clippy::cast_sign_loss)]
pub fn overwrite_for<'a>(
    rules: impl IntoIterator<Item = &'a TextureRule>,
    tex: &TextureDesc,
) -> TextureOverwrite {
    let mut out = TextureOverwrite::default();
    for rule in rules {
        if !rule.matches(tex) {
            continue;
        }
        let mut size = (tex.width, tex.height, tex.depth);
        let mut resized = false;
        for (value, side) in [
            (rule.overwrite_width, &mut size.0),
            (rule.overwrite_height, &mut size.1),
            (rule.overwrite_depth, &mut size.2),
        ] {
            if value != -1 {
                *side = value.max(1) as u32;
                resized = true;
            }
        }
        if resized {
            out.size = Some(size);
        }
        if rule.overwrite_format != -1 {
            out.format = Some(rule.overwrite_format as u32);
        }
        if rule.overwrite_lod_bias != -1 {
            out.lod_bias = Some(rule.overwrite_lod_bias);
        }
        if rule.overwrite_relative_lod_bias != -1 {
            out.relative_lod_bias = Some(rule.overwrite_relative_lod_bias);
        }
        if rule.overwrite_anisotropy != -1 {
            out.anisotropy = Some(rule.overwrite_anisotropy);
        }
    }
    out
}

/// Kind of a replacement shader file (`<hash>_<hash>_ps.txt` ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderKind {
    Pixel,
    Vertex,
    Geometry,
    /// `output.glsl`, `upscaling.glsl`, `downscaling.glsl`.
    Output,
}

/// A shader file of the pack. shocora translates the console's shaders to WGSL itself;
/// these are GLSL written against Cemu's own bindings and are listed, not used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaderFile {
    pub path: PathBuf,
    pub kind: ShaderKind,
    pub base_hash: u64,
    pub aux_hash: u64,
}

/// Rules of an enabled pack for the running title.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ActiveRules {
    pub texture_rules: Vec<TextureRule>,
    /// `[Control] vsyncFrequency`: vertical blanks per second.
    pub vsync_frequency: Option<u32>,
    /// `[OutputShader]`: nearest-neighbour filtering when scaling the picture up / down.
    pub upscale_nearest: bool,
    pub downscale_nearest: bool,
    pub shaders: Vec<ShaderFile>,
    /// `patches.txt` / `patch_*.asm` of the pack.
    pub patch_files: Vec<PathBuf>,
}

fn rule_i32(expr: &Expr<'_>, section: &ini::Section, name: &str) -> Result<Option<i32>, ExprError> {
    #[allow(clippy::cast_possible_truncation)]
    section.get(name).map(|v| expr.eval(v).map(|v| v as i32)).transpose()
}

fn rule_list(expr: &Expr<'_>, section: &ini::Section, name: &str) -> Vec<i32> {
    #[allow(clippy::cast_possible_truncation)]
    section
        .get(name)
        .map(|v| v.split(',').filter_map(|t| expr.eval(t.trim()).ok()).map(|v| v as i32).collect())
        .unwrap_or_default()
}

fn parse_shader_name(name: &str) -> Option<(u64, u64, ShaderKind)> {
    let mut parts = name.splitn(3, '_');
    let base = u64::from_str_radix(parts.next()?, 16).ok()?;
    let aux = u64::from_str_radix(parts.next()?, 16).ok()?;
    let rest = parts.next()?;
    let kind = if rest.starts_with("ps") {
        ShaderKind::Pixel
    } else if rest.starts_with("vs") {
        ShaderKind::Vertex
    } else if rest.starts_with("gs") {
        ShaderKind::Geometry
    } else {
        return None;
    };
    Some((base, aux, kind))
}

impl GraphicPack {
    /// Reads the pack's rules with the selected presets.
    ///
    /// # Errors
    /// Fails if `rules.txt` cannot be read or an expression in it is invalid.
    pub fn activate(&self) -> Result<ActiveRules, PackError> {
        let text = std::fs::read(&self.rules_path)
            .map_err(|source| PackError::Io { path: self.rules_path.clone(), source })?;
        let sections = ini::parse(&String::from_utf8_lossy(&text));
        let expr = self.expr();
        let err = |error| PackError::Rule { path: self.rules_path.clone(), error };
        let mut rules = ActiveRules::default();
        for section in &sections {
            if section.is("TextureRedefine") {
                let mut rule = TextureRule::default();
                let get = |name| rule_i32(&expr, section, name).map_err(err);
                rule.width = get("width")?.unwrap_or(-1);
                rule.height = get("height")?.unwrap_or(-1);
                rule.depth = get("depth")?.unwrap_or(-1);
                if let Some(in_mem1) = get("inMEM1")? {
                    rule.mem1 = if in_mem1 != 0 { Mem1Filter::Inside } else { Mem1Filter::Outside };
                }
                rule.formats = rule_list(&expr, section, "formats");
                rule.formats_excluded = rule_list(&expr, section, "formatsExcluded");
                rule.tilemodes = rule_list(&expr, section, "tilemodes");
                rule.tilemodes_excluded = rule_list(&expr, section, "tilemodesExcluded");
                rule.overwrite_width = get("overwriteWidth")?.unwrap_or(-1);
                rule.overwrite_height = get("overwriteHeight")?.unwrap_or(-1);
                rule.overwrite_depth = get("overwriteDepth")?.unwrap_or(-1);
                rule.overwrite_format = get("overwriteFormat")?.unwrap_or(-1);
                #[allow(clippy::cast_possible_truncation)]
                let bias = |name| {
                    section
                        .get(name)
                        .map(|v| expr.eval(v).map(|b| (b as f32 * 64.0) as i32))
                        .transpose()
                        .map_err(err)
                };
                rule.overwrite_lod_bias = bias("overwriteLodBias")?.unwrap_or(-1);
                rule.overwrite_relative_lod_bias = bias("overwriteRelativeLodBias")?.unwrap_or(-1);
                if let Some(aniso) = get("overwriteAnisotropy")? {
                    match aniso {
                        1 => rule.overwrite_anisotropy = 0,
                        2 => rule.overwrite_anisotropy = 1,
                        4 => rule.overwrite_anisotropy = 2,
                        8 => rule.overwrite_anisotropy = 3,
                        16 => rule.overwrite_anisotropy = 4,
                        other => log::warn!(
                            "{}: invalid overwriteAnisotropy {other}",
                            self.rules_path.display()
                        ),
                    }
                }
                rules.texture_rules.push(rule);
            } else if section.is("Control") {
                #[allow(clippy::cast_sign_loss)]
                if let Some(freq) = rule_i32(&expr, section, "vsyncFrequency").map_err(err)? {
                    rules.vsync_frequency = (freq >= 1).then_some(freq as u32);
                }
            } else if section.is("OutputShader") {
                let nearest = |name| {
                    section.get(name).is_some_and(|v| v.eq_ignore_ascii_case("NearestNeighbor"))
                };
                rules.upscale_nearest = nearest("upscaleMagFilter");
                rules.downscale_nearest = nearest("downscaleMinFilter");
            }
        }
        if let Ok(entries) = std::fs::read_dir(self.dir()) {
            let mut files: Vec<PathBuf> =
                entries.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
            files.sort();
            for path in files {
                let name = path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase());
                let Some(name) = name else { continue };
                if let Some((base_hash, aux_hash, kind)) = parse_shader_name(&name) {
                    rules.shaders.push(ShaderFile { path, kind, base_hash, aux_hash });
                } else if matches!(
                    name.as_str(),
                    "output.glsl" | "upscaling.glsl" | "downscaling.glsl"
                ) {
                    rules.shaders.push(ShaderFile {
                        path,
                        kind: ShaderKind::Output,
                        base_hash: 0,
                        aux_hash: 0,
                    });
                } else if name == "patches.txt"
                    || (name.starts_with("patch_") && name.ends_with(".asm"))
                {
                    rules.patch_files.push(path);
                }
            }
        }
        Ok(rules)
    }
}

/// Which of the title's volumes a pack folder replaces files of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplacedVolume {
    Content,
    Aoc,
    Code,
}

/// Every pack under `root` (folders holding a `rules.txt`, not looked into further).
/// Packs that fail to load are logged and skipped.
pub fn load_all(root: &Path) -> Vec<GraphicPack> {
    let mut packs = Vec::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        let rules = dir.join("rules.txt");
        if rules.is_file() {
            match GraphicPack::load(&rules) {
                Ok(pack) => packs.push(pack),
                Err(error) => log::info!("graphic pack skipped: {error}"),
            }
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut subdirs: Vec<PathBuf> =
            entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        subdirs.sort();
        stack.extend(subdirs.into_iter().rev());
    }
    packs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Intent: resolution rules evaluate with the selected preset and change matching
    /// textures only, as Cemu's texture constructor does.
    /// Pass: a 1280x720 target becomes 1920x1080 at the 1080p preset; other sizes and
    /// excluded formats keep theirs; lod bias is stored in 1/64 steps.
    #[test]
    fn texture_rules_follow_presets() {
        let dir = std::env::temp_dir().join(format!("shocora_gp_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rules = dir.join("rules.txt");
        std::fs::write(
            &rules,
            "[Definition]\ntitleIds=0005000010116100\npath=a/b\nversion=6\n\
             [Default]\n$width=1280\n$height=720\n$gameWidth=1280\n$gameHeight=720\n\
             [Preset]\nname=720p\n[Preset]\nname=1080p\n$width=1920\n$height=1080\n\
             [TextureRedefine]\nwidth=1280\nheight=720\nformatsExcluded=0x431\n\
             overwriteWidth=($width/$gameWidth)*1280\noverwriteHeight=($height/$gameHeight)*720\n\
             [TextureRedefine]\noverwriteLodBias=-0.5\n[Control]\nvsyncFrequency=30*2\n",
        )
        .unwrap();
        std::fs::write(dir.join("patch_res.asm"), "").unwrap();
        std::fs::write(dir.join("0123456789abcdef_0000000000000000_ps.txt"), "").unwrap();
        let mut pack = GraphicPack::load(&rules).unwrap();
        pack.set_active_preset("", "1080p", true);
        let active = pack.activate().unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(active.vsync_frequency, Some(60));
        assert_eq!(active.patch_files.len(), 1);
        assert_eq!(active.shaders.len(), 1);
        let tex = TextureDesc {
            width: 1280,
            height: 720,
            depth: 1,
            format: 0x1A,
            tile_mode: 4,
            in_mem1: true,
        };
        let o = overwrite_for(&active.texture_rules, &tex);
        assert_eq!(o.size, Some((1920, 1080, 1)));
        assert_eq!(o.lod_bias, Some(-32));
        let other = TextureDesc { width: 640, ..tex };
        assert_eq!(overwrite_for(&active.texture_rules, &other).size, None);
        let excluded = TextureDesc { format: 0x431, ..tex };
        assert_eq!(overwrite_for(&active.texture_rules, &excluded).size, None);
    }

    /// Intent: the real packs Cemu downloaded load (when present on this machine).
    /// Pass: no panic; more than 100 packs parse.
    #[test]
    #[ignore = "needs Cemu's downloaded graphic packs"]
    fn loads_cemus_downloaded_packs() {
        let root = std::env::var_os("APPDATA")
            .map(|a| PathBuf::from(a).join("Cemu/graphicPacks/downloadedGraphicPacks"))
            .unwrap();
        let packs = load_all(&root);
        let mut failures = 0;
        for pack in &packs {
            if let Err(e) = pack.activate() {
                failures += 1;
                println!("ACTIVATE FAIL {e}");
            }
        }
        println!("loaded {} packs, {failures} failed to activate", packs.len());
        assert!(packs.len() > 100);
    }
}
