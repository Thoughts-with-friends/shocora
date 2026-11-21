//! The graphic pack window, after Cemu's: the packs as a tree of their paths (game, then
//! the pack's folders) with a switch on every pack, the chosen one's description (which can
//! be machine-translated) and presets beside it, and the download of Cemu's community packs.
//! Every choice is saved at once. The packs' replaced files (mods) take effect in the
//! running title; their GPU rules at the next boot.
use std::collections::{BTreeMap, HashSet};

use shocora_gui_common::{
    graphic_packs::{self, PackSettings, UpdateState, load_packs, pack_key, pack_setting},
    translate::{Translation, Translator},
};
use wiiu_graphic_pack::GraphicPack;

use super::*;

const DEFAULT_SIZE: egui::Vec2 = egui::vec2(820.0, 560.0);
const MIN_SIZE: egui::Vec2 = egui::vec2(520.0, 320.0);
/// Share of the width that goes to the tree; the rest to the chosen pack.
const TREE_SHARE: f32 = 0.55;

/// The window's state.
#[derive(Debug, Default)]
pub(super) struct PackWindow {
    packs: Vec<GraphicPack>,
    selected: Option<usize>,
    /// Text the tree is filtered by.
    search: String,
    /// Show only the packs of titles in the library (or running).
    installed_only: bool,
    /// The download whose packs the tree shows.
    loaded_version: Option<String>,
}

enum PackAction {
    Toggle(usize),
    Select(usize),
    Preset { pack: usize, category: String, name: String },
    Translate(String),
    Check,
    Install(graphic_packs::Release),
    Close,
}

/// What the window shows besides its own state.
struct View<'a> {
    state: &'a UpdateState,
    installed: &'a HashSet<u64>,
    translator: &'a Translator,
    /// Language descriptions are translated into; `None` for English.
    target: Option<&'a str>,
}

/// A folder of the tree: its subfolders and the packs right in it.
#[derive(Default)]
struct Folder<'a> {
    folders: BTreeMap<String, Self>,
    packs: Vec<(usize, &'a str)>,
}

impl<'a> Folder<'a> {
    fn insert(&mut self, path: &'a str, index: usize) {
        match path.split_once('/') {
            Some((folder, rest)) if !rest.is_empty() => {
                self.folders.entry(folder.to_owned()).or_default().insert(rest, index);
            }
            _ => self.packs.push((index, path)),
        }
    }
}

/// Language the window's text is in, as a translation service names it.
fn target_language() -> Option<&'static str> {
    let code = tr("language.code");
    (code != "language.code" && code != "en").then_some(code)
}

impl ShocoraApp {
    /// Looks for a newer release of the community packs at start, if packs were downloaded
    /// before and the user did not turn it off.
    pub(super) fn check_graphic_pack_updates(&self) {
        let root = &self.config.graphic_pack_dir;
        if self.config.check_graphic_pack_updates
            && graphic_packs::installed_version(root).is_some()
        {
            self.pack_updater.check(root.clone());
        }
    }

    pub(super) fn open_graphic_packs(&mut self) {
        let mut window = PackWindow { installed_only: true, ..PackWindow::default() };
        self.reload_packs(&mut window);
        self.graphic_packs = Some(window);
    }

    fn reload_packs(&self, window: &mut PackWindow) {
        let root = &self.config.graphic_pack_dir;
        let mut packs = load_packs(root, &self.config.graphic_packs);
        packs.sort_by_cached_key(|pack| pack.virtual_path.to_lowercase());
        window.packs = packs;
        window.selected = None;
        window.loaded_version = graphic_packs::installed_version(root);
    }

    /// Titles whose packs the tree shows with "installed games" on.
    fn installed_titles(&self) -> HashSet<u64> {
        self.library.games.iter().filter_map(|g| g.title_id).chain(self.title_id).collect()
    }

    /// Saves the window's choices and hands them to the emulator.
    fn store_graphic_packs(&mut self) {
        let Some(window) = &mut self.graphic_packs else { return };
        let root = &self.config.graphic_pack_dir;
        let settings: PackSettings = window
            .packs
            .iter()
            .filter_map(|pack| pack_setting(pack).map(|s| (pack_key(root, pack), s)))
            .collect();
        self.config.graphic_packs = settings;
        self.save_config();
        self.console.send(ConsoleCommand::GraphicPacks {
            dir: self.config.graphic_pack_dir.clone(),
            packs: self.config.graphic_packs.clone(),
        });
    }

