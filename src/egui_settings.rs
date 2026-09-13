//! A settings dialog you get by declaring a struct.
//!
//! Hand [`ShowSettings`] any `#[derive(Reflect)]` struct and this module draws a
//! modal panel with one row per field -- the label on the left, an editor on the
//! right whose kind is derived from the field's *type* -- and a close button in
//! the corner. Every edit reports the whole edited struct back as a
//! [`SettingsApplied`] message as it is made, so a change is live the moment it
//! is made. Nothing is written for the caller; the caller decides what to do
//! with the value it gets.
//!
//! ```ignore
//! #[derive(Reflect, Clone, Default)]
//! struct MySettings {
//!     fullscreen: bool,
//!     #[reflect(@settings::Range::new(0.0, 120.0))]
//!     fps_cap: u32,
//!     title: String,
//!     tint: Color,
//!     scale: Resolution,   // a unit-only enum -> ComboBox
//! }
//!
//! app.add_settings_type::<MySettings>();
//! // ... then, from any system:
//! show.write(ShowSettings::new(current.clone(), "Video"));
//! ```
//!
//! Reflection rather than serde or a bespoke derive: `bevy_reflect` is already
//! compiled into the binary (a non-optional dependency of `bevy_ecs`), and it is
//! the only one of the three that hands over an enum's *variant list*
//! ([`EnumInfo::variant_names`]) -- which is exactly what a ComboBox needs and
//! what a `Serialize` pass cannot see. Variant *identifiers* rarely read well in
//! a combo box, so an enum that says `#[reflect(Display)]` gets its own
//! [`std::fmt::Display`] output as the label instead -- see [`ReflectDisplay`].
//!
//! This module knows nothing about what it is editing -- the app's own settings
//! struct, and what applying it does, live in [`crate::demarc_settings`].
//!
//! The module is split the way [`crate::fuzzy_list`] is split from its drawing:
//! [`describe`] and [`set_variant`] are pure functions over `dyn PartialReflect`
//! with no egui and no `App` in sight, and the tests exercise those.

use bevy::prelude::*;
use bevy::reflect::enums::{DynamicEnum, DynamicVariant, EnumInfo, VariantInfo};
use bevy::reflect::{
    FromType, GetTypeRegistration, NamedField, PartialReflect, ReflectMut, ReflectRef, TypeInfo,
    TypeRegistry,
};
use bevy_egui::{
    EguiContexts, EguiPrimaryContextPass,
    egui::{self, Ui},
};

use crate::egui_ui::{HudState, live_modifiers, panel_frame, sync_modifiers, take_key, update_ui};

/// Bounds for the [`egui::DragValue`] drawn for a numeric field, attached to the
/// field as a reflection attribute:
///
/// ```ignore
/// #[reflect(@Range::new(0.0, 120.0))]
/// fps_cap: u32,
/// ```
///
/// Without one, a number gets an unbounded drag. `speed` is how far one pixel of
/// drag moves the value; [`Range::new`] picks something sensible from the span,
/// and [`Range::with_speed`] overrides it.
#[derive(Reflect, Clone, Copy, Debug, PartialEq)]
pub struct Range {
    pub min: f64,
    pub max: f64,
    pub speed: f64,
}

impl Range {
    /// A range whose drag speed crosses it in roughly 300 pixels -- about a
    /// third of the panel's width, so the whole span is reachable in one drag
    /// without the value skittering.
    ///
    /// The bounds are generic so an integer field can be given integer bounds
    /// (`Range::new(0, 20)`) rather than having to spell them as floats. Every
    /// integer type narrower than 53 bits converts losslessly; `u64`/`i64`
    /// bounds have to be written as `f64` literals, which is no worse than the
    /// precision [`egui::DragValue`] works in anyway.
    pub fn new(min: impl Into<f64>, max: impl Into<f64>) -> Self {
        let (min, max) = (min.into(), max.into());
        Self {
            min,
            max,
            speed: ((max - min) / 300.0).abs(),
        }
    }

    #[allow(dead_code)] // Part of the module's API; nothing in-tree needs it yet.
    pub fn with_speed(min: impl Into<f64>, max: impl Into<f64>, speed: f64) -> Self {
        Self {
            min: min.into(),
            max: max.into(),
            speed,
        }
    }
}

