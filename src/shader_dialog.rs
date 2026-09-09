//! The shader dialog: the post-process shader picked from a *collection* combo
//! box and, under it, one combo box per wildcard of that collection's pattern.
//!
//! The collections are described by `shaders/shaders.toml`, where each table is
//! one collection and its `pattern` says both where that collection's presets
//! are and how their paths are read:
//!
//! ```toml
//! [Commodore]
//! pattern = "Mega_Bezel_Packs/TheNamec-Commodore/presets/<System>/<Monitor>/<Shader>/<Type>_<Time>.slangp"
//! [Handheld]
//! pattern = "shaders_slang/handheld/console-border/<Type>.slangp"
//! ```
//!
//! Every `<Tag>` is a wildcard and becomes one combo box, named after the tag,
//! in the order the tags appear; it offers the strings that matched at that
//! position, and picking one re-fills the boxes below it:
//!
//! ```text
//! Commodore / Commodore_Amiga500 / Commodore_C1084 / MBZ_SHARP_STD / NEAR_CURVED_NIGHT.slangp
//! Collection  System              Monitor           Shader          Type_Time
//! ```
//!
//! A wildcard never crosses `/`, and takes as little as it can except when it
//! is the last one of a path component -- so `MBZ__<Level>__<Type>.slangp`
//! reads `MBZ__0__SMOOTH-ADV__GDV.slangp` as `0` and `SMOOTH-ADV__GDV`.
//!
//! The first collection is always `Default`: whatever shader `--shader` names,
//! which is one preset with nothing to browse, so it has no rows under it.
//!
//! Under the combo boxes, folded away, come the selected preset's own
//! `#pragma parameter` declarations, one editor each ([`preset_params`]),
//! drawn with the settings dialog's widgets and written straight to the filter
//! chain. A description ending in `A | B | C` that covers the parameter's whole
//! range is drawn as a combo box instead ([`split_options`]).
//!
//! Like the settings dialog, a pick takes effect the moment it is made: the
//! selection is composed back into a path and written straight to
//! [`ShaderPath`], which the render world extracts.
//!
//! The tree is walked lazily, one `read_dir` per path component as the boxes
//! above it change, because the Commodore pack alone holds ~72k presets and
//! only a few dozen names are ever on screen. [`PresetBrowser`] is the whole of
//! that logic and knows nothing about egui; the tests exercise it against a
//! tree they build.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::prelude::*;
use bevy_egui::{
    EguiContexts, EguiPrimaryContextPass,
    egui::{self, Ui},
};
use regex::Regex;
use tracing::warn;

use crate::config::{Args, RenderSettings, ShaderArg};
use crate::egui_ui::{HudState, live_modifiers, panel_frame, sync_modifiers, take_key, update_ui};
use crate::post_process::{ShaderEffect, ShaderPath};
// The dialog chrome -- panel metrics, the widget scaling and the close button --
// is the settings dialog's, so the two look like one dialog with two contents.
use crate::egui_settings::{
    BODY_SIZE, CLOSE_SIZE, DISABLED_COLOR, GRID_HEIGHT_FRACTION, LABEL_SIZE, ROW_SPACING,
    TITLE_SIZE, WIDGET_WIDTH, close_button, draw_number, scale_widgets,
};

/// The file the collections are read from, relative to a search root. Its own
/// directory is what every pattern in it is relative to.
pub const CONFIG_PATH: &str = "shaders/shaders.toml";

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
// Patterns
// ---------------------------------------------------------------------------

/// One `/`-separated component of a pattern: literal text with `<Tag>` holes in
/// it. `literals` is one longer than `tags`, and holds the text around them, so
/// a name can be taken apart by [`Segment::captures`] and put back together by
/// [`Segment::compose`].
struct Segment {
    literals: Vec<String>,
    tags: Vec<String>,
    regex: Regex,
}

