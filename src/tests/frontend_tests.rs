use bevy::camera::RenderTarget;
use bevy::ecs::system::RunSystemOnce;
use clap::Parser;

use super::*;
use crate::fake_backend::{FakeBackend, solid_frame};

const TARGET: UVec2 = UVec2::new(1280, 720);

/// Everything the frontend systems read, headless: no window, no audio.
fn world() -> World {
    let mut world = World::new();
    world.init_resource::<Assets<Image>>();
    world.init_resource::<AppSettings>();
    world.init_resource::<RenderSettings>();
    world.init_resource::<ButtonInput<KeyCode>>();
    world.init_resource::<ButtonInput<MouseButton>>();
    world.init_resource::<AccumulatedMouseMotion>();
    world.init_resource::<Time>();
    world.init_resource::<Messages<SetHudText>>();
    world.init_resource::<UiState>();
    world.init_resource::<HideMouse>();
    world.insert_resource(HeadlessTarget {
        image: Handle::default(),
        size: TARGET,
    });
    world
}

/// Swaps the offscreen target for a 1280x720 window with the cursor at `cursor`.
fn with_window(world: &mut World, cursor: Option<Vec2>) {
    world.remove_resource::<HeadlessTarget>();
    let mut window = Window::default();
    window.set_cursor_position(cursor);
    world.spawn((window, PrimaryWindow));
}

/// Spawns view `index`, running a fake core the caller keeps a handle on.
fn spawn(world: &mut World, index: usize, cell: Option<GridCell>) -> (Entity, FakeBackend) {
    let core = FakeBackend::default();
    let entity = spawn_emulator(world, false, None, false, index, cell);
    let mut emu = world.get_mut::<Emulator>(entity).unwrap();
    emu.core = core.boxed();
    emu.run_next = false;
    (entity, core)
}

/// Spawns a `cols`x`rows` grid of views.
fn spawn_grid(world: &mut World, cols: u32, rows: u32) -> Vec<(Entity, FakeBackend)> {
    grid_cells(cols, rows)
        .into_iter()
        .enumerate()
        .map(|(i, cell)| spawn(world, i, Some(cell)))
        .collect()
}

fn run(world: &mut World) {
    world.run_system_once(run_frontend).unwrap();
}

fn layout(world: &mut World) {
    world.run_system_once(update_view_rects).unwrap();
}

fn emu(world: &World, entity: Entity) -> &Emulator {
    world.get::<Emulator>(entity).unwrap()
}

fn emu_mut(world: &mut World, entity: Entity) -> Mut<'_, Emulator> {
    world.get_mut::<Emulator>(entity).unwrap()
}

fn view(world: &World, entity: Entity) -> ViewRect {
    world.get::<PostProcess>(entity).unwrap().view
}

fn settings(world: &mut World) -> Mut<'_, AppSettings> {
    world.resource_mut::<AppSettings>()
}

fn advance(world: &mut World, ms: u64) {
    world
        .resource_mut::<Time>()
        .advance_by(Duration::from_millis(ms));
}

fn hud(world: &mut World) -> Vec<SetHudText> {
    world
        .resource_mut::<Messages<SetHudText>>()
        .drain()
        .collect()
}

/// The bytes of the texture `entity` draws from.
fn texture(world: &World, entity: Entity) -> Vec<u8> {
    let handle = &emu(world, entity).image;
    let image = world.resource::<Assets<Image>>().get(handle).unwrap();
    image.data.clone().unwrap()
}

fn meta(world: &World, key: &str) -> Option<String> {
    world
        .resource::<AppSettings>()
        .system
        .meta_mut()
        .get(key)
        .cloned()
}

fn monitor(width: u32, height: u32, mhz: Option<u32>) -> Monitor {
    Monitor {
        name: None,
        physical_width: width,
        physical_height: height,
        physical_position: IVec2::ZERO,
        refresh_rate_millihertz: mhz,
        scale_factor: 1.0,
        video_modes: Vec::new(),
    }
}

