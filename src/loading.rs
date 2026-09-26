//! Loading a release into an [`Emulator`]: the asynchronous two-phase load
//! itself and the frontend system that drives it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use bevy::prelude::*;

use crate::config::AppSettings;
use crate::egui_ui::{HudLocation, SetHudText};
use crate::emu_file::{DOWNLOAD_COUNTER, EmuFile, FileSource, GameInfo, Override, UrlList};
use crate::emulator::{EmuState, Emulator, InputMode};
use crate::frontend::FrontendSet;
use crate::jobs::{Job, JobError, JobProgress, drop_on_pool};
use crate::newsys::{self, LoadResult, NewSys};
use crate::workfile::WorkFile;

/// One emulator finished a load this frame.
#[derive(Message)]
pub struct LoadFinished(pub Entity);

/// The two off-thread halves a load is made of, in the order they run.
enum LoadPhase {
    /// Resolves the download *and* unpacks it — see [`newsys::unpack_release`].
    Unpacking {
        job: Job<WorkFile>,
        over: Option<Override>,
    },
    /// Tears the old core down and builds the new one.
    Creating(Job<LoadResult>),
}

impl LoadPhase {
    fn cancel(&self) {
        match self {
            LoadPhase::Unpacking { job, .. } => job.cancel(),
            LoadPhase::Creating(job) => job.cancel(),
        }
    }
}

pub struct LoadingPlugin;

/// A load started by [`load_async`] whose job hasn't landed yet.
pub(crate) struct PendingLoad {
    /// What the entry is called, kept here because the job reports only a
    /// [`WorkFile`] and the entry itself is gone by the time it lands — and on
    /// failure there is nowhere else left to read the title from.
    info: GameInfo,
    phase: LoadPhase,
}

/// What [`Emulator::update_load`] found this frame.
pub(crate) enum LoadStatus {
    /// No load in flight.
    Idle,
    /// A download is still running; the previously loaded core, if any, keeps
    /// running meanwhile.
    Pending,
    /// The load finished this frame — `result` is `Ok` when the new core is
    /// live.
    ///
    /// `title` names the entry this was for. It is carried here because on
    /// failure there is nowhere else left to read it from:
    /// [`Emulator::work_file`] still describes whatever was loaded before.
    Done { title: String, result: Result<()> },
}

/// Begin loading `emu_file`, downloading it first if it is URL-backed.
///
/// Returns immediately. Downloading and unpacking run on the I/O pool, and so
/// does building the core out of what they produced
/// (`Emulator::start_create`) — the main thread only takes the finished
/// backend over. So the core currently running keeps running (and playing)
/// until the download is in.
///
/// Only the job is started here: cancelling a load already in flight, the
/// download counters and the emulator's own state are the caller's
/// ([`handle_loading`]).
pub fn load_async(emu_file: &EmuFile, over: Option<&Override>) -> PendingLoad {
    let name = if emu_file.game_info.title.is_empty() {
        "load"
    } else {
        emu_file.game_info.title
    }
    .to_string();

    // Resolution and unpacking both run off-thread; only what touches shared
    // state — system detection, conversion, core creation — is left for
    // `load_prepared` on the main thread. `NewSys` and the `WorkFile` it
    // builds own their meta, so the entry's borrowed pairs are copied into
    // `String`s here, at the one boundary where the file list hands work off.
    let meta: HashMap<String, String> = emu_file
        .meta
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    let mut source = emu_file.path.clone();
    // The one part of an override that has to happen before the transfer:
    // where the release comes from, or which of its downloads is the demo.
    if let Some(url) = over.and_then(|o| o.download_url) {
        source = FileSource::Url(UrlList::one(url));
    } else if let Some(name) = over.and_then(|o| o.download) {
        source.pick_download(name);
    }
    let job = Job::spawn(name, move |progress| {
        let path = source.resolve_with_progress(&|done, total| {
            progress.set_done(done);
            progress.set_total(total.unwrap_or(0));
        })?;
        // Unpacking has no useful byte count; flip back to indeterminate so
        // a progress bar doesn't sit at 100% for the rest of the job.
        progress.set_total(0);
        progress.set_done(0);
        newsys::unpack_release(path, &meta)
    });

    PendingLoad {
        info: emu_file.game_info,
        phase: LoadPhase::Unpacking {
            job,
            over: over.cloned(),
        },
    }
}

