//! The shader dialog: the post-process shader picked from a collection combo
//! box and, for a Mega Bezel pack, one directory level at a time.
//!
//! The top row picks a *collection*: `Default`, which is whatever shader
//! `--shader` (or the settings dialog) last chose, and one entry per bezel pack
//! found under `shaders/Mega_Bezel_Packs`. `Default` is a single preset with
//! nothing to browse, so the rows below it are greyed out. A pack is not a
//! handful of shaders but a directory tree of tens of thousands of `.slangp`
//! presets, laid out
//! `<machine>/<monitor>/<flavour>/<scaling>_<curvature>_<lighting>.slangp`
//! (see `docs/SHADERS.md`). That is far too many for the fuzzy list and not
//! something [`crate::settings`] can draw either -- its combo boxes come from a
//! reflected enum's variant list, and these choices are only known once a
//! directory has been read. So this dialog draws one combo box per level and
//! fills each from what the level above selected:
//!
//! ```text
//! Commodore / Commodore_Amiga500 / Commodore_C1084 / MBZ_SHARP_STD / NEAR_CURVED_NIGHT.slangp
//! Collection  System              Monitor           Shader          Type       Day/Night
//! ```
//!
//! Like the settings dialog, a pick takes effect the moment it is made: the
//! selection is composed back into a path and written straight to
//! [`ShaderPath`], which the render world extracts.
//!
//! The tree is walked lazily, one `read_dir` per level as the level above
//! changes, because the pack holds ~72k presets and only ~60 directory entries
//! are ever on screen. [`PresetBrowser`] is the whole of that logic and knows
//! nothing about egui; the tests exercise it against a tree they build.

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use bevy_egui::{
    EguiContexts, EguiPrimaryContextPass,
    egui::{self, Ui},
};

use crate::config::{RenderSettings, ShaderArg};
use crate::egui_ui::{HudState, live_modifiers, panel_frame, sync_modifiers, take_key, update_ui};
use crate::post_process::{ShaderEffect, ShaderPath};
// The dialog chrome -- panel metrics, the widget scaling and the close button --
// is the settings dialog's, so the two look like one dialog with two contents.
use crate::settings::{
    BODY_SIZE, CLOSE_SIZE, DISABLED_COLOR, DemarcSettings, GRID_HEIGHT_FRACTION, LABEL_SIZE,
    ROW_SPACING, TITLE_SIZE, WIDGET_WIDTH, close_button, scale_widgets,
};

/// Where the Mega Bezel packs are unpacked, relative to the checkout root (or
/// to the executable) -- see the Mega Bezel section of `docs/SHADERS.md` for
/// why a pack has to sit exactly there.
pub const PACKS_DIR: &str = "shaders/Mega_Bezel_Packs";

/// The subdirectory of a pack that holds its preset tree.
const PRESETS: &str = "presets";

/// The directories a shader collection is looked for in: the working directory,
/// which is where the `shaders/` working checkout lives, and next to the
/// executable, for a copy that ships beside the binary. The working directory
/// is the empty path, so what is built on it stays relative -- and so stays
/// copyable into a `--slangp` argument.
fn search_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::new()];
    if let Some(beside) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        roots.push(beside);
    }
    roots
}

// ---------------------------------------------------------------------------
// The tree
// ---------------------------------------------------------------------------

/// How a directory or file name is turned into what the combo box shows.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Labeling {
    /// `Commodore_Amiga500` -> `Commodore Amiga500`. The names are already
    /// capitalised the way the pack's author wrote them.
    Words,
    /// Left exactly as it is, for the flavour codes (`MBZ_SHARP_STD`), which are
    /// initialisms rather than words and are how the pack's README names them.
    Raw,
    /// `NEAR_CURVED` -> `Near Curved`: the shouted halves of a preset's file
    /// name, which are words.
    Title,
}

struct LevelSpec {
    /// Row label in the dialog.
    label: &'static str,
    labeling: Labeling,
}

/// One row of the dialog, top to bottom. The first [`DIR_LEVELS`] are
/// directories under the pack root; the last two are the halves of a preset's
/// file name.
const LEVELS: [LevelSpec; 5] = [
    LevelSpec {
        label: "System",
        labeling: Labeling::Words,
    },
    LevelSpec {
        label: "Monitor",
        labeling: Labeling::Words,
    },
    LevelSpec {
        label: "Shader",
        labeling: Labeling::Raw,
    },
    LevelSpec {
        label: "Type",
        labeling: Labeling::Title,
    },
    LevelSpec {
        label: "Day/Night",
        labeling: Labeling::Title,
    },
];

