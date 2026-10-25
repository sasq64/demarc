//! The frontend systems that drive loads into an [`Emulator`]; the load
//! itself is in `emulator::load`.

use std::time::Duration;

use bevy::prelude::*;

use crate::config::{AppSettings, GlobalSettings};
use crate::demarc_settings::DemarcSettings;
use crate::cross_fade::{CrossFade, Views, redirect_load};
use crate::emu_file::EmuFile;
use crate::emulator::{EmuState, Emulator, LoadStatus};
use crate::frontend::{EmuView, FrontendSet};
use crate::navigator::Navigator;
use crate::playlists::Playlists;
use crate::ui::{HudLocation, SetHudText};

/// One emulator finished a load this frame.
#[derive(Message)]
pub struct LoadFinished(pub Entity);

#[derive(Message)]
pub struct LoadFile {
    pub emu_file: EmuFile,
    pub target_emulator: Option<usize>,
}

pub struct LoadingPlugin;

pub fn load_file(
    mut emus: Views,
    mut reader: MessageReader<LoadFile>,
    settings: ResMut<AppSettings>,
    mut cross_fade: ResMut<CrossFade>,
    global: Res<GlobalSettings>,
) {
    for game in reader.read() {
        debug!("Got load file message");
        let over = settings.override_for_file(&game.emu_file);
        if let Some(o) = &over {
            debug!("Found override for {:?}: {o:?}", game.emu_file);
        }

        let emu_index = game.target_emulator.unwrap_or(settings.current_emu);
        let emu_index = redirect_load(&mut cross_fade, &mut emus, emu_index);

        for (_, mut emu, view, ..) in &mut emus {
            if view.index == emu_index {
                emu.start_load(&game.emu_file, over.as_ref(), &global);
            }
        }
    }
}

pub(crate) fn handle_loading(
    mut emus: Query<(Entity, &EmuView, &mut Emulator)>,
    mut settings: ResMut<AppSettings>,
    mut writer: MessageWriter<SetHudText>,
    mut load_writer: MessageWriter<LoadFile>,
    mut loaded: MessageWriter<LoadFinished>,
    mut navigator: ResMut<Navigator>,
    playlists: Res<Playlists>,
    demo_settings: Res<DemarcSettings>,
    time: Res<Time>,
) {
    for (entity, emuview, mut emu) in &mut emus.iter_mut() {
        let next = if let Some(launch) = navigator.current_launch.as_mut() {
            let d = if emu.run_next {
                1
            } else if emu.run_prev {
                -1
            } else {
                0
            };
            if d == 0 {
                None
            } else {
                launch.step(d, settings.tv_mode).map(|(file, index)| {
                    if let Some(index) = index {
                        settings.current_game = index as isize;
                    }
                    file
                })
            }
        } else {
            let db = settings.default_db();
            let flen = db.len() as isize;
            let d = if flen == 0 {
                0
            } else if emu.run_next && (settings.tv_mode || settings.current_game < flen - 1) {
                1
            } else if emu.run_prev && (settings.tv_mode || settings.current_game > 0) {
                -1
            } else {
                0
            };
            (d != 0).then(|| {
                settings.current_game = (settings.current_game + d + flen) % flen;
                db[settings.current_game as usize].clone()
            })
        };
        if let Some(emu_file) = next {
            emu.run_next = false;
            emu.run_prev = false;
            debug!("Send load file");
            load_writer.write(LoadFile {
                emu_file,
                target_emulator: Some(emuview.index),
            });
            continue;
        }

        let status = emu.update_load(&time, &settings.system);
        match status {
            LoadStatus::Idle | LoadStatus::Pending => {}
            LoadStatus::Done {
                title,
                result: Err(e),
            } => {
                let text = format!(
                    "Could not load {title}: {}",
                    crate::load_error::classify(&e).reason()
                );
                emu.state = EmuState::Stopped;

                if !settings.tv_mode {
                    emu.run_next = false;
                    emu.run_prev = false;
                    writer.write(SetHudText {
                        text,
                        delay: Duration::from_secs(0),
                        duration: Duration::from_secs(4),
                        location: HudLocation::Error,
                    });
                } else {
                    emu.run_next = true;
                }
                error!("{e:?}");
                continue;
            }
            LoadStatus::Done { result: Ok(()), .. } => {
                emu.favorite = playlists.is_favorite(&emu.emu_file);
                if settings.record {
                    emu.start_recording(&time, demo_settings.recording.options());
                }
                emu.run_next = false;
                emu.run_prev = false;
                loaded.write(LoadFinished(entity));
                emu.state = if emu.is_crossfade {
                    EmuState::PreDelay
                } else {
                    EmuState::InfoDelay
                };
            }
        }
        if matches!(emu.state, EmuState::InfoDelay) {
            emu.state = EmuState::Running;
            if settings.show_info && settings.maximized {
                writer.write_batch(emu.info_hud(
                    Duration::from_secs(settings.info_delay),
                    Duration::from_secs(settings.info_duration),
                ));
            }
        }
    }
}

impl Plugin for LoadingPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<LoadFinished>()
            .add_message::<LoadFile>()
            .add_systems(
                Update,
                (
                    handle_loading.in_set(FrontendSet::Loading),
                    load_file.run_if(on_message::<LoadFile>),
                ),
            );
    }
}

#[cfg(test)]
#[path = "tests/loading_tests.rs"]
mod tests;
