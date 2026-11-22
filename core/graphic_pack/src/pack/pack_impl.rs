//! Part of [`super`]: see its documentation.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!               ▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ one pack folder    │   │ pack/pack_impl.rs        │   │ presets, rules     │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ a graphic pack: pack     │   │                    │
//! │                    │   │ impl                     │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "pack impl" share of pack.rs (a graphic pack), moved here so that no file is longer than
//! 500 lines. It works exactly as if it were still written in that file. One pack with its
//! presets (for example "Resolution: 1440p"). Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;

impl GraphicPack {
    /// Loads the pack whose `rules.txt` is `rules_path`.
    ///
    /// # Errors
    /// Fails if the file cannot be read or is not a valid version 3+ pack.
    pub fn load(rules_path: &Path) -> Result<Self, PackError> {
        let text = std::fs::read(rules_path)
            .map_err(|source| PackError::Io { path: rules_path.to_owned(), source })?;
        Self::parse(rules_path, &String::from_utf8_lossy(&text))
    }

    /// Parses `text` as the `rules.txt` at `rules_path`.
    ///
    /// # Errors
    /// See [`Self::load`].
    #[allow(clippy::too_many_lines)]
    pub fn parse(rules_path: &Path, text: &str) -> Result<Self, PackError> {
        let path = || rules_path.to_owned();
        let sections = ini::parse(text);
        let Some(definition) = sections.first().filter(|s| s.is("Definition")) else {
            return Err(PackError::NoDefinition(path()));
        };
        let version: i64 = definition
            .get("version")
            .and_then(|v| v.trim().parse().ok())
            .filter(|v| *v >= 0)
            .ok_or_else(|| PackError::Version(path()))?;
        if version <= LEGACY_VERSION {
            return Err(PackError::Outdated(path()));
        }
        let renderer_filter =
            definition.get("rendererFilter").and_then(|f| match f.to_ascii_lowercase().as_str() {
                "vulkan" => Some(RendererFilter::Vulkan),
                "opengl" => Some(RendererFilter::OpenGl),
                "metal" => Some(RendererFilter::Metal),
                other => {
                    log::warn!("unknown rendererFilter '{other}'");
                    None
                }
            });
        let default_enabled = definition.get("default").is_some_and(truthy);
        let virtual_path = definition.get("path").ok_or_else(|| PackError::NoPath(path()))?;
        let mut universal = false;
        let mut title_ids = Vec::new();
        if let Some(ids) = definition.get("titleIds") {
            if ids.trim() == "*" {
                universal = true;
            } else {
                title_ids = ids.split(',').filter_map(parse_hex_u64).collect();
            }
        }
        if title_ids.is_empty() && !universal {
            return Err(PackError::NoTitles(path()));
        }
        let mut pack = Self {
            rules_path: rules_path.to_owned(),
            version,
            name: definition.get("name").unwrap_or_default().to_owned(),
            virtual_path: virtual_path.to_owned(),
            description: definition.get("description").unwrap_or_default().replace('|', "\n"),
            default_enabled,
            enabled: default_enabled,
            title_ids,
            universal,
            renderer_filter,
            vendor_filter: definition.get("vendorFilter").map(str::to_ascii_lowercase),
            fs_priority: definition
                .get("fsPriority")
                .and_then(|p| p.trim().parse().ok())
                .unwrap_or(100),
            colorbuffer_optimization_aware: definition
                .get("colorbufferOptimizationAware")
                .is_some_and(truthy),
            presets: Vec::new(),
            default_vars: Vec::new(),
            ram_mappings: Vec::new(),
            permissions: Vec::new(),
        };

        for section in &sections[1..] {
            if section.is("Default") {
                pack.default_vars =
                    parse_vars(section).map_err(|error| PackError::Rule { path: path(), error })?;
            } else if section.is("Preset") {
                let Some(name) = section.get("name") else {
                    log::warn!(
                        "{}: preset in line {} has no name",
                        rules_path.display(),
                        section.line
                    );
                    continue;
                };
                let variables = match parse_vars(section) {
                    Ok(vars) => vars,
                    Err(error) => {
                        log::warn!(
                            "{}",
                            PackError::PresetVar { path: path(), preset: name.to_owned(), error }
                        );
                        continue;
                    }
                };
                pack.presets.push(Preset {
                    category: section.get("category").unwrap_or_default().to_owned(),
                    name: name.to_owned(),
                    condition: section.get("condition").unwrap_or_default().to_owned(),
                    variables,
                    active: false,
                    visible: true,
                    is_default: section
                        .get("default")
                        .is_some_and(|d| d.trim().parse::<i64>().is_ok_and(|v| v != 0)),
                });
            } else if section.is("RAM") {
                for i in 0..32 {
                    let option = format!("mapping{i}");
                    let Some(value) = section.get(&option) else { continue };
                    let bad = || PackError::Ram { path: path(), option: option.clone() };
                    if version <= 5 {
                        return Err(bad());
                    }
                    let (start, end) = value.split_once('-').ok_or_else(bad)?;
                    let parse = |s: &str| {
                        let s = s.trim();
                        let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"));
                        s.and_then(|s| u32::from_str_radix(s, 16).ok())
                    };
                    let (start, end) = (parse(start).ok_or_else(bad)?, parse(end).ok_or_else(bad)?);
                    if end <= start || start & 0xFFF != 0 || end & 0xFFF != 0 {
                        return Err(bad());
                    }
                    pack.ram_mappings.push((start, end));
                }
            } else if section.is("Permissions") {
                const GROUPS: [&str; 15] = [
                    "BSP", "DK", "USB", "UHS", "FS", "MCP", "NIM", "ACT", "FPD", "BOSS", "ACP",
                    "PDM", "AC", "NDM", "NSEC",
                ];
                for group in GROUPS {
                    if let Some(mask) = section.get(group).and_then(parse_hex_u64) {
                        pack.permissions.push((group.to_owned(), mask));
                    }
                }
            }
        }

        if version >= 5 {
            // Every preset variable must have a default; its type comes from there.
            for preset in &mut pack.presets {
                for (name, var) in &mut preset.variables {
                    let Some((_, default)) = pack.default_vars.iter().find(|(n, _)| n == name)
                    else {
                        return Err(PackError::UndefinedVar {
                            path: path(),
                            preset: preset.name.clone(),
                            var: name.trim_start_matches('$').to_owned(),
                        });
                    };
                    var.0 = default.0;
                }
            }
            for category in pack.categories() {
                let members: Vec<usize> = pack.category_indices(&category);
                let pick = members
                    .iter()
                    .copied()
                    .find(|&i| pack.presets[i].is_default)
                    .or_else(|| members.first().copied());
                if let Some(i) = pick {
                    pack.presets[i].active = true;
                }
            }
        } else {
            for category in pack.categories() {
                let members = pack.category_indices(&category);
                for pair in members.windows(2) {
                    let names = |i: usize| {
                        let mut v: Vec<&str> =
                            pack.presets[i].variables.iter().map(|(n, _)| n.as_str()).collect();
                        v.sort_unstable();
                        v
                    };
                    if names(pair[0]) != names(pair[1]) {
                        return Err(PackError::Inconsistent { path: path(), category });
                    }
                }
                // Cemu looks at all but the last preset for `default`.
                let default = members[..members.len().saturating_sub(1)]
                    .iter()
                    .copied()
                    .find(|&i| pack.presets[i].is_default);
                pack.presets[default.unwrap_or(members[0])].active = true;
            }
        }
        pack.update_preset_visibility();
        pack.validate_preset_selections();
        Ok(pack)
    }