const DEPTH: usize = LEVELS.len();
/// Levels that name a directory. Below them sit the presets themselves.
const DIR_LEVELS: usize = 3;
/// The `NEAR_CURVED` half of a preset's file name.
const TYPE: usize = 3;
/// The `NIGHT` half.
const LIGHT: usize = 4;

/// One entry of one combo box: the name on disk and what is shown for it.
#[derive(Clone, Debug, PartialEq)]
struct Choice {
    raw: String,
    label: String,
}

/// One combo box: everything it offers and which of those is picked.
#[derive(Clone, Debug, Default)]
struct Level {
    choices: Vec<Choice>,
    index: usize,
}

impl Level {
    /// The name on disk of the current pick, or `None` for a level with nothing
    /// in it (an empty directory, or a preset name with no lighting half).
    fn raw(&self) -> Option<&str> {
        self.choices.get(self.index).map(|c| c.raw.as_str())
    }
}

/// The selection, and the choices each level offers under it.
///
/// Only the levels below the one that changed are re-read, and a level keeps its
/// pick if the new choices still contain it -- so walking through the monitors
/// of one machine stays on the same flavour and preset rather than resetting to
/// the first one each time.
pub struct PresetBrowser {
    root: PathBuf,
    levels: [Level; DEPTH],
    /// The `.slangp` files of the selected flavour directory, split into their
    /// type and lighting halves. Read once per flavour and shared by the two
    /// levels that are drawn from it.
    presets: Vec<(String, String)>,
}

impl PresetBrowser {
    /// Reads the top level of `root` and selects the first of everything.
    ///
    /// `Err` if the pack is not there or holds no presets, which is what the
    /// caller reports rather than opening an empty dialog.
    pub fn new(root: PathBuf) -> Result<Self, String> {
        if !root.is_dir() {
            return Err(format!("No shader pack at {}", root.display()));
        }
        let mut browser = Self {
            root,
            levels: std::array::from_fn(|_| Level::default()),
            presets: Vec::new(),
        };
        browser.rebuild(0);
        if browser.path().is_none() {
            return Err(format!("No presets under {}", browser.root.display()));
        }
        Ok(browser)
    }

    /// Picks `index` at `level` and re-reads everything below it.
    fn select(&mut self, level: usize, index: usize) {
        if level >= DEPTH || index >= self.levels[level].choices.len() {
            return;
        }
        self.levels[level].index = index;
        self.rebuild(level + 1);
    }

    /// The preset the current selection names, or `None` if any level of it came
    /// up empty.
    pub fn path(&self) -> Option<PathBuf> {
        let dir = self.dir(DIR_LEVELS)?;
        let ty = self.levels[TYPE].raw()?;
        // A preset whose name has no lighting half is one whole name already.
        let file = match self.levels[LIGHT].raw() {
            Some(light) => format!("{ty}_{light}.slangp"),
            None => format!("{ty}.slangp"),
        };
        Some(dir.join(file))
    }

    /// The same path with the pack root taken off, which is what the dialog
    /// shows under the combo boxes.
    fn relative_path(&self) -> Option<String> {
        let path = self.path()?;
        let rel = path.strip_prefix(&self.root).unwrap_or(&path);
        Some(rel.to_string_lossy().replace('\\', "/"))
    }

    /// Moves the selection onto `preset`, if it is one of this pack's.
    ///
    /// Used when the dialog opens so it comes up showing what is on screen
    /// rather than the first preset in the pack. Returns whether every level
    /// matched; a path from somewhere else (or one the pack no longer ships)
    /// leaves the selection as it was.
    pub fn reveal(&mut self, preset: &Path) -> bool {
        let Some(rel) = self.strip_root(preset) else {
            return false;
        };
        let parts: Vec<&str> = rel.iter().filter_map(|c| c.to_str()).collect();
        if parts.len() != DIR_LEVELS + 1 {
            return false;
        }
        let Some(stem) = Path::new(parts[DIR_LEVELS])
            .file_stem()
            .and_then(|s| s.to_str())
        else {
            return false;
        };
        let (ty, light) = split_stem(stem);
        let wanted = [parts[0], parts[1], parts[2], ty, light];
        for (level, want) in wanted.iter().enumerate() {
            let Some(index) = self.levels[level]
                .choices
                .iter()
                .position(|c| c.raw == *want)
            else {
                // Everything above matched and is selected; the levels below are
                // whatever that leaves, which is a valid preset either way.
                return false;
            };
            self.select(level, index);
        }
        true
    }