fn args(extra: &[&str]) -> Args {
    Args::parse_from(std::iter::once(&"demarc").chain(extra))
}

/// A post-process view over `rect` of the screen.
fn post_process(source: Handle<Image>, position: UVec2, size: UVec2) -> PostProcess {
    PostProcess {
        source,
        aspect: 0.0,
        aspect_tweak: 1.0,
        used: UVec2::ZERO,
        view: ViewRect {
            position,
            size,
            active: true,
        },
        alpha: 1.0,
        raw: false,
    }
}

#[test]
fn grid_cells_run_left_to_right_then_top_to_bottom() {
    let cells = grid_cells(3, 2);
    assert_eq!(cells.len(), 6);
    assert_eq!(cells[0].offset, Vec2::ZERO);
    assert_eq!(cells[2].offset, Vec2::new(2.0 / 3.0, 0.0));
    assert_eq!(cells[3].offset, Vec2::new(0.0, 0.5));
    for cell in &cells {
        assert_eq!(cell.size, Vec2::new(1.0 / 3.0, 0.5));
    }
}

#[test]
fn layout_uses_the_window_and_falls_back_to_the_headless_target() {
    let window = Window::default();
    let headless = HeadlessTarget {
        image: Handle::default(),
        size: UVec2::new(320, 200),
    };
    assert_eq!(
        screen_size(Some(&window), Some(&headless)),
        Some(window.physical_size())
    );
    assert_eq!(screen_size(None, Some(&headless)), Some(headless.size));
    assert_eq!(screen_size(None, None), None);
}

#[test]
fn the_cursor_is_reported_in_physical_pixels() {
    let mut window = Window::default();
    assert_eq!(cursor_pos(None), None);
    assert_eq!(cursor_pos(Some(&window)), None);

    window.resolution.set_scale_factor_override(Some(2.0));
    window.set_cursor_position(Some(Vec2::new(10.0, 20.0)));
    assert_eq!(cursor_pos(Some(&window)), Some(Vec2::new(20.0, 40.0)));
}

#[test]
fn setup_spawns_one_fullscreen_emulator_by_default() {
    let mut world = world();
    world.insert_resource(args(&[]));

    setup_frontend(&mut world);

    let mut emus = world.query::<(&Emulator, &EmuView, Has<GridCell>)>();
    let found: Vec<_> = emus
        .iter(&world)
        .map(|(emu, view, cell)| (view.index, cell, emu.run_next))
        .collect();
    assert_eq!(found, [(0, false, true)]);
    assert!(world.resource::<HideMouse>().0);

    let mut cameras = world.query_filtered::<&RenderTarget, With<EmuCamera>>();
    let target = cameras.single(&world).unwrap();
    assert!(matches!(target, RenderTarget::Image(_)));
}

#[test]
fn setup_spawns_one_emulator_per_grid_cell() {
    let mut world = world();
    world.insert_resource(args(&["--grid=3x2", "--max-time=30"]));

    setup_frontend(&mut world);

    let mut emus = world.query::<(&Emulator, &EmuView, &GridCell)>();
    let mut indices: Vec<_> = emus.iter(&world).map(|(_, view, _)| view.index).collect();
    indices.sort();
    assert_eq!(indices, [0, 1, 2, 3, 4, 5]);
    assert!(
        emus.iter(&world)
            .all(|(emu, ..)| emu.max_time == Some(30) && emu.run_next)
    );
    // The pointer picks the focused view in a grid, so it stays visible.
    assert!(!world.resource::<HideMouse>().0);
}

#[test]
fn select_keeps_the_first_file_from_loading() {
    let mut world = world();
    world.insert_resource(args(&["--select"]));

    setup_frontend(&mut world);

    let mut emus = world.query::<&Emulator>();
    assert!(emus.iter(&world).all(|emu| !emu.run_next));
}

#[test]
fn a_lone_view_fills_the_screen() {
    let mut world = world();
    let (entity, _) = spawn(&mut world, 0, None);

    layout(&mut world);

    assert_eq!(
        view(&world, entity).rect(),
        Some(URect::from_corners(UVec2::ZERO, TARGET))
    );
    assert_eq!(settings(&mut world).mouse_index, None);
}

