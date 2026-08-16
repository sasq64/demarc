//! Opt-in profiling instrumentation, compiled only under `--features profile`.
//!
//! Two separate things, both absent from a normal build:
//!
//! * **Span traces.** `bevy/trace_chrome` makes Bevy emit a tracing span around
//!   every system run, render pass and asset upload, and installs a
//!   `tracing-chrome` layer that writes them to `trace-<timestamp>.json` (or
//!   `$TRACE_CHROME`). That layer lives inside [`LogPlugin`], which `main`
//!   normally disables in favour of its own subscriber — [`log_plugin`] rebuilds
//!   it with the same writer so the cores stay silenced.
//!
//! * **A change audit.** Frame timing plus a per-second count of *how many
//!   entities changed which components*. Every entry there costs something
//!   downstream — text re-layout, UI layout, transform propagation, or a
//!   re-extract/re-upload into the render world — so a component whose count
//!   tracks the frame count is a system rewriting it whether or not the value
//!   actually moved.
//!
//! ```text
//! cargo run --profile profiling --features profile -- demos/rebels.adf
//! ```
//!
//! Open the resulting `trace-*.json` in <https://ui.perfetto.dev> or
//! `chrome://tracing`.

use std::time::Duration;

use bevy::diagnostic::{
    EntityCountDiagnosticsPlugin, FrameTimeDiagnosticsPlugin, LogDiagnosticsPlugin,
    SystemInformationDiagnosticsPlugin,
};
use bevy::ecs::system::SystemParam;
use bevy::log::{BoxedFmtLayer, DEFAULT_FILTER, Level, LogPlugin};
use bevy::prelude::*;
use bevy::text::TextLayoutInfo;
use bevy::ui::ComputedNode;

use crate::post_process::{BorderScissor, PostProcess, PostProcessUniform};
use crate::{AppSettings, RenderSettings};

/// The dup of the original stdout, stashed for the `fmt_layer` hook below.
/// [`LogPlugin::fmt_layer`] is a plain `fn` pointer, not a closure, so the
/// writer can't be captured — it has to travel through a static.
#[cfg(unix)]
static LOG_WRITER: std::sync::OnceLock<crate::FdWriter> = std::sync::OnceLock::new();

/// What `main` hands to [`log_plugin`]. Only Unix redirects the cores' stdout
/// and so has a writer to pass along; elsewhere the log goes to stderr and the
/// parameter is always `None`.
#[cfg(unix)]
pub type LogWriter = crate::FdWriter;
#[cfg(not(unix))]
pub type LogWriter = std::io::Stderr;

/// The [`LogPlugin`] to use in place of `main`'s hand-built subscriber, so the
/// chrome-trace layer is attached to the spans.
///
/// `writer` is the saved stdout from `silence_stdout`; without it the log would
/// go to the `/dev/null` we pointed stderr at.
pub fn log_plugin(#[allow(unused)] writer: Option<LogWriter>) -> LogPlugin {
    #[cfg(unix)]
    if let Some(writer) = writer {
        let _ = LOG_WRITER.set(writer);
    }
    LogPlugin {
        // Bevy's system spans are emitted at INFO, so the global default has to
        // be INFO for the chrome layer to record them at all. The app's own
        // logs are turned back down to `warn` to keep the terminal readable —
        // the trace file is the output that matters here.
        level: Level::INFO,
        filter: format!("{DEFAULT_FILTER}demarc=warn,demarc::profiling=info"),
        fmt_layer: |_app| {
            #[cfg(unix)]
            if let Some(writer) = LOG_WRITER.get().copied() {
                return Some(Box::new(
                    bevy::log::tracing_subscriber::fmt::Layer::default()
                        .with_writer(move || writer)
                        .compact(),
                ) as BoxedFmtLayer);
            }
            None
        },
        ..default()
    }
}

pub struct ProfilingPlugin;

impl Plugin for ProfilingPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            FrameTimeDiagnosticsPlugin::default(),
            EntityCountDiagnosticsPlugin::default(),
            SystemInformationDiagnosticsPlugin,
            LogDiagnosticsPlugin {
                wait_duration: REPORT_INTERVAL,
                ..default()
            },
        ))
        .init_resource::<ChangeAudit>()
        // `Last`, so every `Update`/`PostUpdate` write has landed but the ticks
        // haven't been cleared for the next frame yet.
        .add_systems(Last, audit_changes);
    }
}

const REPORT_INTERVAL: Duration = Duration::from_secs(1);