    /// Whether `preset` is one of this tree's, which is what picks the
    /// collection the dialog opens on -- including a path of the pack's shape
    /// naming a preset it no longer ships, which still belongs to this pack
    /// rather than to the default collection.
    pub fn contains(&self, preset: &Path) -> bool {
        self.strip_root(preset).is_some()
    }

    /// `preset` relative to the pack root. Tried as given first, so a browser
    /// built on a relative root still recognises a relative path, and through
    /// `canonicalize` after that, which is what matches an absolute
    /// `--slangp` against a relative root.
    fn strip_root<'a>(&self, preset: &'a Path) -> Option<std::borrow::Cow<'a, Path>> {
        if let Ok(rel) = preset.strip_prefix(&self.root) {
            return Some(rel.into());
        }
        let root = self.root.canonicalize().ok()?;
        let full = preset.canonicalize().ok()?;
        let rel = full.strip_prefix(root).ok()?;
        Some(rel.to_path_buf().into())
    }

    /// Re-reads levels `from..` , each one under what the level above now
    /// selects, keeping a level's pick when the new choices still offer it.
    fn rebuild(&mut self, from: usize) {
        for level in from..DEPTH {
            let previous = self.levels[level].raw().map(str::to_owned);
            let choices = self.choices_at(level);
            let index = previous
                .and_then(|raw| choices.iter().position(|c| c.raw == raw))
                .unwrap_or(0);
            self.levels[level] = Level { choices, index };
        }
    }

    /// What one level offers under the current selection. Reads at most one
    /// directory, and for [`TYPE`] also caches its listing for [`LIGHT`].
    fn choices_at(&mut self, level: usize) -> Vec<Choice> {
        let labeling = LEVELS[level].labeling;
        let raws: Vec<String> = match level {
            0..DIR_LEVELS => self
                .dir(level)
                .map(|dir| subdirectories(&dir))
                .unwrap_or_default(),
            TYPE => {
                self.presets = self
                    .dir(DIR_LEVELS)
                    .map(|dir| presets_in(&dir))
                    .unwrap_or_default();
                distinct(self.presets.iter().map(|(ty, _)| ty))
            }
            // Only the lighting variants of the selected type: `OVERLAY_*` ships
            // day only, so this list is not the same for every type.
            _ => {
                let ty = self.levels[TYPE].raw().unwrap_or_default().to_owned();
                distinct(
                    self.presets
                        .iter()
                        .filter(|(t, light)| *t == ty && !light.is_empty())
                        .map(|(_, light)| light),
                )
            }
        };
        raws.into_iter()
            .map(|raw| Choice {
                label: label(&raw, labeling),
                raw,
            })
            .collect()
    }

    /// The directory the first `level` selections name: `dir(0)` is the pack
    /// root, `dir(3)` the flavour directory holding the presets themselves.
    fn dir(&self, level: usize) -> Option<PathBuf> {
        let mut path = self.root.clone();
        for level in &self.levels[..level] {
            path.push(level.raw()?);
        }
        Some(path)
    }
}

/// Sorted names of the subdirectories of `dir`. An unreadable directory is an
/// empty one -- the dialog then offers nothing at that level, which is the
/// truth as far as it can see.
fn subdirectories(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// The `.slangp` files of `dir`, sorted, each split by [`split_stem`].
fn presets_in(dir: &Path) -> Vec<(String, String)> {
    let mut stems: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "slangp"))
        .filter_map(|p| p.file_stem()?.to_str().map(str::to_owned))
        .collect();
    stems.sort();
    stems
        .iter()
        .map(|stem| {
            let (ty, light) = split_stem(stem);
            (ty.to_owned(), light.to_owned())
        })
        .collect()
}

/// Splits a preset's file name into the part that names the geometry and the
/// part that names the lighting: `NEAR_CURVED_NIGHT` -> `NEAR_CURVED` + `NIGHT`.
///
/// The last underscore is the seam. A name with no underscore has no lighting
/// half, and gets an empty one rather than being dropped.
fn split_stem(stem: &str) -> (&str, &str) {
    stem.rsplit_once('_').unwrap_or((stem, ""))
}

/// The distinct values of `values`, in the order they first appear -- which,
/// fed a sorted listing, is alphabetical.
fn distinct<'a>(values: impl Iterator<Item = &'a String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for value in values {
        if !out.iter().any(|seen| seen == value) {
            out.push(value.clone());
        }
    }
    out
}

