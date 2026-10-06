//! The Bevy side of the egui UI in [`retro_ui`]: feeds it the app's messages
//! and state, and turns what it returns back into messages.

use bevy::{camera::visibility::RenderLayers, prelude::*};
use bevy_egui::{
    EguiContexts, EguiGlobalSettings, EguiPlugin, EguiPrimaryContextPass, PrimaryEguiContext, egui,
};
use retro_ui::{Hud, Picker};

use crate::config::Args;
use crate::emu_file::{Award, EmuFile};
use crate::headless::{HeadlessTarget, camera_target};
use crate::ui::{FuzzyListSelect, SetHudText, ShowFuzzyList, UiState};

use resvg::tiny_skia;
use resvg::usvg::{self, Tree};

pub struct EguiUiPlugin;

/// Keeps `font.ttf` alive for [`setup_egui`]. Loading through the asset server
/// rather than reading [`crate::system_dir`] directly means egui picks
/// up the very same face -- and the same hot-reloaded bytes -- as the Bevy UI in
/// [`crate::hud`] and [`crate::text_input`].
#[derive(Resource)]
struct AppFont(Handle<Font>);

fn load_font(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.insert_resource(AppFont(asset_server.load("font.ttf")));
}

const GOLD_COLOR: egui::Color32 = egui::Color32::from_rgb(0x80, 0x60, 0x10);
const SILVER_COLOR: egui::Color32 = egui::Color32::from_rgb(0x60, 0x60, 0x80);

static ICON_SVG: &[u8] = include_bytes!("../files/coupdecoeur.svg");
static STAR_SVG: &[u8] = include_bytes!("../files/viewingtip.svg");

/// Rasterize an SVG (from bytes) into an egui::ColorImage at the given
/// pixel size. `target_size` is in physical pixels.
pub(crate) fn rasterize_svg(
    svg_bytes: &[u8],
    target_size: [u32; 2],
) -> anyhow::Result<egui::ColorImage> {
    let opt = usvg::Options::default();

    // If your SVG uses system fonts (text elements), you need a fontdb.
    // Skip this if your SVG is pure vector shapes.
    // let mut fontdb = usvg::fontdb::Database::new();
    // fontdb.load_system_fonts();

    let tree = Tree::from_data(svg_bytes, &opt)?;

    let [w, h] = target_size;
    let mut pixmap =
        tiny_skia::Pixmap::new(w, h).ok_or_else(|| anyhow::anyhow!("invalid pixmap dimensions"))?;

    // Scale the SVG's own viewBox size to fit target_size.
    let svg_size = tree.size();
    let transform =
        tiny_skia::Transform::from_scale(w as f32 / svg_size.width(), h as f32 / svg_size.height());

    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // tiny_skia::Pixmap stores premultiplied RGBA — egui::ColorImage
    // wants straight (non-premultiplied) RGBA, so unpremultiply per pixel.
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for px in pixmap.pixels() {
        let a = px.alpha();
        if a == 0 {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let unpremul = |c: u8| ((c as u32 * 255) / a as u32) as u8;
            rgba.extend_from_slice(&[
                unpremul(px.red()),
                unpremul(px.green()),
                unpremul(px.blue()),
                a,
            ]);
        }
    }

    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        &rgba,
    ))
}

/// Call once (e.g. lazily on first frame, or in app init) and cache the handle.
fn load_icon_texture(
    ctx: &egui::Context,
    name: &str,
    pixels: &[u8],
) -> anyhow::Result<egui::TextureHandle> {
    let image = rasterize_svg(pixels, [64, 64])?;
    Ok(ctx.load_texture(name, image, egui::TextureOptions::LINEAR))
}

fn setup_egui(
    mut contexts: EguiContexts,
    app_font: Res<AppFont>,
    fonts: Res<Assets<Font>>,
    mut done: Local<bool>,
) -> Result {
    if *done {
        return Ok(());
    }
    let Some(font) = fonts.get(&app_font.0) else {
        return Ok(());
    };
    // egui owns its font bytes (it re-parses them for its own atlas), so this
    // copies out of the Bevy asset instead of sharing the `Blob`.
    retro_ui::apply_style(contexts.ctx_mut()?, font.data.data().to_vec());
    *done = true;
    Ok(())
}