/// Reflected access to a type's [`std::fmt::Display`] impl, so the dialog can
/// label a combo box's entries with what the enum prints rather than with the
/// identifiers its variants happen to be spelled with:
///
/// ```ignore
/// #[derive(Reflect)]
/// #[reflect(Display)]      // -> `ReflectDisplay`, which has to be in scope
/// enum Resolution { Res640x480, /* ... */ }
/// ```
///
/// Registered as type data by the `Reflect` derive, which is why [`describe`]
/// needs a [`TypeRegistry`] to find it.
#[derive(Clone)]
pub struct ReflectDisplay(fn(&dyn PartialReflect) -> Option<String>);

impl ReflectDisplay {
    fn label(&self, value: &dyn PartialReflect) -> Option<String> {
        (self.0)(value)
    }
}

impl<T: PartialReflect + std::fmt::Display> FromType<T> for ReflectDisplay {
    fn from_type() -> Self {
        Self(|value| Some(value.try_downcast_ref::<T>()?.to_string()))
    }
}

/// One entry of a combo box: the name reflection knows the variant by, which is
/// what [`set_variant`] switches to, and the text shown in its place.
#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub name: &'static str,
    pub label: String,
}

/// The editor one field gets, chosen from its type by [`describe`].
#[derive(Clone, Debug, PartialEq)]
pub enum Widget {
    /// `bool` -- a checkbox.
    Bool,
    /// `String` -- a single-line text field.
    Text,
    /// A colour -- a swatch that opens egui's picker.
    Color,
    /// Any integer width -- a drag value stepping by whole numbers.
    Int { range: Option<Range> },
    /// `f32`/`f64` -- a drag value stepping fractionally.
    Float { range: Option<Range> },
    /// An enum whose variants all carry no data -- a combo box.
    Enum { variants: Vec<Variant> },
    /// A nested struct. Not a row: [`sections`] lifts it out into a group of
    /// rows of its own under a heading.
    Section,
    /// A type this dialog has no editor for. Drawn as a greyed-out row rather
    /// than skipped, so a field that silently cannot be edited is still visible.
    Unsupported,
}

/// One row of the dialog.
#[derive(Clone, Debug)]
pub struct Field {
    /// The field's name in the struct, as reflection reports it.
    pub name: &'static str,
    /// `snake_case` turned into `Snake Case` for display.
    pub label: String,
    pub widget: Widget,
}

/// Describes every field of a reflected struct, in declaration order.
///
/// Called once when the dialog opens, not per frame. Anything that is not a
/// struct -- and any field the derive was told to `#[reflect(ignore)]`, which
/// never reaches us -- yields nothing. A field that is itself a struct comes
/// back as [`Widget::Section`].
///
/// `registry` is only consulted for [`ReflectDisplay`].
pub fn describe(value: &dyn PartialReflect, registry: &TypeRegistry) -> Vec<Field> {
    let ReflectRef::Struct(s) = value.reflect_ref() else {
        return Vec::new();
    };
    let Some(TypeInfo::Struct(info)) = value.get_represented_type_info() else {
        return Vec::new();
    };
    (0..s.field_len())
        .filter_map(|i| {
            let field = info.field_at(i)?;
            let value = s.field_at(i)?;
            Some(Field {
                name: field.name(),
                label: title_case(field.name()),
                widget: widget_for(field, value, registry),
            })
        })
        .collect()
}

