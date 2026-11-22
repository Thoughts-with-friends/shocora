//! Port of Cemu's `GraphicPack2`: one pack (a folder with a `rules.txt`), its presets and
//! the rules it applies to a running title.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!               ▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ one pack folder    │   │ pack.rs                  │   │ presets, rules     │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ a graphic pack           │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! One pack with its presets (for example "Resolution: 1440p"). Terms such as HLE, JIT or
//! shader are explained in `docs/guide/wiiu_term.md`.
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::{
    expr::Expr,
    ini::{self, Section},
};

/// Packs older than this are Cemu 1.x "legacy" packs and are not loaded.
const LEGACY_VERSION: i64 = 2;

#[derive(Debug, Error)]
pub enum PackError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{0}: [Definition] must be the first section")]
    NoDefinition(PathBuf),
    #[error("{0}: missing or invalid version")]
    Version(PathBuf),
    #[error("{0}: outdated graphic pack (version 2 or older)")]
    Outdated(PathBuf),
    #[error("{0}: [Definition] must contain the option: path")]
    NoPath(PathBuf),
    #[error("{0}: no title IDs")]
    NoTitles(PathBuf),
    #[error("{path}: preset \"{preset}\" uses ${var}, which [Default] does not define")]
    UndefinedVar { path: PathBuf, preset: String, var: String },
    #[error("{path}: preset \"{preset}\": {error}")]
    PresetVar { path: PathBuf, preset: String, error: crate::expr::ExprError },
    #[error("{path}: inconsistent preset variables in category \"{category}\"")]
    Inconsistent { path: PathBuf, category: String },
    #[error("{path}: invalid [RAM] {option}")]
    Ram { path: PathBuf, option: String },
    #[error("{path}: {error}")]
    Rule { path: PathBuf, error: crate::expr::ExprError },
}

/// `$name:int` or `$name` (double).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarType {
    Double,
    Int,
}

/// A preset variable: its type and value.
pub type PresetVar = (VarType, f64);

/// One `[Preset]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    /// Empty for the uncategorised presets of older packs.
    pub category: String,
    pub name: String,
    /// Expression over the other presets' variables deciding whether this one is shown.
    pub condition: String,
    pub variables: Vec<(String, PresetVar)>,
    pub active: bool,
    pub visible: bool,
    pub is_default: bool,
}

impl Preset {
    fn var(&self, name: &str) -> Option<PresetVar> {
        self.variables.iter().find(|(n, _)| n == name).map(|(_, v)| *v)
    }
}

/// Renderers a pack may be limited to (`rendererFilter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererFilter {
    OpenGl,
    Vulkan,
    Metal,
}

/// A loaded graphic pack.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphicPack {
    /// The `rules.txt`.
    pub rules_path: PathBuf,
    pub version: i64,
    pub name: String,
    /// Place in the pack tree, `/`-separated (`Xenoblade Chronicles X/Graphics`).
    pub virtual_path: String,
    pub description: String,
    pub default_enabled: bool,
    pub enabled: bool,
    pub title_ids: Vec<u64>,
    /// `titleIds = *`: every title.
    pub universal: bool,
    pub renderer_filter: Option<RendererFilter>,
    pub vendor_filter: Option<String>,
    /// Priority of the pack's replaced files over others (`fsPriority`, default 100).
    pub fs_priority: i32,
    /// The pack sizes render targets exactly (`colorbufferOptimizationAware`).
    pub colorbuffer_optimization_aware: bool,
    pub presets: Vec<Preset>,
    /// `[Default]` variables.
    pub default_vars: Vec<(String, PresetVar)>,
    /// `[RAM] mappingN = start - end`: extra guest memory (version 6+).
    pub ram_mappings: Vec<(u32, u32)>,
    /// `[Permissions]`: capability group name and mask.
    pub permissions: Vec<(String, u64)>,
}

/// Parses the `$` options of a section, each able to use the ones before it.
fn parse_vars(section: &Section) -> Result<Vec<(String, PresetVar)>, crate::expr::ExprError> {
    let mut expr = Expr::new();
    let mut vars: Vec<(String, PresetVar)> = Vec::new();
    for (option, value) in &section.options {
        if !option.starts_with('$') {
            continue;
        }
        let (name, ty) = match option.split_once(':') {
            Some((name, ty)) => {
                let ty = match ty.trim() {
                    "int" => VarType::Int,
                    _ => VarType::Double,
                };
                (name.trim().to_owned(), ty)
            }
            None => (option.clone(), VarType::Double),
        };
        let value = expr.eval(value)?;
        if !vars.iter().any(|(n, _)| *n == name) {
            vars.push((name.clone(), (ty, value)));
        }
        expr.add_constant(&name, value);
    }
    Ok(vars)
}

fn truthy(value: &str) -> bool {
    value.eq_ignore_ascii_case("true") || value == "1"
}

/// `ConvertString<uint64>(token, 16)`: hex digits, stopping at the first other character.
fn parse_hex_u64(token: &str) -> Option<u64> {
    let t = token.trim();
    let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    let digits: String = t.chars().take_while(char::is_ascii_hexdigit).collect();
    u64::from_str_radix(&digits, 16).ok()
}

#[cfg(test)]
#[path = "pack_tests.rs"]
mod tests;

mod pack_impl;
#[allow(unused_imports)]
pub(crate) use pack_impl::*;