/// The icon textures for `ctx`, rasterized and uploaded on first use. Each
/// window's context has its own texture manager, so they cannot be shared.
fn icons(ctx: &egui::Context) -> Option<(egui::TextureId, egui::TextureId)> {
    let key = egui::Id::new("icons");
    let pair = match ctx.data(|d| d.get_temp::<(egui::TextureHandle, egui::TextureHandle)>(key)) {
        Some(pair) => pair,
        None => {
            let heart = load_icon_texture(ctx, "heart_icon", ICON_SVG).ok()?;
            let star = load_icon_texture(ctx, "star_icon", STAR_SVG).ok()?;
            ctx.data_mut(|d| d.insert_temp(key, (heart.clone(), star.clone())));
            (heart, star)
        }
    };
    Some((pair.0.id(), pair.1.id()))
}

#[derive(Resource, Default)]
pub struct HudState {
    hud: Hud,
    picker: Picker<EmuFile>,
    /// How many dialogs (`crate::egui_settings`, `crate::shader_dialog`) are up.
    /// Counted rather than a flag so closing one dialog while another is still
    /// open does not hand the keyboard back to the emulated machine.
    open_dialogs: u32,
}

impl HudState {
    /// The search box text, when the list currently open is `id`'s.
    pub fn list_query(&self, id: usize) -> Option<&str> {
        self.picker.query(id)
    }

    /// Source id of the highlighted row in list `id`, while it is open.
    pub fn list_selected_item(&self, id: usize) -> Option<usize> {
        self.picker.selected_item(id)
    }

    /// Told by a dialog as it opens and closes. Each dialog reports each
    /// transition once, so the count only has to survive a stray close.
    pub fn set_settings_open(&mut self, open: bool) {
        self.open_dialogs = if open {
            self.open_dialogs + 1
        } else {
            self.open_dialogs.saturating_sub(1)
        };
    }
}

/// The modifier keys held *right now*, read from Bevy rather than from egui.
/// egui only learns about a modifier through the key events it is fed, so one
/// pressed here and released while another window had focus stays "held" for
/// good -- and every exact-match lookup against [`egui::Modifiers::NONE`] then
/// quietly stops matching, which is what leaves the picker unable to see a
/// plain arrow key again. Bevy clears its keyboard state outright on
/// [`KeyboardFocusLost`](bevy::input::keyboard::KeyboardFocusLost), so this
/// answer recovers by itself.
pub(crate) fn live_modifiers(keys: &ButtonInput<KeyCode>) -> egui::Modifiers {
    let held = |a, b| keys.pressed(a) || keys.pressed(b);
    let alt = held(KeyCode::AltLeft, KeyCode::AltRight);
    let ctrl = held(KeyCode::ControlLeft, KeyCode::ControlRight);
    let shift = held(KeyCode::ShiftLeft, KeyCode::ShiftRight);
    let mac_cmd = cfg!(target_os = "macos") && held(KeyCode::SuperLeft, KeyCode::SuperRight);
    egui::Modifiers {
        alt,
        ctrl,
        shift,
        mac_cmd,
        // What "the" modifier is: Cmd on macOS, Ctrl everywhere else.
        command: if cfg!(target_os = "macos") {
            mac_cmd
        } else {
            ctrl
        },
    }
}

/// Replaces what egui believes is held -- both the running state and the
/// modifiers stamped on the key events still queued for this frame -- with
/// `mods`, so everything drawn afterwards resolves its shortcuts against the
/// live keyboard instead of a stuck one.
fn sync_modifiers(i: &mut egui::InputState, mods: egui::Modifiers) {
    i.modifiers = mods;
    for event in &mut i.events {
        if let egui::Event::Key { modifiers, .. } = event {
            *modifiers = mods;
        }
    }
}

pub(crate) fn update_ui(
    mut contexts: EguiContexts,
    mut state: ResMut<HudState>,
    time: Res<Time>,
    mut selected: MessageWriter<FuzzyListSelect>,
    keys: Res<ButtonInput<KeyCode>>,
    camera: Single<&Camera, With<PrimaryEguiContext>>,
) -> Result {
    let ctx = contexts.ctx_mut()?;
    if let (Some(size), Some(factor)) =
        (camera.logical_target_size(), camera.target_scaling_factor())
    {
        retro_ui::set_scale(ctx, size.y, factor);
    }
    // Before anything reads this frame's keys, the dialogs drawn after this
    // system included.
    let mods = live_modifiers(&keys);
    ctx.input_mut(|i| sync_modifiers(i, mods));
    state.hud.show(
        ctx,
        time.elapsed_secs(),
        crate::emu_file::bytes_in_progress(),
    );
    if let Some(picked) = draw_picker(ctx, &mut state.picker) {
        selected.write(FuzzyListSelect {
            id: picked.id,
            item: picked.item,
            text: picked.text,
            alt: picked.alt,
            emu_file: picked.data,
        });
    }
    Ok(())
}