/// 1281 does not halve: the shared edge rounds one way for both neighbours.
#[test]
fn grid_views_tile_an_odd_sized_screen_exactly() {
    let mut world = world();
    world.resource_mut::<HeadlessTarget>().size = UVec2::new(1281, 721);
    let views = spawn_grid(&mut world, 2, 2);

    layout(&mut world);

    let rects: Vec<_> = views
        .iter()
        .map(|(entity, _)| view(&world, *entity))
        .map(|v| (v.position, v.size))
        .collect();
    assert_eq!(
        rects,
        [
            (UVec2::new(0, 0), UVec2::new(641, 361)),
            (UVec2::new(641, 0), UVec2::new(640, 361)),
            (UVec2::new(0, 361), UVec2::new(641, 360)),
            (UVec2::new(641, 361), UVec2::new(640, 360)),
        ]
    );
}

#[test]
fn a_maximized_view_takes_the_screen_and_hides_the_rest() {
    let mut world = world();
    let views = spawn_grid(&mut world, 2, 2);
    settings(&mut world).maximized = true;
    settings(&mut world).current_emu = 3;

    layout(&mut world);

    for (i, (entity, _)) in views.iter().enumerate() {
        let rect = view(&world, *entity).rect();
        if i == 3 {
            assert_eq!(rect, Some(URect::from_corners(UVec2::ZERO, TARGET)));
        } else {
            assert_eq!(rect, None, "view {i} is covered");
        }
    }
}

#[test]
fn the_cursor_picks_the_grid_view_under_it() {
    let mut world = world();
    with_window(&mut world, Some(Vec2::new(900.0, 100.0)));
    spawn_grid(&mut world, 2, 2);

    layout(&mut world);
    assert_eq!(settings(&mut world).mouse_index, Some(1));

    // The cross fade spare is not something to point at.
    let mut emus = world.query::<&mut Emulator>();
    for mut emu in emus.iter_mut(&mut world) {
        emu.is_crossfade = true;
    }
    layout(&mut world);
    assert_eq!(settings(&mut world).mouse_index, None);
}

#[test]
fn headless_there_is_no_view_under_the_cursor() {
    let mut world = world();
    spawn_grid(&mut world, 2, 2);

    layout(&mut world);

    assert_eq!(settings(&mut world).mouse_index, None);
}

#[test]
fn the_outline_sits_inside_its_cell() {
    let window = Vec2::new(800.0, 600.0);
    let cells = grid_cells(2, 2);

    // Top-left cell: left of and above the centred origin, y pointing up.
    assert_eq!(
        outline_rect(window, Some(&cells[0])),
        (Vec2::new(-200.0, 150.0), Vec2::new(396.0, 296.0))
    );
    assert_eq!(
        outline_rect(window, Some(&cells[3])),
        (Vec2::new(200.0, -150.0), Vec2::new(396.0, 296.0))
    );
    assert_eq!(
        outline_rect(window, None),
        (Vec2::ZERO, Vec2::new(796.0, 596.0))
    );
    // Too small to inset: nothing, rather than a negative size.
    assert_eq!(outline_rect(Vec2::splat(2.0), None).1, Vec2::ZERO);
}

#[test]
fn a_headless_run_takes_its_shape_from_the_target() {
    let mut world = world();

    world.run_system_once(detect_screen).unwrap();

    assert_eq!(meta(&world, META_WIDESCREEN).as_deref(), Some("true"));
    assert_eq!(meta(&world, META_REFRESH), None);
}

#[test]
fn the_monitor_sets_widescreen_and_refresh_rate() {
    let mut world = world();
    world.remove_resource::<HeadlessTarget>();
    world.spawn(monitor(1024, 768, Some(59_940)));

    world.run_system_once(detect_screen).unwrap();

    assert_eq!(meta(&world, META_WIDESCREEN).as_deref(), Some("false"));
    assert_eq!(meta(&world, META_REFRESH).as_deref(), Some("60"));
}