    fn pack_action(&mut self, action: PackAction, ctx: &egui::Context) {
        let Some(window) = &mut self.graphic_packs else { return };
        match action {
            PackAction::Toggle(index) => {
                if let Some(pack) = window.packs.get_mut(index) {
                    pack.enabled = !pack.enabled;
                }
                window.selected = Some(index);
                self.store_graphic_packs();
            }
            PackAction::Select(index) => window.selected = Some(index),
            PackAction::Preset { pack, category, name } => {
                if let Some(pack) = window.packs.get_mut(pack) {
                    pack.set_active_preset(&category, &name, true);
                    pack.validate_preset_selections();
                }
                self.store_graphic_packs();
            }
            PackAction::Translate(text) => {
                if let Some(target) = target_language() {
                    let ctx = ctx.clone();
                    self.translator.request(&text, target, move || ctx.request_repaint());
                }
            }
            PackAction::Check => self.pack_updater.check(self.config.graphic_pack_dir.clone()),
            PackAction::Install(release) => {
                self.pack_updater.install(self.config.graphic_pack_dir.clone(), release);
            }
            PackAction::Close => self.graphic_packs = None,
        }
    }

    pub(super) fn graphic_pack_window(&mut self, ctx: &egui::Context) {
        let state = self.pack_updater.state();
        if state.is_busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let mut action = None;
        if let Some(mut window) = self.graphic_packs.take() {
            // A finished download shows at once.
            if let UpdateState::Installed(version) = &state
                && window.loaded_version.as_ref() != Some(version)
            {
                self.reload_packs(&mut window);
            }
            let mut open = true;
            let installed = self.installed_titles();
            let view = View {
                state: &state,
                installed: &installed,
                translator: &self.translator,
                target: target_language(),
            };
            let check_at_start = self.config.check_graphic_pack_updates;
            let mut check = check_at_start;
            egui::Window::new(tr("packs.title"))
                .open(&mut open)
                .collapsible(false)
                .resizable(true)
                .default_size(DEFAULT_SIZE)
                .min_size(MIN_SIZE)
                .show(ctx, |ui| pack_contents(ui, &mut window, &view, &mut check, &mut action));
            if check != check_at_start {
                self.config.check_graphic_pack_updates = check;
                self.save_config();
            }
            if !open {
                action = Some(PackAction::Close);
            }
            self.graphic_packs = Some(window);
        } else if let UpdateState::Available(release) = &state {
            // Found at start: offered on its own, as Cemu does.
            egui::Window::new(tr("packs.update_title"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::RIGHT_BOTTOM, [-12.0, -40.0])
                .show(ctx, |ui| {
                    ui.label(trf("packs.update_available", &[("version", &release.name)]));
                    ui.horizontal(|ui| {
                        if ui.button(tr("packs.download")).clicked() {
                            action = Some(PackAction::Install(release.clone()));
                        }
                        if ui.button(tr("packs.later")).clicked() {
                            self.pack_updater.dismiss();
                        }
                    });
                });
        } else if matches!(state, UpdateState::Downloading(_) | UpdateState::Extracting(_)) {
            egui::Window::new(tr("packs.update_title"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::RIGHT_BOTTOM, [-12.0, -40.0])
                .show(ctx, |ui| ui.label(update_text(&state)));
        }
        if let Some(action) = action {
            self.pack_action(action, ctx);
        }
    }
}

/// What the download is doing, in words.
fn update_text(state: &UpdateState) -> String {
    match state {
        UpdateState::Idle => String::new(),
        UpdateState::Checking => tr("packs.checking").to_owned(),
        UpdateState::UpToDate(version) => trf("packs.up_to_date", &[("version", version)]),
        UpdateState::Available(release) => {
            trf("packs.update_available", &[("version", &release.name)])
        }
        UpdateState::Downloading(p) => {
            trf("packs.downloading", &[("percent", &format!("{:.0}", p * 100.0))])
        }
        UpdateState::Extracting(p) => {
            trf("packs.extracting", &[("percent", &format!("{:.0}", p * 100.0))])
        }
        UpdateState::Installed(version) => trf("packs.installed", &[("version", version)]),
        UpdateState::Failed(error) => trf("packs.failed", &[("error", error)]),
    }
}

fn pack_contents(
    ui: &mut egui::Ui,
    window: &mut PackWindow,
    view: &View<'_>,
    check_at_start: &mut bool,
    action: &mut Option<PackAction>,
) {
    ui.horizontal(|ui| {
        ui.label(tr("packs.filter"));
        ui.add(egui::TextEdit::singleline(&mut window.search).desired_width(220.0));
        ui.checkbox(&mut window.installed_only, tr("packs.installed_games"));
    });
    // Room left for the two panes once the bottom rows are counted.
    let row = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
    let height = row.mul_add(-3.0, ui.available_height()).max(120.0);
    let tree_width = ui.available_width() * TREE_SHARE;
    ui.horizontal_top(|ui| {
        egui::Frame::group(ui.style()).inner_margin(2.0).show(ui, |ui| {
            ui.set_width(tree_width);
            egui::ScrollArea::both()
                .id_salt("pack_tree")
                .auto_shrink(false)
                .max_height(height)
                .min_scrolled_height(height)
                .show(ui, |ui| pack_tree(ui, window, view, action));
        });
        ui.vertical(|ui| {
            egui::ScrollArea::vertical()
                .id_salt("pack_details")
                .auto_shrink(false)
                .max_height(height)
                .show(ui, |ui| pack_details(ui, window, view, action));
        });
    });
    ui.horizontal(|ui| {
        ui.checkbox(check_at_start, tr("packs.check_at_start"));
        let label = match &window.loaded_version {
            Some(_) => tr("packs.check_updates"),
            None => tr("packs.download"),
        };
        if ui.add_enabled(!view.state.is_busy(), egui::Button::new(label)).clicked() {
            *action = Some(PackAction::Check);
        }
        if let UpdateState::Available(release) = view.state
            && ui.button(tr("packs.download")).clicked()
        {
            *action = Some(PackAction::Install(release.clone()));
        }
        ui.add(egui::Label::new(update_text(view.state)).truncate());
    });
    ui.separator();
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(tr("packs.close")).clicked() {
                *action = Some(PackAction::Close);
            }
            ui.add(egui::Label::new(tr("packs.next_boot")).truncate());
        });
    });
}