fn label(raw: &str, labeling: Labeling) -> String {
    match labeling {
        Labeling::Raw => raw.to_owned(),
        Labeling::Words => raw.replace('_', " "),
        Labeling::Title => {
            let mut out = String::with_capacity(raw.len());
            for (i, word) in raw.split('_').filter(|w| !w.is_empty()).enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                let mut chars = word.chars();
                if let Some(first) = chars.next() {
                    out.extend(first.to_uppercase());
                    out.extend(chars.flat_map(char::to_lowercase));
                }
            }
            out
        }
    }
}

// ---------------------------------------------------------------------------
// Collections
// ---------------------------------------------------------------------------

/// One entry of the dialog's top combo box.
///
/// The first is always the default collection: whatever `--shader` (or the
/// settings dialog) last picked, which is what the app runs when no pack preset
/// is chosen, and the only entry a checkout with no `shaders/` directory has.
/// Every other entry is a directory tree that [`PresetBrowser`] walks.
///
/// Only the Mega Bezel packs are found for now. The other thing under
/// `shaders/` worth offering is the slang-shaders checkout itself, but
/// `shaders_slang` is a pile of category directories with presets sitting at
/// several depths rather than the packs' fixed five levels, so what a "level"
/// would mean there is still TBD. When it is settled it becomes another
/// [`collections`] entry with a browser of its own, and nothing below here
/// changes.
struct Collection {
    /// What the combo box shows.
    label: String,
    /// The tree this collection browses, or `None` for the default collection,
    /// which is one preset and has no levels.
    browser: Option<PresetBrowser>,
}

/// Index of the default collection, which is also what the dialog falls back
/// to.
const DEFAULT: usize = 0;

/// Everything the dialog can offer, the default collection first.
fn collections() -> Vec<Collection> {
    let mut found = vec![Collection {
        label: "Default".to_owned(),
        browser: None,
    }];
    found.extend(bezel_packs());
    found
}

/// The bezel packs under [`PACKS_DIR`], one collection each. A pack whose tree
/// cannot be browsed -- no `presets` directory, or no presets in it -- is left
/// out rather than offered as a row of empty combo boxes, and the same pack
/// found twice (in the working directory and beside the executable) is offered
/// once.
fn bezel_packs() -> Vec<Collection> {
    let mut packs: Vec<Collection> = Vec::new();
    for base in search_roots() {
        let dir = base.join(PACKS_DIR);
        for name in subdirectories(&dir) {
            let label = pack_label(&name);
            if packs.iter().any(|pack| pack.label == label) {
                continue;
            }
            if let Ok(browser) = PresetBrowser::new(dir.join(&name).join(PRESETS)) {
                packs.push(Collection {
                    label,
                    browser: Some(browser),
                });
            }
        }
    }
    packs
}

/// A pack directory is named `<author>-<machines>`, and it is the machines the
/// dialog is naming: `TheNamec-Commodore` -> `Commodore`. A name with no author
/// half is already the name.
fn pack_label(dir: &str) -> String {
    let name = dir.rsplit_once('-').map_or(dir, |(_, name)| name);
    name.replace('_', " ")
}

// ---------------------------------------------------------------------------
// The dialog
// ---------------------------------------------------------------------------

/// Opens the shader dialog (RightAlt+Shift+E, [`crate::commands::Cmd`]).
#[derive(Message)]
pub struct ShowShaderDialog;

/// The dialog, the collections it found and which of them is selected. All of
/// it outlives a close, so reopening comes up where it was left.
#[derive(Resource, Default)]
pub struct ShaderDialog {
    open: bool,
    /// Filled on the first open: the trees are on disk, and reading them once
    /// per session is enough.
    collections: Vec<Collection>,
    /// Index into `collections`; [`DEFAULT`] until a pack is picked.
    selected: usize,
}

impl ShaderDialog {
    /// The selected collection's tree, or `None` on the default collection --
    /// which is what greys the level rows out.
    fn browser(&self) -> Option<&PresetBrowser> {
        self.collections.get(self.selected)?.browser.as_ref()
    }

    /// Moves the selection onto the collection holding `preset`, and onto the
    /// preset itself within it. A preset from no collection -- the built-in
    /// Lottes, or a `--slangp` from somewhere else -- selects the default.
    fn reveal(&mut self, preset: &Path) {
        for (index, collection) in self.collections.iter_mut().enumerate() {
            if let Some(browser) = collection.browser.as_mut()
                && browser.contains(preset)
            {
                browser.reveal(preset);
                self.selected = index;
                return;
            }
        }
        self.selected = DEFAULT;
    }
}