fn draw_picker(
    ctx: &egui::Context,
    picker: &mut Picker<EmuFile>,
) -> Option<retro_ui::Picked<EmuFile>> {
    let (heart_id, star_id) = icons(ctx)?;
    picker.show(ctx, |job, source, id| {
        let mut cdc = 0;
        let mut vt = false;
        let mut winner = 0;
        let mut nominee = 0;
        if let Some(emu_file) = source.get_data(id) {
            cdc = emu_file.cdc();
            for award in emu_file.get_wins() {
                match award {
                    Award::ViewingTip => vt = true,
                    _ => winner += 1,
                }
            }
            for award in emu_file.get_nominees() {
                match award {
                    Award::ViewingTip => vt = true,
                    _ => nominee += 1,
                }
            }
        }
        let font = job
            .sections
            .first()
            .map(|s| s.format.font_id.clone())
            .unwrap_or_default();
        for _ in 0..winner {
            job.append(
                " \u{f4cf}",
                0.0,
                egui::TextFormat::simple(font.clone(), GOLD_COLOR),
            );
        }
        for _ in 0..nominee {
            job.append(
                " \u{f4cf}",
                0.0,
                egui::TextFormat::simple(font.clone(), SILVER_COLOR),
            );
        }

        let mut x = if winner > 0 || nominee > 0 {
            20.0
        } else {
            10.0
        };
        let mut images = Vec::new();
        for _ in 0..cdc {
            images.push((heart_id, x));
            x += 12.0;
        }
        if vt {
            images.push((star_id, x + 12.0));
        }
        images
    })
}

fn spawn_toast(
    mut state: ResMut<HudState>,
    time: Res<Time>,
    mut reader: MessageReader<SetHudText>,
) {
    for msg in reader.read() {
        info!("MSG: {}", msg.text);
        state.hud.set_text(
            msg.location,
            &msg.text,
            time.elapsed_secs(),
            msg.delay,
            msg.duration,
        );
    }
}

fn open_fuzzy_list(mut state: ResMut<HudState>, mut reader: MessageReader<ShowFuzzyList>) {
    for msg in reader.read() {
        state.picker.open(
            msg.id,
            msg.source.clone(),
            msg.prompt.as_deref(),
            msg.selected,
            &msg.title,
        );
    }
}

fn sync_ui_state(hud: Res<HudState>, mut ui: ResMut<UiState>) {
    ui.set_if_neq(UiState {
        modal: hud.picker.is_open() || hud.open_dialogs > 0,
    });
}

fn setup_ui_camera(mut commands: Commands, headless: Option<Res<HeadlessTarget>>, args: Res<Args>) {
    // Camera for full res UI on top of screen.
    let mut camera = commands.spawn((
        Camera2d,
        Camera {
            order: 1,
            clear_color: ClearColorConfig::None,
            ..default()
        },
        camera_target(headless.as_deref()),
        RenderLayers::layer(2),
    ));
    // In DJ mode the cue window's camera hosts egui instead (see `crate::dj`).
    if !crate::dj::enabled(&args) {
        camera.insert(PrimaryEguiContext);
    }
}

impl Plugin for EguiUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(EguiPlugin::default());
        // `EguiPlugin::build` has already run and inserted the resource, so this
        // lands before any camera is spawned in `Startup`.
        app.world_mut()
            .resource_mut::<EguiGlobalSettings>()
            .auto_create_primary_context = false;
        app.add_systems(Startup, load_font)
            .add_systems(
                EguiPrimaryContextPass,
                (
                    setup_egui,
                    update_ui.run_if(not(resource_exists::<HeadlessTarget>)),
                )
                    .chain(),
            )
            .add_message::<SetHudText>()
            .add_message::<ShowFuzzyList>()
            .add_message::<FuzzyListSelect>()
            .add_systems(Startup, setup_ui_camera)
            .add_systems(PreUpdate, sync_ui_state)
            .add_systems(
                Update,
                (
                    spawn_toast.run_if(on_message::<SetHudText>),
                    open_fuzzy_list.run_if(on_message::<ShowFuzzyList>),
                ),
            )
            .insert_resource(HudState::default())
            .insert_resource(UiState::default());
    }
}