/// The packs shown, as folders of their paths with a switch on every pack.
fn pack_tree(
    ui: &mut egui::Ui,
    window: &PackWindow,
    view: &View<'_>,
    action: &mut Option<PackAction>,
) {
    let search = window.search.to_lowercase();
    let mut root = Folder::default();
    for (index, pack) in window.packs.iter().enumerate() {
        let shown = (search.is_empty() || pack.virtual_path.to_lowercase().contains(&search))
            && (!window.installed_only
                || pack.universal
                || pack.title_ids.iter().any(|id| view.installed.contains(id)));
        if shown {
            root.insert(&pack.virtual_path, index);
        }
    }
    if root.folders.is_empty() && root.packs.is_empty() {
        ui.label(tr("packs.none"));
        return;
    }
    // A search opens every folder holding a match.
    folder_ui(ui, window, &root, "", !search.is_empty(), action);
}

fn folder_ui(
    ui: &mut egui::Ui,
    window: &PackWindow,
    folder: &Folder<'_>,
    path: &str,
    open: bool,
    action: &mut Option<PackAction>,
) {
    for (name, sub) in &folder.folders {
        let sub_path = format!("{path}/{name}");
        let header = egui::CollapsingHeader::new(name).id_salt(("pack_folder", &sub_path));
        let header = if open { header.open(Some(true)) } else { header };
        header.show(ui, |ui| folder_ui(ui, window, sub, &sub_path, open, action));
    }
    for &(index, name) in &folder.packs {
        let Some(pack) = window.packs.get(index) else { continue };
        ui.horizontal(|ui| {
            let mut enabled = pack.enabled;
            if ui.checkbox(&mut enabled, "").changed() {
                *action = Some(PackAction::Toggle(index));
            }
            let chosen = window.selected == Some(index);
            if ui.selectable_label(chosen, name).clicked() && !chosen {
                *action = Some(PackAction::Select(index));
            }
        });
    }
}

/// The chosen pack's name, description (translated on request) and its presets.
fn pack_details(
    ui: &mut egui::Ui,
    window: &PackWindow,
    view: &View<'_>,
    action: &mut Option<PackAction>,
) {
    let Some((index, pack)) = window.selected.and_then(|i| window.packs.get(i).map(|p| (i, p)))
    else {
        ui.label(tr("packs.select"));
        return;
    };
    ui.heading(pack.display_name());
    ui.separator();
    if !pack.description.is_empty() {
        let translated = view.target.and_then(|t| view.translator.get(&pack.description, t));
        match &translated {
            Some(Translation::Done(text)) => {
                ui.label(text);
                ui.collapsing(tr("packs.original"), |ui| ui.label(&pack.description));
            }
            _ => {
                ui.label(&pack.description);
            }
        }
        if view.target.is_some() {
            match translated {
                None => {
                    if ui
                        .button(tr("packs.translate"))
                        .on_hover_text(tr("packs.translate_hint"))
                        .clicked()
                    {
                        *action = Some(PackAction::Translate(pack.description.clone()));
                    }
                }
                Some(Translation::Pending) => {
                    ui.label(tr("packs.translating"));
                }
                Some(Translation::Failed(error)) => {
                    ui.label(trf("packs.translate_failed", &[("error", &error)]));
                }
                Some(Translation::Done(_)) => {}
            }
        }
        ui.separator();
    }
    for category in pack.categories() {
        let visible: Vec<&str> = pack
            .category_indices(&category)
            .into_iter()
            .map(|i| &pack.presets[i])
            .filter(|p| p.visible)
            .map(|p| p.name.as_str())
            .collect();
        if visible.is_empty() {
            continue;
        }
        let current = pack.active_preset(&category).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label(if category.is_empty() { tr("packs.preset") } else { &category });
            egui::ComboBox::from_id_salt(("pack_preset", index, &category))
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for name in visible {
                        if ui.selectable_label(name == current, name).clicked() && name != current {
                            *action = Some(PackAction::Preset {
                                pack: index,
                                category: category.clone(),
                                name: name.to_owned(),
                            });
                        }
                    }
                });
        });
    }
}