/// What a frame of the dialog picked, applied once the panel is no longer
/// borrowing the dialog.
enum Picked {
    /// A row of the top combo box.
    Collection(usize),
    /// `(level, index)` of one of the level combo boxes below it.
    Level(usize, usize),
}

pub struct ShaderDialogPlugin;

impl Plugin for ShaderDialogPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ShaderDialog>()
            .add_message::<ShowShaderDialog>()
            .add_systems(Update, open_dialog.run_if(on_message::<ShowShaderDialog>))
            // After `update_ui`, which sets the frame's `pixels_per_point` --
            // the same ordering the settings dialog needs.
            .add_systems(EguiPrimaryContextPass, shader_dialog_ui.after(update_ui));
    }
}

fn open_dialog(
    mut reader: MessageReader<ShowShaderDialog>,
    mut dialog: ResMut<ShaderDialog>,
    mut hud_state: ResMut<HudState>,
    shader: Res<ShaderPath>,
) {
    // One open however many asked for it this frame.
    if reader.read().count() == 0 {
        return;
    }
    if dialog.collections.is_empty() {
        dialog.collections = collections();
    }
    // Come up showing what is on screen: the collection the preset belongs to,
    // with every level of it selected, or the default collection for anything
    // else (which is what the default collection means).
    match &shader.effect {
        ShaderEffect::Slangp(path) => dialog.reveal(path),
        ShaderEffect::Wgsl(_) => dialog.selected = DEFAULT,
    }
    if !dialog.open {
        hud_state.set_settings_open(true);
    }
    dialog.open = true;
}

fn shader_dialog_ui(
    mut contexts: EguiContexts,
    mut dialog: ResMut<ShaderDialog>,
    mut hud: ResMut<HudState>,
    keys: Res<ButtonInput<KeyCode>>,
    mut shader_path: ResMut<ShaderPath>,
    mut render: ResMut<RenderSettings>,
    settings: Res<DemarcSettings>,
) -> Result {
    if !dialog.open {
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?;

    // Same reason as the picker and the settings dialog: egui only learns of a
    // modifier through the key events Bevy feeds it, so its own idea of what is
    // held goes stale.
    let mods = live_modifiers(&keys);
    let mut closing = ctx.input_mut(|i| {
        sync_modifiers(i, mods);
        take_key(i, egui::Key::Escape) > 0
    });
    // Picked inside the closure and applied after it, because the dialog is
    // borrowed for as long as the panel is being drawn.
    let mut picked = None;
    let composed = composed_path(&dialog, settings.shader);

    egui::Area::new(egui::Id::new("shader_dialog"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            panel_frame().show(ui, |ui| {
                scale_widgets(ui);
                let panel = ui
                    .vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Shader").size(TITLE_SIZE).strong());
                            // Room for the close button, placed in this row's
                            // right corner once the width is known.
                            ui.add_space(CLOSE_SIZE);
                        });
                        ui.add_space(ROW_SPACING.y);
                        let max_height = ctx.content_rect().height() * GRID_HEIGHT_FRACTION;
                        egui::ScrollArea::vertical()
                            .max_height(max_height)
                            .auto_shrink([true, true])
                            .show(ui, |ui| picked = dialog_body(ui, &dialog));
                        // Under the scroll area rather than in it, so a long
                        // grid scrolls without taking the line with it.
                        ui.add_space(ROW_SPACING.y);
                        ui.label(
                            egui::RichText::new(&composed)
                                .size(BODY_SIZE * 0.75)
                                .color(DISABLED_COLOR),
                        );
                    })
                    .response
                    .rect;
                closing |= close_button(ui, panel);
            });
        });

    match picked {
        Some(Picked::Collection(index)) => {
            dialog.selected = index;
            apply(&dialog, &mut shader_path, &mut render, settings.shader);
        }
        Some(Picked::Level(level, index)) => {
            let selected = dialog.selected;
            if let Some(collection) = dialog.collections.get_mut(selected)
                && let Some(browser) = collection.browser.as_mut()
            {
                browser.select(level, index);
            }
            apply(&dialog, &mut shader_path, &mut render, settings.shader);
        }
        None => {}
    }
    if closing {
        dialog.open = false;
        hud.set_settings_open(false);
    }
    Ok(())
}