/// `idle_timeout` -> `Idle Timeout`. Field names are the only labels we
/// have: doc comments would be better, but `NamedField::docs` sits behind
/// bevy's `reflect_documentation` feature.
fn title_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for (i, word) in name.split('_').filter(|w| !w.is_empty()).enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// Picks the editor for one field.
///
/// **Order matters.** `Color` is itself a reflected *enum* (over its colour
/// spaces) and `Option<T>` is an enum with a payload-carrying `Some`, so the
/// concrete types have to be recognised before the generic enum arm, and that
/// arm has to insist every variant is a unit variant.
fn widget_for(field: &NamedField, value: &dyn PartialReflect, registry: &TypeRegistry) -> Widget {
    let range = field.get_attribute::<Range>().copied();
    let Some(info) = field.type_info() else {
        return Widget::Unsupported;
    };
    let id = info.type_id();

    if id == std::any::TypeId::of::<bool>() {
        return Widget::Bool;
    }
    if id == std::any::TypeId::of::<String>() {
        return Widget::Text;
    }
    if is_color(id) {
        return Widget::Color;
    }
    if is_int(id) {
        return Widget::Int { range };
    }
    if is_float(id) {
        return Widget::Float { range };
    }
    // Below the colour check, because `Srgba` and `LinearRgba` are structs too.
    if matches!(value.reflect_ref(), ReflectRef::Struct(_)) {
        return Widget::Section;
    }
    // The type info is the authority on the variant list; the value only tells
    // us which one is live. Fall back to the value's own info for a field typed
    // as something dynamic.
    let enum_info = match info {
        TypeInfo::Enum(e) => Some(e),
        _ => match value.get_represented_type_info() {
            Some(TypeInfo::Enum(e)) => Some(e),
            _ => None,
        },
    };
    match enum_info {
        Some(e) if all_unit_variants(e) => Widget::Enum {
            variants: variants(e, value, registry),
        },
        _ => Widget::Unsupported,
    }
}

/// Labels every variant of a unit enum: with the type's `Display` impl if it
/// registered one, and with the variant identifiers otherwise.
fn variants(info: &EnumInfo, value: &dyn PartialReflect, registry: &TypeRegistry) -> Vec<Variant> {
    let display = registry.get_type_data::<ReflectDisplay>(info.type_id());
    info.variant_names()
        .iter()
        .map(|&name| Variant {
            name,
            label: display
                .and_then(|d| displayed(d, value, name))
                .unwrap_or_else(|| name.to_owned()),
        })
        .collect()
}

/// What the enum would print in variant `name`. `Display` is implemented on the
/// concrete type, so this needs a real value of it rather than a `DynamicEnum`,
/// which [`PartialReflect::reflect_clone`] gives.
fn displayed(display: &ReflectDisplay, value: &dyn PartialReflect, name: &str) -> Option<String> {
    let mut value = value.reflect_clone().ok()?;
    set_variant(value.as_partial_reflect_mut(), name);
    display.label(value.as_partial_reflect())
}

/// Whether every variant carries no data, which is what makes an enum a plain
/// pick-one list. `Option<T>` and `ScaleModeArg::Fixed(f32)` fail this.
fn all_unit_variants(info: &EnumInfo) -> bool {
    info.iter().all(|v| matches!(v, VariantInfo::Unit(_)))
}

fn is_color(id: std::any::TypeId) -> bool {
    use bevy::color::{LinearRgba, Srgba};
    id == std::any::TypeId::of::<Color>()
        || id == std::any::TypeId::of::<Srgba>()
        || id == std::any::TypeId::of::<LinearRgba>()
}

macro_rules! any_of {
    ($id:expr, $($ty:ty),+ $(,)?) => {
        $($id == std::any::TypeId::of::<$ty>())||+
    };
}

/// Every integer width [`egui::DragValue`] can actually take. `i128`/`u128` are
/// deliberately absent -- egui has no `Numeric` impl for them, so they fall
/// through to [`Widget::Unsupported`] rather than to a drag we cannot draw.
fn is_int(id: std::any::TypeId) -> bool {
    any_of!(id, i8, i16, i32, i64, isize, u8, u16, u32, u64, usize)
}

fn is_float(id: std::any::TypeId) -> bool {
    any_of!(id, f32, f64)
}

/// Switches a reflected unit enum to the variant called `variant`.
///
/// Returns whether it changed: `false` if `field` is not an enum, if it is
/// already on that variant, or if the enum has no such variant. The derived
/// `try_apply` does the real work -- given a [`DynamicEnum`] naming a different
/// variant it reconstructs the value as that variant.
pub fn set_variant(field: &mut dyn PartialReflect, variant: &str) -> bool {
    let ReflectRef::Enum(current) = field.reflect_ref() else {
        return false;
    };
    if current.variant_name() == variant {
        return false;
    }
    let mut new = DynamicEnum::new(variant, DynamicVariant::Unit);
    if let Some(info) = field.get_represented_type_info() {
        new.set_represented_type(Some(info));
    }
    field.try_apply(&new).is_ok()
}