/// The components worth watching. Each one is something whose change triggers
/// real downstream work:
///
/// * `Text`/`TextFont`/`TextLayout` — re-runs text layout and glyph rasterization.
/// * `TextColor`/`BackgroundColor` — re-extracts the UI node into the render world.
/// * `Node` — re-runs the UI layout pass (`ComputedNode`/`TextLayoutInfo` are the
///   outputs of those passes, listed so a feedback loop is visible as both sides).
/// * `Transform` — re-runs transform propagation into `GlobalTransform`.
/// * `Camera` — a viewport/`is_active` write rebuilds render state.
/// * `PostProcess`/`PostProcessUniform`/`BorderScissor` — re-extract plus a
///   uniform buffer write per camera.
#[derive(SystemParam)]
struct Changed<'w, 's> {
    text: Query<'w, 's, (), bevy::prelude::Changed<Text>>,
    text_font: Query<'w, 's, (), bevy::prelude::Changed<TextFont>>,
    text_layout: Query<'w, 's, (), bevy::prelude::Changed<TextLayout>>,
    text_color: Query<'w, 's, (), bevy::prelude::Changed<TextColor>>,
    text_layout_info: Query<'w, 's, (), bevy::prelude::Changed<TextLayoutInfo>>,
    node: Query<'w, 's, (), bevy::prelude::Changed<Node>>,
    computed_node: Query<'w, 's, (), bevy::prelude::Changed<ComputedNode>>,
    background: Query<'w, 's, (), bevy::prelude::Changed<BackgroundColor>>,
    transform: Query<'w, 's, (), bevy::prelude::Changed<Transform>>,
    global_transform: Query<'w, 's, (), bevy::prelude::Changed<GlobalTransform>>,
    visibility: Query<'w, 's, (), bevy::prelude::Changed<Visibility>>,
    camera: Query<'w, 's, (), bevy::prelude::Changed<Camera>>,
    post_process: Query<'w, 's, (), bevy::prelude::Changed<PostProcess>>,
    post_process_uniform: Query<'w, 's, (), bevy::prelude::Changed<PostProcessUniform>>,
    border_scissor: Query<'w, 's, (), bevy::prelude::Changed<BorderScissor>>,
}

impl Changed<'_, '_> {
    /// `(label, entities changed this frame)` for every watched component.
    fn counts(&self) -> [(&'static str, usize); 15] {
        [
            ("Text", self.text.iter().count()),
            ("TextFont", self.text_font.iter().count()),
            ("TextLayout", self.text_layout.iter().count()),
            ("TextColor", self.text_color.iter().count()),
            ("TextLayoutInfo", self.text_layout_info.iter().count()),
            ("Node", self.node.iter().count()),
            ("ComputedNode", self.computed_node.iter().count()),
            ("BackgroundColor", self.background.iter().count()),
            ("Transform", self.transform.iter().count()),
            ("GlobalTransform", self.global_transform.iter().count()),
            ("Visibility", self.visibility.iter().count()),
            ("Camera", self.camera.iter().count()),
            ("PostProcess", self.post_process.iter().count()),
            (
                "PostProcessUniform",
                self.post_process_uniform.iter().count(),
            ),
            ("BorderScissor", self.border_scissor.iter().count()),
        ]
    }
}

#[derive(Resource)]
struct ChangeAudit {
    elapsed: Duration,
    frames: u32,
    /// Running totals, parallel to [`Changed::counts`].
    counts: [usize; 15],
    /// Frames on which the resource was written at all (`ResMut` deref).
    settings_frames: u32,
    render_settings_frames: u32,
    /// `AssetEvent::Modified` for images — one per texture re-upload.
    image_modified: usize,
}

impl Default for ChangeAudit {
    fn default() -> Self {
        Self {
            elapsed: Duration::ZERO,
            frames: 0,
            counts: [0; 15],
            settings_frames: 0,
            render_settings_frames: 0,
            image_modified: 0,
        }
    }
}

/// Accumulates the per-frame change counts and logs a summary once a second.
fn audit_changes(
    changed: Changed,
    mut audit: ResMut<ChangeAudit>,
    time: Res<Time>,
    settings: Res<AppSettings>,
    render_settings: Res<RenderSettings>,
    mut image_events: MessageReader<AssetEvent<Image>>,
) {
    audit.frames += 1;
    audit.elapsed += time.delta();
    for (i, (_, count)) in changed.counts().iter().enumerate() {
        audit.counts[i] += count;
    }
    if settings.is_changed() {
        audit.settings_frames += 1;
    }
    if render_settings.is_changed() {
        audit.render_settings_frames += 1;
    }
    audit.image_modified += image_events
        .read()
        .filter(|e| matches!(e, AssetEvent::Modified { .. }))
        .count();

    if audit.elapsed < REPORT_INTERVAL {
        return;
    }

    let frames = audit.frames.max(1) as f32;
    let labels = changed.counts();
    // Busiest first; a component sitting at ~1.00/frame per entity is the
    // signature of an unconditional write.
    let mut rows: Vec<(&str, usize)> = labels
        .iter()
        .enumerate()
        .map(|(i, (label, _))| (*label, audit.counts[i]))
        .filter(|(_, total)| *total > 0)
        .collect();
    rows.sort_by_key(|(_, total)| std::cmp::Reverse(*total));

    let changes = rows
        .iter()
        .map(|(label, total)| format!("{label} {total} ({:.2}/f)", *total as f32 / frames))
        .collect::<Vec<_>>()
        .join(", ");
    info!(
        "change audit: {} frames in {:.2}s | Image uploads {} ({:.2}/f) | \
         AppSettings dirty {}/{} frames, RenderSettings {}/{} | {}",
        audit.frames,
        audit.elapsed.as_secs_f32(),
        audit.image_modified,
        audit.image_modified as f32 / frames,
        audit.settings_frames,
        audit.frames,
        audit.render_settings_frames,
        audit.frames,
        if changes.is_empty() {
            "no component changes".to_string()
        } else {
            changes
        },
    );

    *audit = ChangeAudit::default();
}