impl Emulator {
    /// Drive a [`load_async`] forward; call once per frame.
    ///
    /// Both halves of the load run on the job pool: the unpacked [`WorkFile`]
    /// the first one produces is handed straight to a second job that tears the
    /// old core down and builds the new one, and only the finished
    /// [`LoadResult`] is taken over here. The caller sees exactly the outcome
    /// the old synchronous `load` produced — just some frames later.
    pub fn update_load(&mut self, time: &Time, sys: &Arc<NewSys>) -> LoadStatus {
        let Some(pending) = self.pending_load.as_mut() else {
            return LoadStatus::Idle;
        };
        match &mut pending.phase {
            LoadPhase::Unpacking { job, .. } => {
                // `poll` hands the result over exactly once, so it has to be
                // kept here rather than re-read after the `take` below.
                let Some(resolved) = job.poll() else {
                    return LoadStatus::Pending;
                };
                let PendingLoad { info, phase } =
                    self.pending_load.take().expect("checked just above");
                let LoadPhase::Unpacking { over, .. } = phase else {
                    unreachable!("matched just above");
                };
                match resolved {
                    Ok(work_file) => {
                        self.start_create(sys, info, work_file, over);
                        LoadStatus::Pending
                    }
                    Err(err) => {
                        DOWNLOAD_COUNTER.ended();
                        LoadStatus::Done {
                            title: info.title.to_string(),
                            result: Err(Self::job_error(err)),
                        }
                        //self.failed_load(advance, info.title.to_string(), Self::job_error(err))
                    }
                }
            }
            LoadPhase::Creating(job) => {
                let Some(resolved) = job.poll() else {
                    return LoadStatus::Pending;
                };
                let PendingLoad { info, .. } =
                    self.pending_load.take().expect("checked just above");
                // Past the `poll` above the load is over one way or another --
                // landed, failed or cancelled -- so it stops counting here,
                // whichever of the branches below the outcome takes.
                DOWNLOAD_COUNTER.ended();
                let title = info.title.to_string();
                match resolved {
                    Ok(res) => {
                        self.finish_load(time, res, info);
                        LoadStatus::Done {
                            title,
                            result: Ok(()),
                        }
                    }
                    Err(err) => {
                        LoadStatus::Done {
                            title,
                            result: Err(Self::job_error(err)),
                        }
                        //self.failed_load(advance, title, Self::job_error(err)),
                    }
                }
            }
        }
    }

    /// Let go of the running core without blocking the main thread, for
    /// callers that are not about to build another one.
    ///
    /// Unlike [`start_create`](Self::start_create) nothing has to wait for the
    /// teardown to finish, so it is simply detached.
    pub(crate) fn drop_core_async(&mut self) {
        if let Some(core) = self.core.take() {
            drop_on_pool(core);
        }
    }

    /// Start the second half of a load. From here on this emulator has no core,
    /// and the job pool holds both the old one — to tear down — and everything
    /// needed to build the new one.
    ///
    /// The old core is taken before the new one is built, and let go of by the
    /// job before it starts on it: a backend may own something the machine only
    /// has one of, and the next one cannot take it until this one has let go.
    /// `musix`'s sc68 plugin is the case that bites — libsc68 has a
    /// process-wide init that the plugin claims per song, so a second SNDH
    /// loaded while the first is still alive fails to init and no plugin is
    /// found for the file — but libretro cores are widely non-reentrant in the
    /// same way.
    ///
    /// The cost is that a load which fails leaves nothing running rather than
    /// the previous entry; the frontend already draws that state (it skips an
    /// emulator with no core), and tv mode steps on to the next.
    fn start_create(
        &mut self,
        sys: &Arc<NewSys>,
        info: GameInfo,
        work_file: WorkFile,
        over: Option<Override>,
    ) {
        let old_core = self.core.take();
        let sys = Arc::clone(sys);
        let name = if info.title.is_empty() {
            "load"
        } else {
            info.title
        };
        let job = Job::spawn(name, move |_| {
            // Both of these block for long enough to be seen as a dropped frame
            // on the main thread: a core's `retro_deinit` joins its worker
            // thread, and building the next one runs `retro_load_game` — and
            // downloads the core itself when it is not cached yet.
            drop(old_core);
            sys.load_prepared(work_file, over.as_ref())
        });
        self.pending_load = Some(PendingLoad {
            info,
            phase: LoadPhase::Creating(job),
        });
    }

    /// Unwrap a [`JobError`] into the error the frontend reports.
    ///
    /// `Failed` is unwrapped rather than wrapped: `load_error::classify`
    /// downcasts along the error chain to tell a 404 from a dead mirror, and an
    /// extra layer on top would still work but buys nothing.
    fn job_error(err: JobError) -> anyhow::Error {
        match err {
            JobError::Failed(err) => err,
            JobError::Cancelled => anyhow::anyhow!("load cancelled"),
        }
    }

    /// Report a load that didn't happen, re-arming the advance it consumed.
    ///
    /// Restoring `run_next`/`run_prev` leaves the frontend where the old
    /// synchronous path left it on failure: still asking to move on, so tv mode
    /// steps past the broken entry, while an interactive session clears them
    /// itself and stops on the error message.
    fn failed_load(
        &mut self,
        advance: (bool, bool),
        title: String,
        err: anyhow::Error,
    ) -> LoadStatus {
        (self.run_next, self.run_prev) = advance;
        LoadStatus::Done {
            title,
            result: Err(err),
        }
    }