/// A run of rows drawn together under one heading: the fields of the root
/// struct (the first section, whose `title` is empty), then one section per
/// nested struct.
#[derive(Clone, Debug)]
pub struct Section {
    pub title: String,
    /// Field indices leading from the root struct down to the struct these rows
    /// edit; empty for the root itself. [`field_at_path`] walks it.
    pub path: Vec<usize>,
    /// Every field of that struct, so an index here is the field index there.
    pub fields: Vec<Field>,
}

/// Groups a reflected struct into sections: its own fields first, then one
/// section per nested struct, depth first in declaration order. Deeper nesting
/// joins the labels (`Wine / Prefix`).
pub fn sections(value: &dyn PartialReflect, registry: &TypeRegistry) -> Vec<Section> {
    let mut out = Vec::new();
    collect_sections(value, "", &[], registry, &mut out);
    out
}

fn collect_sections(
    value: &dyn PartialReflect,
    title: &str,
    path: &[usize],
    registry: &TypeRegistry,
    out: &mut Vec<Section>,
) {
    let ReflectRef::Struct(s) = value.reflect_ref() else {
        return;
    };
    let fields = describe(value, registry);
    let nested: Vec<(usize, String)> = fields
        .iter()
        .enumerate()
        .filter(|(_, f)| f.widget == Widget::Section)
        .map(|(i, f)| (i, f.label.clone()))
        .collect();
    out.push(Section {
        title: title.to_owned(),
        path: path.to_vec(),
        fields,
    });
    for (i, label) in nested {
        let Some(child) = s.field_at(i) else { continue };
        let title = if title.is_empty() {
            label
        } else {
            format!("{title} / {label}")
        };
        let mut child_path = path.to_vec();
        child_path.push(i);
        collect_sections(child, &title, &child_path, registry, out);
    }
}

/// Follows a [`Section::path`] from the root struct to the struct it names.
pub fn field_at_path<'a>(
    root: &'a mut dyn PartialReflect,
    path: &[usize],
) -> Option<&'a mut dyn PartialReflect> {
    let Some((&index, rest)) = path.split_first() else {
        return Some(root);
    };
    let ReflectMut::Struct(s) = root.reflect_mut() else {
        return None;
    };
    field_at_path(s.field_at_mut(index)?, rest)
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Sizes are in the 1600-tall virtual space `crate::egui_ui::update_ui` sets up
/// with `set_pixels_per_point`, so these are much larger than egui's defaults.
pub(crate) const TITLE_SIZE: f32 = 40.0;
/// Side of the square close button in the panel's top-right corner.
pub(crate) const CLOSE_SIZE: f32 = 40.0;
pub(crate) const LABEL_SIZE: f32 = 28.0;
/// Heading over a group of rows coming from one nested struct.
pub(crate) const SECTION_SIZE: f32 = 32.0;
pub(crate) const BODY_SIZE: f32 = 26.0;
/// Width of the editor column. Fixed, so the rows line up and the panel does not
/// resize as a combo box's text changes.
pub(crate) const WIDGET_WIDTH: f32 = 320.0;
pub(crate) const ROW_SPACING: egui::Vec2 = egui::vec2(24.0, 12.0);
/// Fraction of the screen height the field grid may take before it scrolls.
pub(crate) const GRID_HEIGHT_FRACTION: f32 = 0.85;
/// Fraction of the screen the panel is at least as wide and tall as.
pub(crate) const PANEL_MIN_FRACTION: egui::Vec2 = egui::vec2(0.45, 0.5);
pub(crate) const DISABLED_COLOR: egui::Color32 = egui::Color32::from_rgb(0x80, 0x80, 0x80);
/// How much of the close button's side the painted cross spans.
const CROSS_FRACTION: f32 = 0.45;

/// Scales the widgets that size themselves from the *style* rather than from a
/// font we hand them -- checkboxes, drag values, colour swatches, buttons.
///
/// `setup_egui` only overrides the Heading and Body text styles, so Button (what
/// a [`egui::DragValue`] and a [`egui::Button`] label themselves with) is left at
/// egui's default 14pt, which is unreadably small in this app's 1600-tall
/// virtual space. The spacing has to grow with it or the widgets stay
/// letterbox-thin around the bigger text.
pub(crate) fn scale_widgets(ui: &mut Ui) {
    let style = ui.style_mut();
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::proportional(BODY_SIZE),
    );
    style.spacing.interact_size = egui::vec2(BODY_SIZE * 2.0, BODY_SIZE * 1.5);
    style.spacing.button_padding = egui::vec2(BODY_SIZE * 0.4, BODY_SIZE * 0.2);
    style.spacing.icon_width = BODY_SIZE;
    style.spacing.icon_width_inner = BODY_SIZE * 0.6;
    style.spacing.icon_spacing = BODY_SIZE * 0.3;
}