impl Segment {
    fn parse(text: &str) -> Result<Self, String> {
        let mut literals = Vec::new();
        let mut tags = Vec::new();
        let mut rest = text;
        while let Some(open) = rest.find('<') {
            let Some(close) = rest[open..].find('>').map(|i| i + open) else {
                return Err(format!("unclosed <> in {text:?}"));
            };
            literals.push(rest[..open].to_owned());
            tags.push(rest[open + 1..close].to_owned());
            rest = &rest[close + 1..];
        }
        literals.push(rest.to_owned());

        let mut pattern = String::from("^");
        for (i, literal) in literals.iter().enumerate() {
            pattern.push_str(&regex::escape(literal));
            if i < tags.len() {
                // Lazy but for the last one, which takes whatever is left over.
                pattern.push_str(if i + 1 == tags.len() { "(.+)" } else { "(.+?)" });
            }
        }
        pattern.push('$');
        let regex = Regex::new(&pattern).map_err(|err| err.to_string())?;
        Ok(Self {
            literals,
            tags,
            regex,
        })
    }

    /// The wildcard values of `name`, or `None` if it is not this segment's
    /// shape.
    fn captures(&self, name: &str) -> Option<Vec<String>> {
        let caps = self.regex.captures(name)?;
        Some(
            caps.iter()
                .skip(1)
                .map(|m| m.map_or(String::new(), |m| m.as_str().to_owned()))
                .collect(),
        )
    }

    /// The segment with `values` put back into its holes.
    fn compose(&self, values: &[String]) -> String {
        let mut out = self.literals[0].clone();
        for (value, literal) in values.iter().zip(&self.literals[1..]) {
            out.push_str(value);
            out.push_str(literal);
        }
        out
    }
}

// ---------------------------------------------------------------------------
// The tree
// ---------------------------------------------------------------------------

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
    /// in it (a directory holding no preset of this pattern's shape).
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
    segments: Vec<Segment>,
    /// Index of each segment's first level; a segment holds one level per tag,
    /// and the levels are the tags of every segment in order.
    starts: Vec<usize>,
    /// Row labels: the tag names, flattened the same way.
    names: Vec<String>,
    levels: Vec<Level>,
}

impl PresetBrowser {
    /// Reads the top level of `root` and selects the first of everything.
    ///
    /// `Err` if the pattern is malformed, or if nothing on disk matches it,
    /// which is what leaves the collection out rather than offering a row of
    /// empty combo boxes.
    pub fn new(root: PathBuf, pattern: &str) -> Result<Self, String> {
        let segments = pattern
            .split('/')
            .map(Segment::parse)
            .collect::<Result<Vec<_>, _>>()?;
        let mut starts = Vec::with_capacity(segments.len());
        let mut names: Vec<String> = Vec::new();
        for segment in &segments {
            starts.push(names.len());
            names.extend(segment.tags.iter().cloned());
        }
        let levels = vec![Level::default(); names.len()];
        let mut browser = Self {
            root,
            segments,
            starts,
            names,
            levels,
        };
        browser.rebuild(0);
        match browser.path() {
            Some(path) if path.exists() => Ok(browser),
            _ => Err(format!("No presets matching {pattern}")),
        }
    }

    /// Picks `index` at `level` and re-reads everything below it.
    fn select(&mut self, level: usize, index: usize) {
        if level >= self.levels.len() || index >= self.levels[level].choices.len() {
            return;
        }
        self.levels[level].index = index;
        self.rebuild(level + 1);
    }

    /// The preset the current selection names, or `None` if any level of it came
    /// up empty.
    pub fn path(&self) -> Option<PathBuf> {
        self.dir(self.segments.len())
    }

    /// The same path with the collection root taken off, which is what the
    /// dialog shows under the combo boxes.
    fn relative_path(&self) -> Option<String> {
        let path = self.path()?;
        let rel = path.strip_prefix(&self.root).unwrap_or(&path);
        Some(rel.to_string_lossy().replace('\\', "/"))
    }