#[test]
fn the_primary_monitor_wins_over_the_first() {
    let mut world = world();
    world.remove_resource::<HeadlessTarget>();
    world.spawn(monitor(1024, 768, Some(75_000)));
    world.spawn((monitor(2560, 1440, Some(144_000)), PrimaryMonitor));

    world.run_system_once(detect_screen).unwrap();

    assert_eq!(meta(&world, META_WIDESCREEN).as_deref(), Some("true"));
    assert_eq!(meta(&world, META_REFRESH).as_deref(), Some("144"));
}

#[test]
fn screen_meta_given_on_the_command_line_is_kept() {
    let mut world = world();
    world.remove_resource::<HeadlessTarget>();
    world.spawn(monitor(1920, 1080, Some(60_000)));
    let system = settings(&mut world).system.clone();
    system.set_meta(META_WIDESCREEN, "false".into());
    system.set_meta(META_REFRESH, "50".into());

    world.run_system_once(detect_screen).unwrap();

    assert_eq!(meta(&world, META_WIDESCREEN).as_deref(), Some("false"));
    assert_eq!(meta(&world, META_REFRESH).as_deref(), Some("50"));
}

#[test]
fn the_cursor_maps_onto_the_stretched_frame() {
    let images = Assets::<Image>::default();
    let pp = post_process(Handle::default(), UVec2::new(100, 50), UVec2::new(800, 600));
    let uv = |pos| cursor_frame_uv(pos, &pp, &images, ScaleMode::Stretch);

    assert_eq!(uv(Some(Vec2::new(500.0, 350.0))), Some(Vec2::splat(0.5)));
    assert_eq!(uv(Some(Vec2::new(100.0, 50.0))), Some(Vec2::ZERO));
    assert_eq!(uv(Some(Vec2::new(99.0, 350.0))), None, "left of the view");
    assert_eq!(uv(None), None);
}

/// A square frame in an 800x600 view: 100 pixels of bar on either side.
#[test]
fn the_cursor_is_off_the_frame_in_the_letterbox_bars() {
    let mut images = Assets::<Image>::default();
    let source = images.add(Image::new_fill(
        Extent3d {
            width: 100,
            height: 100,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0; 4],
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::default(),
    ));
    let pp = post_process(source, UVec2::ZERO, UVec2::new(800, 600));
    let uv = |x, y| cursor_frame_uv(Some(Vec2::new(x, y)), &pp, &images, ScaleMode::Fit);

    assert_eq!(uv(400.0, 300.0), Some(Vec2::splat(0.5)));
    assert_eq!(uv(250.0, 300.0), Some(Vec2::new(0.25, 0.5)));
    assert_eq!(uv(50.0, 300.0), None);
    assert_eq!(uv(750.0, 300.0), None);
}

#[test]
fn a_view_without_a_rectangle_has_no_cursor() {
    let images = Assets::<Image>::default();
    let mut pp = post_process(Handle::default(), UVec2::ZERO, UVec2::ZERO);
    let pos = Some(Vec2::new(1.0, 1.0));
    assert_eq!(cursor_frame_uv(pos, &pp, &images, ScaleMode::Stretch), None);

    pp.view.size = UVec2::new(800, 600);
    pp.view.active = false;
    assert_eq!(cursor_frame_uv(pos, &pp, &images, ScaleMode::Stretch), None);
}

#[test]
fn blit_copies_a_frame_into_the_top_left() {
    let frame = VideoFrame {
        width: 2,
        height: 2,
        pixels: Arc::new(vec![1, 2, 3, 4]),
    };
    let mut dst = vec![0u8; 3 * 3 * 4];

    blit_frame(&mut dst, 3, 3, &frame);

    let pixels: Vec<u32> = dst
        .chunks_exact(4)
        .map(|p| u32::from_ne_bytes(p.try_into().unwrap()))
        .collect();
    assert_eq!(pixels, [1, 2, 0, 3, 4, 0, 0, 0, 0]);
}

