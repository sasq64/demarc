//! The shader dialog: a Mega Bezel preset picked one directory level at a time.
//!
//! The bezel packs are not a handful of shaders but a directory tree of tens of
//! thousands of `.slangp` presets, laid out
//! `<machine>/<monitor>/<flavour>/<scaling>_<curvature>_<lighting>.slangp`
//! (see `docs/SHADERS.md`). That is far too many for the fuzzy list and not
//! something [`crate::settings`] can draw either -- its combo boxes come from a
//! reflected enum's variant list, and these choices are only known once a
//! directory has been read. So this dialog draws one combo box per level and
//! fills each from what the level above selected:
//!
//! ```text
//! Commodore_Amiga500 / Commodore_C1084 / MBZ_SHARP_STD / NEAR_CURVED_NIGHT.slangp
//!   System             Monitor           Shader          Type       Day/Night
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

use crate::config::RenderSettings;
use crate::egui_ui::{
    HudState, SetHudText, live_modifiers, panel_frame, sync_modifiers, take_key, update_ui,
};
use crate::post_process::{ShaderEffect, ShaderPath};
// The dialog chrome -- panel metrics, the widget scaling and the close button --
// is the settings dialog's, so the two look like one dialog with two contents.
use crate::settings::{
    BODY_SIZE, CLOSE_SIZE, DISABLED_COLOR, GRID_HEIGHT_FRACTION, LABEL_SIZE, ROW_SPACING,
    TITLE_SIZE, WIDGET_WIDTH, close_button, scale_widgets,
};

/// The preset directory the dialog browses, relative to the checkout root (or
/// to the executable) -- see the Mega Bezel section of `docs/SHADERS.md` for why
/// the pack has to sit exactly there.
pub const PACK_PRESETS: &str = "shaders/Mega_Bezel_Packs/TheNamec-Commodore/presets";

/// Locate [`PACK_PRESETS`]: next to the working directory, which is where the
/// `shaders/` working checkout lives, or next to the executable for a copy that
/// ships beside the binary.
fn preset_root() -> Option<PathBuf> {
    let local = PathBuf::from(PACK_PRESETS);
    if local.is_dir() {
        return Some(local);
    }
    let beside = std::env::current_exe().ok()?.parent()?.join(PACK_PRESETS);
    beside.is_dir().then_some(beside)
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
// The dialog
// ---------------------------------------------------------------------------

/// Opens the shader dialog (RightAlt+Shift+E, [`crate::commands::Cmd`]).
#[derive(Message)]
pub struct ShowShaderDialog;

/// The dialog, and the tree it is browsing. The browser outlives a close, so
/// reopening comes up where it was left.
#[derive(Resource, Default)]
pub struct ShaderDialog {
    open: bool,
    browser: Option<PresetBrowser>,
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
    mut hud: MessageWriter<SetHudText>,
    shader: Res<ShaderPath>,
) {
    // One open however many asked for it this frame.
    if reader.read().count() == 0 {
        return;
    }
    if dialog.browser.is_none() {
        let found = preset_root().ok_or_else(|| format!("No shader pack at {PACK_PRESETS}"));
        match found.and_then(PresetBrowser::new) {
            Ok(browser) => dialog.browser = Some(browser),
            Err(err) => {
                // Nothing to draw, so say why rather than opening an empty panel.
                hud.write(SetHudText {
                    text: err,
                    duration: std::time::Duration::from_secs(4),
                    ..default()
                });
                return;
            }
        }
    }
    // Come up showing what is on screen, when that is one of the pack's.
    if let (Some(browser), ShaderEffect::Slangp(path)) = (dialog.browser.as_mut(), &shader.effect) {
        browser.reveal(path);
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
    // Picked inside the closure and applied after it, because the browser is
    // borrowed for as long as the panel is being drawn.
    let mut picked = None;

    if let Some(browser) = &dialog.browser {
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
                                .show(ui, |ui| picked = browser_body(ui, browser));
                        })
                        .response
                        .rect;
                    closing |= close_button(ui, panel);
                });
            });
    }

    if let Some((level, index)) = picked
        && let Some(browser) = dialog.browser.as_mut()
    {
        browser.select(level, index);
        if let Some(path) = browser.path() {
            shader_path.effect = ShaderEffect::Slangp(path);
            // Picking a preset is asking to see it, so switch the effect on the
            // way `crate::settings::apply_settings` does for `--shader`.
            render.crt_effect = true;
        }
    }
    if closing {
        dialog.open = false;
        hud.set_settings_open(false);
    }
    Ok(())
}

/// One combo box per level, and the preset they compose underneath. Returns the
/// `(level, index)` picked this frame, if any.
fn browser_body(ui: &mut Ui, browser: &PresetBrowser) -> Option<(usize, usize)> {
    let mut picked = None;
    egui::Grid::new("shader_grid")
        .num_columns(2)
        .spacing(ROW_SPACING)
        .show(ui, |ui| {
            for (level, spec) in LEVELS.iter().enumerate() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(spec.label).size(LABEL_SIZE));
                });
                ui.scope(|ui| {
                    ui.set_min_width(WIDGET_WIDTH);
                    if let Some(index) = draw_level(ui, level, &browser.levels[level]) {
                        picked = Some((level, index));
                    }
                });
                ui.end_row();
            }
        });
    // The preset itself, so what the five boxes add up to is visible -- and
    // copyable into a `--slangp` on the command line.
    if let Some(path) = browser.relative_path() {
        ui.add_space(ROW_SPACING.y);
        ui.label(
            egui::RichText::new(path)
                .size(BODY_SIZE * 0.75)
                .color(DISABLED_COLOR),
        );
    }
    picked
}

/// One level's combo box, or a dash for a level with nothing to offer (a preset
/// name with no lighting half, or a directory the pack left empty).
fn draw_level(ui: &mut Ui, level: usize, state: &Level) -> Option<usize> {
    let Some(current) = state.choices.get(state.index) else {
        ui.label(
            egui::RichText::new("—")
                .size(BODY_SIZE)
                .color(DISABLED_COLOR),
        );
        return None;
    };
    let mut picked = None;
    egui::ComboBox::from_id_salt(LEVELS[level].label)
        .selected_text(egui::RichText::new(&current.label).size(BODY_SIZE))
        .width(WIDGET_WIDTH)
        .show_ui(ui, |ui| {
            for (index, choice) in state.choices.iter().enumerate() {
                let selected = index == state.index;
                if ui
                    .selectable_label(selected, egui::RichText::new(&choice.label).size(BODY_SIZE))
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