    /// Moves the selection onto `preset`, if it is one of this collection's.
    ///
    /// Used when the dialog opens so it comes up showing what is on screen
    /// rather than the first preset of the collection. Returns whether every
    /// level matched; a path from somewhere else (or one the collection no
    /// longer ships) leaves the selection as it was.
    pub fn reveal(&mut self, preset: &Path) -> bool {
        let Some(wanted) = self.match_path(preset) else {
            return false;
        };
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

    /// Whether `preset` has this collection's shape, which is what picks the
    /// collection the dialog opens on -- including a path of the right shape
    /// naming a preset the collection no longer ships, which still belongs here
    /// rather than to the default collection.
    pub fn contains(&self, preset: &Path) -> bool {
        self.match_path(preset).is_some()
    }

    /// The wildcard values `preset` has, level by level, or `None` if it is not
    /// under the root or not the pattern's shape.
    fn match_path(&self, preset: &Path) -> Option<Vec<String>> {
        let rel = self.strip_root(preset)?;
        let parts: Vec<&str> = rel.iter().filter_map(|c| c.to_str()).collect();
        if parts.len() != self.segments.len() {
            return None;
        }
        let mut values = Vec::with_capacity(self.levels.len());
        for (segment, part) in self.segments.iter().zip(parts) {
            values.extend(segment.captures(part)?);
        }
        Some(values)
    }

    /// `preset` relative to the collection root. Tried as given first, so a
    /// browser built on a relative root still recognises a relative path, and
    /// through `canonicalize` after that, which is what matches an absolute
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

    /// Re-reads every level from `from` down, each one under what the levels
    /// above it now select, keeping a level's pick when the new choices still
    /// offer it.
    fn rebuild(&mut self, from: usize) {
        if from >= self.levels.len() {
            return;
        }
        for segment in self.segment_of(from)..self.segments.len() {
            let start = self.starts[segment];
            let tags = self.segments[segment].tags.len();
            if tags == 0 {
                continue;
            }
            // One listing for the whole segment, shared by its tags: a name with
            // two wildcards in it is read once, not once per box.
            let rows = self.entries(segment);
            let file = segment + 1 == self.segments.len();
            for group in 0..tags {
                let level = start + group;
                let picked: Vec<String> = (0..group)
                    .map(|i| self.levels[start + i].raw().unwrap_or_default().to_owned())
                    .collect();
                let mut raws: Vec<String> = Vec::new();
                for row in rows.iter().filter(|row| row[..group] == picked[..]) {
                    if !raws.contains(&row[group]) {
                        raws.push(row[group].clone());
                    }
                }
                let previous = self.levels[level].raw().map(str::to_owned);
                let choices: Vec<Choice> = raws
                    .into_iter()
                    .map(|raw| Choice {
                        label: label(&raw, file),
                        raw,
                    })
                    .collect();
                let index = previous
                    .and_then(|raw| choices.iter().position(|c| c.raw == raw))
                    .unwrap_or(0);
                self.levels[level] = Level { choices, index };
            }
        }
    }

    /// The wildcard values of everything in `segment`'s directory that has its
    /// shape, sorted by name. An unreadable directory is an empty one -- the
    /// dialog then offers nothing at that level, which is the truth as far as it
    /// can see.
    fn entries(&self, segment: usize) -> Vec<Vec<String>> {
        let Some(dir) = self.dir(segment) else {
            return Vec::new();
        };
        let file = segment + 1 == self.segments.len();
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir() != file)
            .filter_map(|e| e.file_name().into_string().ok())
            // A checkout's `.git` sorts first and holds no presets.
            .filter(|name| !name.starts_with('.'))
            .collect();
        names.sort();
        names
            .iter()
            .filter_map(|name| self.segments[segment].captures(name))
            .collect()
    }

    /// Which segment `level` belongs to.
    fn segment_of(&self, level: usize) -> usize {
        self.starts
            .iter()
            .rposition(|start| *start <= level)
            .unwrap_or(0)
    }

    /// One path component, the selected values put back into it.
    fn component(&self, segment: usize) -> Option<String> {
        let start = self.starts[segment];
        let values: Option<Vec<String>> = (0..self.segments[segment].tags.len())
            .map(|i| self.levels[start + i].raw().map(str::to_owned))
            .collect();
        Some(self.segments[segment].compose(&values?))
    }

    /// The path the first `count` components name: `dir(0)` is the collection
    /// root, `dir(segments.len())` the preset itself.
    fn dir(&self, count: usize) -> Option<PathBuf> {
        let mut path = self.root.clone();
        for segment in 0..count {
            path.push(self.component(segment)?);
        }
        Some(path)
    }
}