#[test]
fn blit_clips_a_frame_larger_than_the_texture() {
    let frame = VideoFrame {
        width: 3,
        height: 3,
        pixels: Arc::new((1..=9).collect()),
    };
    let mut dst = vec![0u8; 2 * 2 * 4];

    blit_frame(&mut dst, 2, 2, &frame);

    let pixels: Vec<u32> = dst
        .chunks_exact(4)
        .map(|p| u32::from_ne_bytes(p.try_into().unwrap()))
        .collect();
    assert_eq!(pixels, [1, 2, 4, 5]);
}

/// The first frame of a new size only resizes the texture; the next fills it.
#[test]
fn the_texture_follows_the_cores_frame_size() {
    let mut world = world();
    let (entity, core) = spawn(&mut world, 0, None);
    core.lock().frame = solid_frame(4, 2, 0x11223344);

    run(&mut world);
    assert_eq!(
        (emu(&world, entity).width, emu(&world, entity).height),
        (4, 2)
    );
    assert_eq!(texture(&world, entity), vec![0u8; 4 * 2 * 4]);

    run(&mut world);
    assert_eq!(
        texture(&world, entity),
        crate::backend::frame_bytes(&[0x11223344; 8])
    );
    assert!(
        emu(&world, entity).sink.stream.is_none(),
        "headless is silent"
    );
}

#[test]
fn an_unchanged_frame_is_not_copied_again() {
    let mut world = world();
    let (entity, core) = spawn(&mut world, 0, None);
    core.lock().frame = solid_frame(4, 2, 0x11223344);
    run(&mut world);
    run(&mut world);

    let handle = emu(&world, entity).image.clone();
    let mut images = world.resource_mut::<Assets<Image>>();
    images
        .get_mut(&handle)
        .unwrap()
        .data
        .as_mut()
        .unwrap()
        .fill(0);
    run(&mut world);

    assert_eq!(texture(&world, entity), vec![0u8; 4 * 2 * 4]);
}

#[test]
fn the_picture_lags_the_core_by_the_frame_delay() {
    let mut world = world();
    let (entity, core) = spawn(&mut world, 0, None);
    core.lock().frame = solid_frame(4, 2, 1);
    for _ in 0..=FRAME_DELAY {
        run(&mut world);
    }
    let old = texture(&world, entity);

    core.lock().frame = solid_frame(4, 2, 2);
    for _ in 0..FRAME_DELAY {
        run(&mut world);
        assert_eq!(texture(&world, entity), old);
    }
    run(&mut world);
    assert_eq!(
        texture(&world, entity),
        crate::backend::frame_bytes(&[2; 8])
    );
}

#[test]
fn aspect_and_used_area_reach_the_view() {
    let mut world = world();
    let (entity, core) = spawn(&mut world, 0, None);
    core.lock().frame = solid_frame(4, 2, 0);
    core.lock().aspect = 1.6;
    core.lock().used = Some((2, 1));

    run(&mut world);

    let pp = world.get::<PostProcess>(entity).unwrap();
    assert_eq!(pp.aspect, 1.6);
    assert_eq!(pp.used, UVec2::new(2, 1));
}

#[test]
fn an_emulator_without_a_texture_is_left_alone() {
    let mut world = world();
    let (entity, core) = spawn(&mut world, 0, None);
    let handle = emu(&world, entity).image.clone();
    world.resource_mut::<Assets<Image>>().remove(&handle);

    run(&mut world);

    assert_eq!(core.lock().runs, 0);
    assert!(core.lock().focus.is_empty());
}

#[test]
fn only_the_current_view_has_focus() {
    let mut world = world();
    let views = spawn_grid(&mut world, 2, 1);
    settings(&mut world).current_emu = 1;

    run(&mut world);

    assert_eq!(views[0].1.lock().focus, [ViewFocus::Visible]);
    assert_eq!(views[1].1.lock().focus, [ViewFocus::Focus]);
}