/// Draws every section -- the root struct's own rows first, then one heading
/// and grid per nested struct -- and returns whether anything was edited this
/// frame.
///
/// Deliberately not generic: it works through `dyn PartialReflect`, so a dozen
/// settings types share one copy of this code and only the thin ECS wrapper
/// around it is monomorphised.
fn settings_body(ui: &mut Ui, value: &mut dyn PartialReflect, sections: &[Section]) -> bool {
    let mut changed = false;
    for (i, section) in sections.iter().enumerate() {
        let Some(target) = field_at_path(value, &section.path) else {
            continue;
        };
        if !section.title.is_empty() {
            if i > 0 {
                ui.add_space(ROW_SPACING.y * 2.0);
            }
            ui.label(
                egui::RichText::new(&section.title)
                    .size(SECTION_SIZE)
                    .strong(),
            );
            ui.add_space(ROW_SPACING.y);
        }
        changed |= section_rows(ui, i, target, &section.fields);
    }
    changed
}

/// One section's rows, in a two-column grid -- label right-aligned on the left,
/// editor on the right. `section` keeps egui ids unique across the panel.
fn section_rows(
    ui: &mut Ui,
    section: usize,
    value: &mut dyn PartialReflect,
    fields: &[Field],
) -> bool {
    let ReflectMut::Struct(target) = value.reflect_mut() else {
        return false;
    };
    let mut changed = false;
    egui::Grid::new(("settings_grid", section))
        .num_columns(2)
        .spacing(ROW_SPACING)
        .show(ui, |ui| {
            for (i, field) in fields.iter().enumerate() {
                if field.widget == Widget::Section {
                    continue;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(&field.label).size(LABEL_SIZE));
                });
                ui.scope(|ui| {
                    ui.set_min_width(WIDGET_WIDTH);
                    match target.field_at_mut(i) {
                        Some(value) => changed |= draw_widget(ui, section, field, value),
                        // `describe` and the value came from the same struct, so
                        // this cannot fire; draw something rather than panic.
                        None => {
                            unsupported(ui);
                        }
                    }
                });
                ui.end_row();
            }
        });
    changed
}

fn unsupported(ui: &mut Ui) {
    ui.label(
        egui::RichText::new("<unsupported>")
            .size(BODY_SIZE)
            .color(DISABLED_COLOR)
            .italics(),
    );
}

/// Draws the editor for one field. `value` is that field, already narrowed out
/// of the struct.
fn draw_widget(ui: &mut Ui, section: usize, field: &Field, value: &mut dyn PartialReflect) -> bool {
    match &field.widget {
        Widget::Bool => match value.try_downcast_mut::<bool>() {
            Some(v) => ui.checkbox(v, "").changed(),
            None => false,
        },
        Widget::Text => match value.try_downcast_mut::<String>() {
            Some(v) => ui
                .add(
                    egui::TextEdit::singleline(v)
                        .desired_width(WIDGET_WIDTH)
                        .font(egui::FontId::proportional(BODY_SIZE)),
                )
                .changed(),
            None => false,
        },
        Widget::Color => draw_color(ui, value),
        Widget::Int { range } => draw_int(ui, value, *range),
        Widget::Float { range } => draw_float(ui, value, *range),
        Widget::Enum { variants } => draw_enum(ui, (section, field.name), variants, value),
        Widget::Section | Widget::Unsupported => {
            unsupported(ui);
            false
        }
    }
}