/// What a combo box shows for a matched string.
///
/// A shouted name from a file (`NEAR_CURVED`, `NIGHT`) is the pack's way of
/// writing words, and reads as words; a shouted directory name
/// (`MBZ_SHARP_STD`) is a code, and is how its README names it, so it is left
/// alone. Anything with lowercase in it is already written the way its author
/// meant it, bar the underscores a directory name uses for spaces.
fn label(raw: &str, file: bool) -> String {
    let shouted = !raw.chars().any(char::is_lowercase);
    match (file, shouted) {
        (true, true) => title(raw),
        (false, false) => raw.replace('_', " "),
        _ => raw.to_owned(),
    }
}

/// `NEAR_CURVED` -> `Near Curved`.
fn title(raw: &str) -> String {
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

// ---------------------------------------------------------------------------
// Collections
// ---------------------------------------------------------------------------

/// One entry of the dialog's top combo box.
///
/// The first is always the default collection: whatever `--shader` names, which
/// is what the app runs when no preset is chosen, and the only entry a checkout
/// with no `shaders/shaders.toml` has. Every other entry is one table of that
/// file, browsed by a [`PresetBrowser`] over its pattern.
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
    if let Some((root, text)) = read_config() {
        found.extend(parse_collections(&root, &text));
    }
    found
}

/// The first [`CONFIG_PATH`] found under the search roots, with the directory
/// its patterns are relative to.
fn read_config() -> Option<(PathBuf, String)> {
    for base in search_roots() {
        let path = base.join(CONFIG_PATH);
        if let Ok(text) = std::fs::read_to_string(&path) {
            let dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
            return Some((dir, text));
        }
    }
    None
}