/// The cross fade spare is covered too, but has to keep running to fade in.
#[test]
fn views_under_a_maximized_one_are_invisible() {
    let mut world = world();
    let views = spawn_grid(&mut world, 2, 1);
    let (spare, spare_core) = spawn(&mut world, 2, None);
    emu_mut(&mut world, spare).is_crossfade = true;
    settings(&mut world).maximized = true;

    run(&mut world);

    assert_eq!(views[0].1.lock().focus, [ViewFocus::Focus]);
    assert_eq!(views[1].1.lock().focus, [ViewFocus::Invisible]);
    assert_eq!(spare_core.lock().focus, [ViewFocus::Visible]);
}

#[test]
fn max_time_asks_for_the_next_release() {
    let mut world = world();
    let (entity, _) = spawn(&mut world, 0, None);
    emu_mut(&mut world, entity).max_time = Some(10);

    advance(&mut world, 9_000);
    run(&mut world);
    assert!(!emu(&world, entity).run_next);

    advance(&mut world, 2_000);
    run(&mut world);
    assert!(emu(&world, entity).run_next);

    // Not again while that load is on its way.
    emu_mut(&mut world, entity).run_next = false;
    advance(&mut world, 1_000);
    run(&mut world);
    assert!(!emu(&world, entity).run_next);
}

/// Selecting a view restarts the wait, so it is not switched out from under
/// the click.
#[test]
fn max_time_waits_for_a_fresh_selection() {
    let mut world = world();
    let (entity, _) = spawn(&mut world, 0, None);
    emu_mut(&mut world, entity).max_time = Some(10);
    advance(&mut world, 11_000);
    settings(&mut world).select_box_drawn_at = 10.5;

    run(&mut world);

    assert!(!emu(&world, entity).run_next);
}

#[test]
fn the_cross_fade_spare_never_times_out() {
    let mut world = world();
    let (entity, _) = spawn(&mut world, 0, None);
    let mut spare = emu_mut(&mut world, entity);
    spare.is_crossfade = true;
    spare.max_time = Some(10);
    spare.idle_time = 100.0;
    spare.core = None;
    settings(&mut world).idle_timeout = 5;
    advance(&mut world, 11_000);

    run(&mut world);

    assert!(!emu(&world, entity).run_next);
}

#[test]
fn an_idle_emulator_moves_on_after_the_timeout() {
    let mut world = world();
    let (entity, _) = spawn(&mut world, 0, None);
    emu_mut(&mut world, entity).core = None;
    settings(&mut world).idle_timeout = 5;

    emu_mut(&mut world, entity).idle_time = 4.0;
    run(&mut world);
    assert!(!emu(&world, entity).run_next);

    emu_mut(&mut world, entity).idle_time = 6.0;
    run(&mut world);
    assert!(emu(&world, entity).run_next);
    assert_eq!(emu(&world, entity).idle_time, 0.0);
}

#[test]
fn tv_mode_times_out_after_twenty_seconds() {
    let mut world = world();
    let (entity, _) = spawn(&mut world, 0, None);
    emu_mut(&mut world, entity).core = None;

    emu_mut(&mut world, entity).idle_time = 21.0;
    run(&mut world);
    assert!(!emu(&world, entity).run_next, "no timeout unless asked for");

    emu_mut(&mut world, entity).idle_time = 21.0;
    settings(&mut world).tv_mode = true;
    run(&mut world);
    assert!(emu(&world, entity).run_next);
}

#[test]
fn input_goes_to_the_maximized_current_view_only() {
    let mut world = world();
    let views = spawn_grid(&mut world, 2, 1);
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::Space);

    run(&mut world);
    assert!(
        views[0].1.lock().keys.is_empty(),
        "a grid tile takes no keys"
    );

    settings(&mut world).maximized = true;
    run(&mut world);
    assert_eq!(views[0].1.lock().keys.len(), 1);
    assert!(views[1].1.lock().keys.is_empty());
}