fn draw_color(ui: &mut Ui, value: &mut dyn PartialReflect) -> bool {
    use bevy::color::{LinearRgba, Srgba};

    // Read whichever colour type this is out as sRGB bytes, let egui edit those,
    // and write the result back in the field's own colour space.
    let (mut rgba, write): (egui::Color32, fn(&mut dyn PartialReflect, Srgba)) =
        if let Some(c) = value.try_downcast_ref::<Color>() {
            (to_egui(Srgba::from(*c)), |v, s| {
                if let Some(c) = v.try_downcast_mut::<Color>() {
                    *c = Color::Srgba(s);
                }
            })
        } else if let Some(c) = value.try_downcast_ref::<Srgba>() {
            (to_egui(*c), |v, s| {
                if let Some(c) = v.try_downcast_mut::<Srgba>() {
                    *c = s;
                }
            })
        } else if let Some(c) = value.try_downcast_ref::<LinearRgba>() {
            (to_egui(Srgba::from(*c)), |v, s| {
                if let Some(c) = v.try_downcast_mut::<LinearRgba>() {
                    *c = LinearRgba::from(s);
                }
            })
        } else {
            unsupported(ui);
            return false;
        };

    let changed = egui::color_picker::color_edit_button_srgba(
        ui,
        &mut rgba,
        egui::color_picker::Alpha::OnlyBlend,
    )
    .changed();
    if changed {
        let [r, g, b, a] = rgba.to_srgba_unmultiplied();
        write(value, Srgba::rgba_u8(r, g, b, a));
    }
    changed
}