    /// Name shown for the pack.
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() { &self.virtual_path } else { &self.name }
    }

    /// Folder holding the pack.
    pub fn dir(&self) -> &Path {
        self.rules_path.parent().unwrap_or_else(|| Path::new("."))
    }

    pub fn contains_title_id(&self, title_id: u64) -> bool {
        self.universal || self.title_ids.contains(&title_id)
    }

    /// Preset categories in the order they first appear.
    pub fn categories(&self) -> Vec<String> {
        let mut order: Vec<String> = Vec::new();
        for preset in &self.presets {
            if !order.contains(&preset.category) {
                order.push(preset.category.clone());
            }
        }
        order
    }

    /// Indices of the presets of `category`.
    pub fn category_indices(&self, category: &str) -> Vec<usize> {
        (0..self.presets.len()).filter(|&i| self.presets[i].category == category).collect()
    }

    /// Name of the selected preset of `category`.
    pub fn active_preset(&self, category: &str) -> Option<&str> {
        self.presets.iter().find(|p| p.active && p.category == category).map(|p| p.name.as_str())
    }

    /// Selects preset `name` of `category` (an empty name clears the selection).
    /// Returns whether it exists.
    pub fn set_active_preset(
        &mut self,
        category: &str,
        name: &str,
        update_visibility: bool,
    ) -> bool {
        for preset in &mut self.presets {
            if preset.category == category {
                preset.active = false;
            }
        }
        if name.is_empty() {
            return true;
        }
        let found = self.presets.iter_mut().find(|p| p.category == category && p.name == name);
        let result = found.map(|p| p.active = true).is_some();
        if update_visibility {
            self.update_preset_visibility();
            self.validate_preset_selections();
        }
        result
    }

    /// Adds the variables in force to `expr`: active and shown presets first, then active
    /// hidden ones, then the defaults (Cemu's `FillPresetConstants`).
    pub fn fill_preset_constants(&self, expr: &mut Expr<'_>) {
        if self.version < 5 {
            for preset in self.presets.iter().filter(|p| p.active) {
                for (name, (_, value)) in &preset.variables {
                    expr.add_constant(name, *value);
                }
            }
            return;
        }
        for preset in self.presets.iter().filter(|p| p.active && p.visible) {
            for (name, (ty, value)) in &preset.variables {
                expr.add_constant(name, typed(*ty, *value));
            }
        }
        for preset in self.presets.iter().filter(|p| p.active && !p.visible) {
            for (name, (ty, value)) in &preset.variables {
                expr.try_add_constant(name, typed(*ty, *value));
            }
        }
        for (name, (ty, value)) in &self.default_vars {
            expr.try_add_constant(name, typed(*ty, *value));
        }
    }

    /// The value of preset variable `name` in force.
    pub fn preset_var(&self, name: &str) -> Option<PresetVar> {
        let active = || self.presets.iter().filter(|p| p.active);
        if self.version < 5 {
            return active().find_map(|p| p.var(name));
        }
        active()
            .filter(|p| p.visible)
            .find_map(|p| p.var(name))
            .or_else(|| active().filter(|p| !p.visible).find_map(|p| p.var(name)))
            .or_else(|| self.default_vars.iter().find(|(n, _)| n == name).map(|(_, v)| *v))
    }

    pub(super) fn is_preset_visible(&self, index: usize) -> bool {
        let condition = &self.presets[index].condition;
        if condition.is_empty() {
            return true;
        }
        let mut expr = Expr::integer();
        self.fill_preset_constants(&mut expr);
        expr.eval(condition).is_ok_and(|v| v != 0.0)
    }

    pub fn update_preset_visibility(&mut self) {
        for i in 0..self.presets.len() {
            self.presets[i].visible = self.version < 5 || self.is_preset_visible(i);
        }
    }

    /// Keeps one shown preset selected in every category that has any shown.
    pub fn validate_preset_selections(&mut self) {
        if self.version < 5 {
            return;
        }
        let mut changed = false;
        for category in self.categories() {
            let members = self.category_indices(&category);
            let mut default = None;
            let mut selected = None;
            let mut visible = 0;
            for &i in &members {
                if self.presets[i].visible {
                    visible += 1;
                    if default.is_none() || self.presets[i].is_default {
                        default = Some(i);
                    }
                }
                if self.presets[i].active {
                    if selected.is_some() {
                        self.presets[i].active = false;
                        changed = true;
                    } else {
                        selected = Some(i);
                    }
                }
            }
            if visible == 0 {
                continue;
            }
            match (selected, default) {
                (None, Some(d)) => self.presets[d].active = true,
                (Some(s), Some(d)) if !self.presets[s].visible => {
                    self.presets[s].active = false;
                    self.presets[d].active = true;
                    changed = true;
                }
                _ => {}
            }
        }
        if changed {
            self.update_preset_visibility();
        }
    }

    /// An expression evaluator holding the variables of the selected presets.
    pub fn expr(&self) -> Expr<'static> {
        let mut expr = Expr::new();
        self.fill_preset_constants(&mut expr);
        expr
    }
}

pub(super) fn typed(ty: VarType, value: f64) -> f64 {
    match ty {
        VarType::Double => value,
        VarType::Int => value.trunc(),
    }
}