#[test]
fn all_emus_feeds_every_view() {
    let mut world = world();
    let views = spawn_grid(&mut world, 2, 1);
    settings(&mut world).maximized = true;
    settings(&mut world).all_emus = true;

    run(&mut world);

    assert_eq!(views[0].1.lock().buttons.len(), 1);
    assert_eq!(views[1].1.lock().buttons.len(), 1);
}

#[test]
fn hotkey_modifiers_and_dialogs_keep_input_from_the_core() {
    let mut world = world();
    let (_, core) = spawn(&mut world, 0, None);
    settings(&mut world).maximized = true;

    world.resource_mut::<UiState>().modal = true;
    run(&mut world);
    world.resource_mut::<UiState>().modal = false;

    for key in [KeyCode::AltRight, KeyCode::ControlRight] {
        world.resource_mut::<ButtonInput<KeyCode>>().press(key);
        run(&mut world);
        world.resource_mut::<ButtonInput<KeyCode>>().reset_all();
    }
    assert!(core.lock().buttons.is_empty());

    run(&mut world);
    assert_eq!(core.lock().buttons.len(), 1);
}

#[test]
fn a_click_selects_the_view_and_shows_its_info() {
    let mut world = world();
    let views = spawn_grid(&mut world, 2, 1);
    emu_mut(&mut world, views[1].0).emu_file.game_info.title = "Second";
    settings(&mut world).mouse_index = Some(1);
    advance(&mut world, 1_000);
    world
        .resource_mut::<ButtonInput<MouseButton>>()
        .press(MouseButton::Left);

    run(&mut world);

    assert_eq!(settings(&mut world).current_emu, 1);
    assert_eq!(settings(&mut world).select_box_drawn_at, 1.0);
    assert!(!settings(&mut world).maximized);
    let hud = hud(&mut world);
    assert_eq!(hud.len(), 1);
    assert_eq!(hud[0].location, HudLocation::InfoText);
    assert!(hud[0].text.starts_with("\"Second\""));
}

#[test]
fn a_double_click_toggles_maximized() {
    let mut world = world();
    spawn_grid(&mut world, 2, 1);
    settings(&mut world).mouse_index = Some(1);
    let click = |world: &mut World, ms| {
        advance(world, ms);
        let mut buttons = world.resource_mut::<ButtonInput<MouseButton>>();
        buttons.reset_all();
        buttons.press(MouseButton::Left);
        run(world);
    };

    click(&mut world, 1_000);
    click(&mut world, 200);
    assert!(settings(&mut world).maximized);

    click(&mut world, 1_000);
    assert!(
        settings(&mut world).maximized,
        "too slow to be a double click"
    );

    click(&mut world, 200);
    assert!(!settings(&mut world).maximized);
}

/// Over a maximized view the pointer names no grid cell, so the selection
/// stays where it is.
#[test]
fn a_click_on_a_maximized_view_keeps_the_selection() {
    let mut world = world();
    spawn_grid(&mut world, 2, 1);
    settings(&mut world).current_emu = 1;
    settings(&mut world).maximized = true;
    with_window(&mut world, Some(Vec2::new(10.0, 10.0)));
    // Keeps the audio device closed now that there is a window.
    settings(&mut world).speed_test = true;
    layout(&mut world);
    advance(&mut world, 1_000);
    world
        .resource_mut::<ButtonInput<MouseButton>>()
        .press(MouseButton::Left);

    run(&mut world);

    assert_eq!(settings(&mut world).current_emu, 1);
    assert!(hud(&mut world).is_empty());
}

#[test]
fn a_finished_skip_clears_the_warp_indicator() {
    let mut world = world();
    let (entity, core) = spawn(&mut world, 0, None);
    emu_mut(&mut world, entity).skip(100);
    core.lock().state = crate::backend::STATE_SKIPPING;

    run(&mut world);
    assert!(hud(&mut world).is_empty());

    core.lock().state = 0;
    run(&mut world);
    let hud = hud(&mut world);
    assert_eq!(hud.len(), 1);
    assert_eq!(hud[0].location, HudLocation::TopRight);
    assert_eq!(hud[0].text, "");
}