fn to_egui(c: bevy::color::Srgba) -> egui::Color32 {
    use bevy::color::ColorToPacked;
    let [r, g, b, a] = c.to_u8_array();
    egui::Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// Builds the drag for one numeric type, honouring `range` if the field carried
/// one. `speed` is the fallback for a field without a [`Range`].
fn drag<T: egui::emath::Numeric>(ui: &mut Ui, v: &mut T, range: Option<Range>, speed: f64) -> bool {
    let mut widget = egui::DragValue::new(v);
    widget = match range {
        Some(r) => widget.speed(r.speed).range(r.min..=r.max),
        None => widget.speed(speed),
    };
    ui.add(widget).changed()
}

/// One arm per integer width. `try_downcast_mut` is what tells them apart, so
/// this has to be spelled out; the macro keeps it to one line each.
macro_rules! drag_arms {
    ($ui:expr, $value:expr, $range:expr, $speed:expr, $($ty:ty),+ $(,)?) => {{
        $(
            if let Some(v) = $value.try_downcast_mut::<$ty>() {
                return drag($ui, v, $range, $speed);
            }
        )+
    }};
}

/// The editor for a bare `f32` that carries its own bounds and step: a checkbox
/// for a 0/1 flag, a whole-number drag for an integral step, a fractional drag
/// otherwise.
///
/// Shared with the shader dialog, whose slangp parameters are all `f32` with
/// exactly this metadata -- there is no type to derive a widget from there, so
/// the step is what stands in for one.
pub(crate) fn draw_number(ui: &mut Ui, value: &mut f32, min: f32, max: f32, step: f32) -> bool {
    let whole = step >= 1.0 && step.fract() == 0.0;
    if whole && min == 0.0 && max == 1.0 {
        let mut on = *value >= 0.5;
        if !ui.checkbox(&mut on, "").changed() {
            return false;
        }
        *value = f32::from(u8::from(on));
        return true;
    }
    let speed = f64::from(max - min).abs() / 300.0;
    let range = Some(Range::with_speed(min, max, speed.max(f64::from(step))));
    if !whole {
        return drag(ui, value, range, speed);
    }
    let mut whole_value = value.round() as i64;
    if !drag(ui, &mut whole_value, range, speed) {
        return false;
    }
    *value = whole_value as f32;
    true
}

fn draw_int(ui: &mut Ui, value: &mut dyn PartialReflect, range: Option<Range>) -> bool {
    drag_arms!(
        ui, value, range, 1.0, i8, i16, i32, i64, isize, u8, u16, u32, u64, usize
    );
    unsupported(ui);
    false
}

fn draw_float(ui: &mut Ui, value: &mut dyn PartialReflect, range: Option<Range>) -> bool {
    // A tenth of a unit per pixel: fine enough for the 0..1-ish factors this app
    // is full of, without making a large value take a mile of dragging.
    drag_arms!(ui, value, range, 0.1, f32, f64);
    unsupported(ui);
    false
}

fn draw_enum(
    ui: &mut Ui,
    salt: (usize, &str),
    variants: &[Variant],
    value: &mut dyn PartialReflect,
) -> bool {
    let ReflectRef::Enum(current) = value.reflect_ref() else {
        unsupported(ui);
        return false;
    };
    let current = current.variant_name().to_owned();
    let selected_text = variants
        .iter()
        .find(|v| v.name == current)
        .map_or(current.as_str(), |v| v.label.as_str());
    // Picked inside the closure and applied after it, because the combo box
    // holds `value`'s borrow for as long as it is open.
    let mut picked = None;
    egui::ComboBox::from_id_salt(salt)
        .selected_text(egui::RichText::new(selected_text).size(BODY_SIZE))
        .width(WIDGET_WIDTH)
        .show_ui(ui, |ui| {
            for variant in variants {
                // `selectable_label` rather than `selectable_value`, which would
                // force `PartialEq` on every settings enum. Three of the ones in
                // `crate::config` do not derive it.
                let selected = variant.name == current;
                if ui
                    .selectable_label(
                        selected,
                        egui::RichText::new(&variant.label).size(BODY_SIZE),
                    )
                    .clicked()
                {
                    picked = Some(variant.name);
                }
            }
        });
    match picked {
        Some(variant) => set_variant(value, variant),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// The dialog: messages, state, systems
// ---------------------------------------------------------------------------

/// Opens the settings dialog over `value`. The dialog edits a copy of it and
/// reports every edit back as a [`SettingsApplied`]; the caller's own value is
/// never touched.
///
/// One dialog is open at a time per settings type `T`; writing this again while
/// one is up replaces what it is editing.
#[derive(Message, Clone)]
pub struct ShowSettings<T> {
    pub value: T,
    /// Heading above the fields.
    pub title: String,
}

impl<T> ShowSettings<T> {
    pub fn new(value: T, title: impl Into<String>) -> Self {
        Self {
            value,
            title: title.into(),
        }
    }
}

/// The edited settings, emitted on every edit. There is no confirmation step:
/// what the dialog shows is what the caller has already been handed.
#[derive(Message, Clone)]
pub struct SettingsApplied<T>(pub T);

/// The open dialog for one settings type.
#[derive(Resource)]
struct SettingsState<T> {
    open: bool,
    /// What the widgets edit.
    draft: T,
    /// Described once when the dialog opens, not per frame.
    sections: Vec<Section>,
    title: String,
}

impl<T: Default> Default for SettingsState<T> {
    fn default() -> Self {
        Self {
            open: false,
            draft: T::default(),
            sections: Vec::new(),
            title: String::new(),
        }
    }
}

/// Adds the resource, messages and systems for one settings type.
///
/// Mirrors [`crate::jobs::AppJobsExt::add_job_type`], the other generic
/// registration in this app.
pub trait AppSettingsExt {
    fn add_settings_type<T: SettingsType>(&mut self) -> &mut Self;
}

/// What a struct must be to get a dialog: reflected (that is where the fields,
/// their types and an enum's variants come from), registrable (which is how the
/// `#[reflect(Display)]` of a nested enum reaches [`describe`]), clonable (the
/// draft is a plain copy, which is what saves us needing `FromReflect`) and
/// default-constructible (the resource exists before the first open).
pub trait SettingsType:
    Reflect + GetTypeRegistration + Clone + Default + Send + Sync + 'static
{
}
impl<T: Reflect + GetTypeRegistration + Clone + Default + Send + Sync + 'static> SettingsType
    for T
{
}

impl AppSettingsExt for App {
    fn add_settings_type<T: SettingsType>(&mut self) -> &mut Self {
        // Registers the field types too, so a nested enum's `ReflectDisplay`
        // lands in the registry.
        self.register_type::<T>()
            .init_resource::<SettingsState<T>>()
            .add_message::<ShowSettings<T>>()
            .add_message::<SettingsApplied<T>>()
            .add_systems(
                Update,
                open_settings::<T>.run_if(on_message::<ShowSettings<T>>),
            )
            // After `update_ui`, which is what sets `pixels_per_point` for the
            // frame -- drawing before it would size the first frame wrongly.
            .add_systems(EguiPrimaryContextPass, settings_ui::<T>.after(update_ui))
    }
}

fn open_settings<T: SettingsType>(
    mut state: ResMut<SettingsState<T>>,
    mut hud: ResMut<HudState>,
    registry: Res<AppTypeRegistry>,
    mut reader: MessageReader<ShowSettings<T>>,
) {
    let registry = registry.read();
    // Last writer this frame wins; opening two dialogs over one type is a
    // caller bug, not something to queue up.
    for msg in reader.read() {
        state.sections = sections(msg.value.as_partial_reflect(), &registry);
        state.draft = msg.value.clone();
        state.title = msg.title.clone();
        // Reported once per transition: `HudState` counts open dialogs, so
        // reopening one that is already up must not count twice.
        if !state.open {
            hud.set_settings_open(true);
        }
        state.open = true;
    }
}

fn settings_ui<T: SettingsType>(
    mut contexts: EguiContexts,
    mut state: ResMut<SettingsState<T>>,
    mut hud: ResMut<HudState>,
    keys: Res<ButtonInput<KeyCode>>,
    mut applied: MessageWriter<SettingsApplied<T>>,
) -> Result {
    if !state.open {
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?;

    // Same reason as the picker: egui only learns of a modifier through the key
    // events Bevy feeds it, so its own idea of what is held goes stale.
    let mods = live_modifiers(&keys);
    let mut closing = ctx.input_mut(|i| {
        sync_modifiers(i, mods);
        take_key(i, egui::Key::Escape) > 0
    });
    let mut edited = false;

    egui::Area::new(egui::Id::new("settings"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            panel_frame().show(ui, |ui| {
                scale_widgets(ui);
                let min = ctx.content_rect().size() * PANEL_MIN_FRACTION;
                let panel = ui
                    .vertical(|ui| {
                        ui.set_min_size(min);
                        ui.horizontal(|ui| {
                            if !state.title.is_empty() {
                                ui.label(
                                    egui::RichText::new(&state.title).size(TITLE_SIZE).strong(),
                                );
                            }
                            // Room for the close button, which is placed in this
                            // row's right corner once the width is known.
                            ui.add_space(CLOSE_SIZE);
                        });
                        ui.add_space(ROW_SPACING.y);
                        let max_height = ctx.content_rect().height() * GRID_HEIGHT_FRACTION;
                        egui::ScrollArea::vertical()
                            .max_height(max_height)
                            .auto_shrink([true, true])
                            .show(ui, |ui| {
                                let SettingsState {
                                    draft, sections, ..
                                } = &mut *state;
                                edited =
                                    settings_body(ui, draft.as_partial_reflect_mut(), sections);
                            });
                    })
                    .response
                    .rect;
                closing |= close_button(ui, panel);
            });
        });

    // Every edit goes out as it is made, so closing has nothing left to confirm
    // or to undo.
    if edited {
        applied.write(SettingsApplied(state.draft.clone()));
    }
    if closing {
        close(&mut state, &mut hud);
    }
    Ok(())
}

/// The x in the panel's top-right corner, which closes the dialog exactly as
/// Escape does.
///
/// Placed against `panel` -- the rect the title and fields ended up occupying --
/// because the panel is only as wide as its widest row, which is not known until
/// they are drawn. The title row has already reserved [`CLOSE_SIZE`] for it, so
/// the two cannot collide.
///
/// The cross is painted rather than written: the app's bitmap font has nothing
/// above Latin-1, so a `U+2715` glyph came out as a missing-character box.
pub(crate) fn close_button(ui: &mut Ui, panel: egui::Rect) -> bool {
    let rect = egui::Rect::from_min_size(
        egui::pos2(panel.right() - CLOSE_SIZE, panel.top()),
        egui::Vec2::splat(CLOSE_SIZE),
    );
    // `min_size`, because an empty button otherwise shrinks to its padding.
    let response = ui.put(rect, egui::Button::new("").min_size(rect.size()));
    let arm = response.rect.size().min_elem() * CROSS_FRACTION * 0.5;
    let center = response.rect.center();
    let stroke = egui::Stroke::new(
        (arm * 0.22).max(1.0),
        ui.style().interact(&response).fg_stroke.color,
    );
    let painter = ui.painter();
    for dir in [egui::vec2(arm, arm), egui::vec2(arm, -arm)] {
        painter.line_segment([center - dir, center + dir], stroke);
    }
    response.clicked()
}

fn close<T: SettingsType>(state: &mut SettingsState<T>, hud: &mut HudState) {
    state.open = false;
    state.sections.clear();
    hud.set_settings_open(false);
}

#[cfg(test)]
#[path = "tests/egui_settings_tests.rs"]
mod tests;