/// One collection per table of the config, in the order it writes them. A
/// pattern nothing on disk matches is left out.
fn parse_collections(root: &Path, text: &str) -> Vec<Collection> {
    let table: toml::Table = match text.parse() {
        Ok(table) => table,
        Err(err) => {
            warn!("{CONFIG_PATH}: {err}");
            return Vec::new();
        }
    };
    table
        .iter()
        .filter_map(|(label, entry)| {
            let pattern = entry.get("pattern")?.as_str()?;
            match PresetBrowser::new(root.to_path_buf(), pattern) {
                Ok(browser) => Some(Collection {
                    label: label.clone(),
                    browser: Some(browser),
                }),
                Err(err) => {
                    warn!("shader collection {label}: {err}");
                    None
                }
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Preset parameters
// ---------------------------------------------------------------------------

/// One `#pragma parameter` of the selected preset, as the shader declares it
/// and with whatever value is in force.
struct ShaderParam {
    /// The uniform name, which is what the filter chain is set by.
    name: String,
    /// The description the pragma gives, which is what the row is labelled
    /// with, with any option list taken off it.
    label: String,
    /// The choices the description listed, empty for a plain number.
    options: Vec<String>,
    value: f32,
    /// What "Reset" puts the parameter back to.
    default: f32,
    min: f32,
    max: f32,
    step: f32,
}

/// The option list a description ends in, if the parameter is a choice of
/// exactly those: `"Compare Area:  LEFT | RIGHT | TOP | BOTTOM"` over `0..3`
/// step `1` is four options, and reads as a combo box rather than a number.
/// Returns the description without the list, and the options.
fn split_options(description: &str, min: f32, max: f32, step: f32) -> (String, Vec<String>) {
    let plain = || (description.to_owned(), Vec::new());
    let text = description.trim_end();
    let Some(colon) = text.rfind(':') else {
        return plain();
    };
    let list = &text[colon + 1..];
    if !list.contains('|') {
        return plain();
    }
    let options: Vec<String> = list.split('|').map(|o| o.trim().to_owned()).collect();
    if options.iter().any(String::is_empty) {
        return plain();
    }
    // Only when the options are exactly the values the parameter can take.
    let whole = step >= 1.0 && step.fract() == 0.0;
    if !whole || (max - min) / step + 1.0 != options.len() as f32 {
        return plain();
    }
    (text[..colon].to_owned(), options)
}

/// Every parameter the preset's passes declare, deduplicated (a parameter
/// shared by several passes is one row) and ordered by pass, then by label.
///
/// The values are the ones the chain starts with: the shader's initial value,
/// overridden by the preset's own `#parameter` lines -- the same precedence
/// librashader's `RuntimeParameters` applies when it builds the chain.
fn preset_params(path: &Path) -> Vec<ShaderParam> {
    use librashader::preprocess::ShaderSource;
    use librashader::presets::{ShaderFeatures, ShaderPreset};

    let preset = match ShaderPreset::try_parse(path, ShaderFeatures::NONE) {
        Ok(preset) => preset,
        Err(err) => {
            warn!("{}: {err}", path.display());
            return Vec::new();
        }
    };
    let mut params: Vec<ShaderParam> = Vec::new();
    for pass in &preset.passes {
        let Ok(source) = ShaderSource::load(&pass.path, preset.features) else {
            continue;
        };
        let mut declared: Vec<ShaderParam> = source
            .parameters
            .values()
            // A pragma with a blank description is one of the spacers the Mega
            // Bezel packs lay their RetroArch menu out with; there is nothing
            // to label a row with.
            .filter(|p| !p.description.trim().is_empty())
            .filter(|p| !params.iter().any(|old| old.name == p.id.as_ref()))
            .map(|p| {
                let (label, options) = split_options(&p.description, p.minimum, p.maximum, p.step);
                ShaderParam {
                    name: p.id.to_string(),
                    label,
                    options,
                    value: p.initial,
                    default: p.initial,
                    min: p.minimum,
                    max: p.maximum,
                    step: p.step,
                }
            })
            .collect();
        // The source hands them over in a hash map, so a pass's parameters have
        // no order of their own to keep.
        declared.sort_by(|a, b| a.label.cmp(&b.label));
        params.extend(declared);
    }
    for over in &preset.parameters {
        if let Some(param) = params.iter_mut().find(|p| p.name == over.name.as_ref()) {
            param.value = over.value;
            param.default = over.value;
        }
    }
    params
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
    /// Index into `collections`; [`DEFAULT`] until another is picked.
    selected: usize,
    /// The parameters of the preset now on screen, one row each under the
    /// combo boxes.
    params: Vec<ShaderParam>,
    /// The preset `params` was read from, so it is re-read once per preset
    /// rather than once per frame.
    params_for: Option<PathBuf>,
}

impl ShaderDialog {
    /// The selected collection's tree, or `None` on the default collection --
    /// which is what leaves it with no level rows.
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
    /// `(parameter, value)` of one of the preset's parameter editors.
    Param(usize, f32),
    /// The "Reset" button: every parameter back to the preset's own value.
    Reset,
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
    args: Res<Args>,
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
    let default = args.shader.unwrap_or_default();
    let composed = composed_path(&dialog, default);
    refresh_params(&mut dialog, &shader_path);

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
            apply(&dialog, &mut shader_path, &mut render, default);
            refresh_params(&mut dialog, &shader_path);
        }
        Some(Picked::Level(level, index)) => {
            let selected = dialog.selected;
            if let Some(collection) = dialog.collections.get_mut(selected)
                && let Some(browser) = collection.browser.as_mut()
            {
                browser.select(level, index);
            }
            apply(&dialog, &mut shader_path, &mut render, default);
            refresh_params(&mut dialog, &shader_path);
        }
        Some(Picked::Param(index, value)) => {
            if let Some(param) = dialog.params.get_mut(index) {
                param.value = value;
                Arc::make_mut(&mut shader_path.params).insert(param.name.clone(), value);
            }
        }
        Some(Picked::Reset) => {
            for param in &mut dialog.params {
                param.value = param.default;
            }
            shader_path.params = Arc::new(HashMap::new());
        }
        None => {}
    }
    if closing {
        dialog.open = false;
        hud.set_settings_open(false);
    }
    Ok(())
}

/// Puts the selection on screen. A collection's preset is a filter chain to run;
/// the default collection is whatever shader the command line chose, with the
/// effect switched on unless that is `--shader none`.
fn apply(
    dialog: &ShaderDialog,
    shader_path: &mut ShaderPath,
    render: &mut RenderSettings,
    default: ShaderArg,
) {
    // The overrides named the old preset's parameters.
    shader_path.params = Arc::new(HashMap::new());
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

/// Re-reads the parameter rows when the preset on screen has changed, which is
/// a preset pick and (once) the open. Reading them means parsing the preset and
/// preprocessing every pass it names, so it is kept off the per-frame path.
fn refresh_params(dialog: &mut ShaderDialog, shader_path: &ShaderPath) {
    let preset = match &shader_path.effect {
        ShaderEffect::Slangp(path) => Some(path.clone()),
        ShaderEffect::Wgsl(_) => None,
    };
    if preset == dialog.params_for {
        return;
    }
    dialog.params = preset.as_deref().map(preset_params).unwrap_or_default();
    dialog.params_for = preset;
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
    egui::Grid::new("shader_grid")
        .num_columns(2)
        .spacing(ROW_SPACING)
        .show(ui, |ui| {
            let labels: Vec<&str> = dialog
                .collections
                .iter()
                .map(|c| c.label.as_str())
                .collect();
            if let Some(index) = row(ui, "Collection", |ui| {
                combo(ui, "Collection", &labels, dialog.selected)
            }) {
                picked = Some(Picked::Collection(index));
            }
            // The levels of the selected collection, named after its pattern's
            // tags. The default collection has none, and shows this row alone.
            if let Some(browser) = dialog.browser() {
                for (level, name) in browser.names.iter().enumerate() {
                    if let Some(index) =
                        row(ui, name, |ui| draw_level(ui, level, &browser.levels[level]))
                    {
                        picked = Some(Picked::Level(level, index));
                    }
                }
            }
        });
    if !dialog.params.is_empty() {
        ui.add_space(ROW_SPACING.y);
        picked = params_body(ui, dialog).or(picked);
    }
    picked
}

/// The parameters the preset itself declares, folded away because a Mega Bezel
/// preset declares hundreds of them.
fn params_body(ui: &mut Ui, dialog: &ShaderDialog) -> Option<Picked> {
    let mut picked = None;
    egui::CollapsingHeader::new(egui::RichText::new("Parameters").size(LABEL_SIZE))
        .id_salt("shader_params")
        .show(ui, |ui| {
            if ui
                .button(egui::RichText::new("Reset").size(BODY_SIZE))
                .clicked()
            {
                picked = Some(Picked::Reset);
            }
            egui::Grid::new("shader_param_grid")
                .num_columns(2)
                .spacing(ROW_SPACING)
                .show(ui, |ui| {
                    for (index, param) in dialog.params.iter().enumerate() {
                        // Edited on a copy and reported back, because the dialog
                        // is borrowed for as long as the panel is being drawn.
                        let mut value = param.value;
                        let changed = row(ui, &param.label, |ui| {
                            draw_param(ui, index, param, &mut value)
                        });
                        if changed {
                            picked = Some(Picked::Param(index, value));
                        }
                    }
                });
        });
    picked
}

/// One parameter editor: the combo box its description listed the options of,
/// or the settings dialog's number widget.
fn draw_param(ui: &mut Ui, index: usize, param: &ShaderParam, value: &mut f32) -> bool {
    if param.options.is_empty() {
        return draw_number(ui, value, param.min, param.max, param.step);
    }
    let labels: Vec<&str> = param.options.iter().map(String::as_str).collect();
    let selected = ((*value - param.min) / param.step).round().max(0.0) as usize;
    let Some(index) = combo(ui, &format!("param{index}"), &labels, selected) else {
        return false;
    };
    *value = param.min + index as f32 * param.step;
    true
}

/// One row of the grid: its label on the left, whatever `widget` draws on the
/// right.
fn row<R>(ui: &mut Ui, label: &str, widget: impl FnOnce(&mut Ui) -> R) -> R {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.label(egui::RichText::new(label).size(LABEL_SIZE));
    });
    let inner = ui
        .scope(|ui| {
            ui.set_min_width(WIDGET_WIDTH);
            widget(ui)
        })
        .inner;
    ui.end_row();
    inner
}

/// One level's combo box, or a dash for a level with nothing to offer: a
/// directory holding no preset of the collection's shape.
fn draw_level(ui: &mut Ui, level: usize, state: &Level) -> Option<usize> {
    if state.raw().is_none() {
        ui.label(
            egui::RichText::new("—")
                .size(BODY_SIZE)
                .color(DISABLED_COLOR),
        );
        return None;
    }
    let labels: Vec<&str> = state.choices.iter().map(|c| c.label.as_str()).collect();
    // Salted by position rather than by tag name, which two collections may
    // share.
    combo(ui, &format!("level{level}"), &labels, state.index)
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