/// Puts the selection on screen. A pack preset is a filter chain to run;
/// the default collection is whatever shader the command line or the settings
/// dialog last chose, switched on the way `crate::settings::apply_settings`
/// does for `--shader`.
fn apply(
    dialog: &ShaderDialog,
    shader_path: &mut ShaderPath,
    render: &mut RenderSettings,
    default: ShaderArg,
) {
    match dialog.browser().and_then(PresetBrowser::path) {
        Some(path) => {
            shader_path.effect = ShaderEffect::Slangp(path);
            // Picking a preset is asking to see it, so switch the effect on.
            render.crt_effect = true;
        }
        None => {
            shader_path.effect = default.effect();
            render.crt_effect = default != ShaderArg::None;
        }
    }
}

/// What the dialog prints under the combo boxes: the preset the selection
/// names, relative to its collection, which is also the tail of a `--slangp`
/// argument.
fn composed_path(dialog: &ShaderDialog, default: ShaderArg) -> String {
    match dialog.browser() {
        Some(browser) => browser.relative_path().unwrap_or_default(),
        // `--shader none` is the stock passthrough preset with the effect
        // switched off, so name what it does rather than what it does it with.
        None if default == ShaderArg::None => "no effect".to_owned(),
        None => default.path().to_owned(),
    }
}

/// The collection combo box and one combo box per level under it. Returns what
/// was picked this frame, if anything.
fn dialog_body(ui: &mut Ui, dialog: &ShaderDialog) -> Option<Picked> {
    let mut picked = None;
    let browser = dialog.browser();
    egui::Grid::new("shader_grid")
        .num_columns(2)
        .spacing(ROW_SPACING)
        .show(ui, |ui| {
            let labels: Vec<&str> = dialog
                .collections
                .iter()
                .map(|c| c.label.as_str())
                .collect();
            if let Some(index) = row(ui, "Collection", true, |ui| {
                combo(ui, "Collection", &labels, dialog.selected)
            }) {
                picked = Some(Picked::Collection(index));
            }
            // The levels of the selected collection, or -- on the default
            // collection, which has none -- greyed-out rows in their place, so
            // the dialog keeps its shape as the top box is switched.
            for (level, spec) in LEVELS.iter().enumerate() {
                let state = browser.map(|browser| &browser.levels[level]);
                if let Some(index) = row(ui, spec.label, browser.is_some(), |ui| {
                    draw_level(ui, level, state)
                }) {
                    picked = Some(Picked::Level(level, index));
                }
            }
        });
    picked
}

/// One row of the grid: its label on the left, whatever `widget` draws on the
/// right, the pair greyed out and unclickable when `enabled` is false.
fn row<R>(ui: &mut Ui, label: &str, enabled: bool, widget: impl FnOnce(&mut Ui) -> R) -> R {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let text = egui::RichText::new(label).size(LABEL_SIZE);
        ui.label(if enabled {
            text
        } else {
            text.color(DISABLED_COLOR)
        });
    });
    let inner = ui
        .scope(|ui| {
            ui.set_min_width(WIDGET_WIDTH);
            ui.add_enabled_ui(enabled, widget).inner
        })
        .inner;
    ui.end_row();
    inner
}

/// One level's combo box, or a dash for a level with nothing to offer: the
/// default collection, a preset name with no lighting half, or a directory the
/// pack left empty.
fn draw_level(ui: &mut Ui, level: usize, state: Option<&Level>) -> Option<usize> {
    let Some(state) = state.filter(|state| state.raw().is_some()) else {
        ui.label(
            egui::RichText::new("—")
                .size(BODY_SIZE)
                .color(DISABLED_COLOR),
        );
        return None;
    };
    let labels: Vec<&str> = state.choices.iter().map(|c| c.label.as_str()).collect();
    combo(ui, LEVELS[level].label, &labels, state.index)
}

/// One combo box, showing `selected` of `labels`. Returns the index picked this
/// frame, if any.
fn combo(ui: &mut Ui, id_salt: &str, labels: &[&str], selected: usize) -> Option<usize> {
    let mut picked = None;
    let current = labels.get(selected).copied().unwrap_or_default();
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(egui::RichText::new(current).size(BODY_SIZE))
        .width(WIDGET_WIDTH)
        .show_ui(ui, |ui| {
            for (index, label) in labels.iter().enumerate() {
                if ui
                    .selectable_label(
                        index == selected,
                        egui::RichText::new(*label).size(BODY_SIZE),
                    )
                    .clicked()
                {
                    picked = Some(index);
                }
            }
        });
    picked
}

#[cfg(test)]
#[path = "tests/shader_dialog_tests.rs"]
mod tests;