    // The pair below is what a real progress bar would be built from. What the
    // UI draws today is only the count of downloads in flight
    // ([`crate::emu_file::downloads_in_progress`]), so outside the tests
    // nothing calls them — but the byte counting behind `load_progress` is
    // plumbed end to end already (`fetch` counts,
    // [`FileSource::resolve_with_progress`] forwards).

    /// True while a [`load_async`] download is outstanding.
    pub fn is_loading(&self) -> bool {
        self.pending_load.is_some()
    }

    /// Byte progress of the download in flight. Reports an unknown total for a
    /// multi-disk set and for a server that declares no size.
    #[allow(dead_code)]
    pub fn load_progress(&self) -> Option<&JobProgress> {
        self.pending_load.as_ref().map(|p| match &p.phase {
            LoadPhase::Unpacking { job, .. } => job.progress(),
            LoadPhase::Creating(job) => job.progress(),
        })
    }

    /// Load `emu_file` here and now, downloading and unpacking it on this
    /// thread. [`load_async`] is what the frontend uses;
    /// this is the whole thing in one call, for a caller with nothing on
    /// screen to keep running — which today is nobody, since the frontend went
    /// asynchronous, but it is the one place the synchronous order of the load
    /// is still written out.
    #[allow(dead_code)]
    pub fn load(
        &mut self,
        time: &Time,
        sys: &NewSys,
        emu_file: &EmuFile,
        over: Option<&Override>,
    ) -> Result<()> {
        let mut source = emu_file.path.clone();
        let path = source.resolve()?;

        // `NewSys` and the `WorkFile` it builds own their meta, so the entry's
        // borrowed pairs are copied into `String`s here, at the one boundary
        // where the file list hands work off.
        let meta: HashMap<String, String> = emu_file
            .meta
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();

        let work_file = newsys::unpack_release(path, &meta)?;
        // Same order the asynchronous path keeps: the old core lets go of
        // whatever it owns before the new one is built (see
        // [`start_create`](Self::start_create)).
        self.core = None;
        let res = sys.load_prepared(work_file, over)?;
        self.finish_load(time, res, emu_file.game_info);
        Ok(())
    }

    /// Take over a [`LoadResult`] built by
    /// [`NewSys::load_prepared`](crate::newsys::NewSys::load_prepared) —
    /// everything about starting a release that has to happen on the main
    /// thread, which is only this bookkeeping.
    fn finish_load(&mut self, time: &Time, res: LoadResult, info: GameInfo) {
        self.title_info = info;
        if res.is_console {
            self.input_mode = InputMode::Joystick1;
        }

        self.is_image = res.system_name.starts_with("Image");
        self.paused = self.is_image && (!self.color_cycle);

        self.core = Some(res.backend);
        self.work_file = res.work_file;

        self.run_next = false;
        self.audio_seen = false;
        self.next_frame = time.elapsed_secs_f64();
        self.start_time = time.elapsed_secs_f64();
        self.last_active_time = time.elapsed_secs();
        trace!("FRAME START");
    }
}

pub(crate) fn handle_loading(
    mut emus: Query<(Entity, &mut Emulator)>,
    mut settings: ResMut<AppSettings>,
    mut writer: MessageWriter<SetHudText>,
    mut loaded: MessageWriter<LoadFinished>,
    time: Res<Time>,
) {
    for (entity, mut emu) in &mut emus.iter_mut() {
        let flen = settings.files.len() as isize;

        let d = if emu.run_next && (settings.tv_mode || settings.current_game < flen - 1) {
            1
        } else if emu.run_prev && (settings.tv_mode || settings.current_game > 0) {
            -1
        } else {
            0
        };
        if d != 0 {
            settings.current_game = (settings.current_game + d + flen) % flen;
            let index = settings.current_game as usize;
            let game = &settings.files[index];
            let over = settings.override_for(index);
            if let Some(o) = &over {
                debug!("Found override for {game:?}: {o:?}");
            }
            if let Some(previous) = &emu.pending_load {
                previous.phase.cancel();
                DOWNLOAD_COUNTER.ended();
            }
            emu.state = EmuState::Loading;
            emu.run_next = false;
            emu.run_prev = false;
            emu.pending_load = Some(load_async(game, over.as_ref()));
            DOWNLOAD_COUNTER.started();
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
                emu.run_next = false;
                emu.run_prev = false;
                loaded.write(LoadFinished(entity));
                if emu.is_crossfade {
                    emu.state = EmuState::PreDelay;
                } else {
                    emu.state = EmuState::Running;
                    if settings.show_info && settings.maximized {
                        writer.write(SetHudText {
                            text: emu.get_info(),
                            delay: Duration::from_secs(settings.info_delay),
                            duration: Duration::from_secs(settings.info_duration),
                            location: HudLocation::InfoText,
                        });
                    }
                }
                continue;
            }
        }
    }
}

impl Plugin for LoadingPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<LoadFinished>()
            .add_systems(Update, handle_loading.in_set(FrontendSet::Loading));
    }
}

#[cfg(test)]
#[path = "tests/loading_tests.rs"]
mod tests;
