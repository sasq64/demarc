//! Background jobs: long, blocking work — downloading a release, unpacking an
//! archive — run off the main thread and polled for completion from an ordinary
//! Bevy system.
//!
//! # Why there is no async runtime here
//!
//! Every I/O crate demarc uses is blocking (`ureq`, `suppaftp`, `zip`,
//! `unarc-rs`, `fatfs`), so there is nothing to `.await`; adding tokio or smol
//! would only mean calling the same blocking code through a runtime. What this
//! module borrows from `async` is just its *handle* type. [`bevy::tasks::Task`]
//! is a future-like object that can be polled without blocking, and Bevy's
//! [`IoTaskPool`] is a thread pool that exists precisely to run blocking I/O.
//! So a job body is a plain blocking closure, wrapped in an `async move` block
//! that never awaits anything. No new dependency, no runtime, no colouring the
//! rest of the app async.
//!
//! # Usage
//!
//! Spawn from any system holding [`Jobs<T>`], where `T` is the job's result
//! type; [`JobsPlugin`] registers `T = PathBuf`, which covers both helpers
//! below. Other result types need one [`AppJobsExt::add_job_type`] call.
//!
//! ```ignore
//! fn start(mut jobs: ResMut<Jobs<PathBuf>>) {
//!     jobs.download("https://files.scene.org/get/demo.zip");
//! }
//!
//! fn finished(mut done: MessageReader<JobFinished<PathBuf>>) {
//!     for msg in done.read() {
//!         match msg.result() {
//!             Some(Ok(path)) => info!("{} -> {}", msg.name, path.display()),
//!             Some(Err(err)) => warn!("{} failed: {err}", msg.name),
//!             None => {} // another reader already took it
//!         }
//!     }
//! }
//! ```
//!
//! Arbitrary work goes through [`Jobs::spawn`], which hands the closure a
//! [`JobProgress`] to report through:
//!
//! ```ignore
//! jobs.spawn("scan", |progress| {
//!     progress.set_total(files.len() as u64);
//!     for f in files {
//!         if progress.is_cancelled() {
//!             break;
//!         }
//!         progress.advance(1);
//!     }
//!     Ok(result)
//! });
//! ```

// This is a general-purpose facility whose API is deliberately wider than its
// current callers, which so far use only a corner of it.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use bevy::ecs::message::Message;
use bevy::prelude::*;
use bevy::tasks::{IoTaskPool, Task, futures::check_ready};

/// Registers the machinery for `PathBuf`-returning jobs, which is what
/// [`Jobs::download`] and [`Jobs::unpack`] produce. Register any other result
/// type with [`AppJobsExt::add_job_type`].
pub struct JobsPlugin;

impl Plugin for JobsPlugin {
    fn build(&self, app: &mut App) {
        app.add_job_type::<PathBuf>();
    }
}

/// Adds the resource, message and polling system for one job result type.
pub trait AppJobsExt {
    fn add_job_type<T: Send + Sync + 'static>(&mut self) -> &mut Self;
}

impl AppJobsExt for App {
    fn add_job_type<T: Send + Sync + 'static>(&mut self) -> &mut Self {
        self.init_resource::<Jobs<T>>()
            .add_message::<JobFinished<T>>()
            .add_systems(Update, poll_jobs::<T>)
    }
}

/// Handle to a spawned job, unique within its [`Jobs<T>`] resource.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(u64);

/// Why a job didn't produce a value.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// [`Jobs::cancel`] was called. Note this is reported whenever the flag was
    /// set by the time the body returned, even if the body ignored it and ran
    /// to completion — the result is discarded in that case.
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Failed(#[from] anyhow::Error),
}

/// Shared, lock-free channel between a running job and the main thread: the job
/// body writes progress and reads the cancel flag, systems read progress and
/// set the flag.
///
/// A `total` of 0 means "unknown", which is what [`fraction`](Self::fraction)
/// reports as `None` — an FTP server that won't answer `SIZE` leaves a download
/// in that state, so a progress bar needs an indeterminate mode.
#[derive(Debug, Default)]
pub struct JobProgress {
    done: AtomicU64,
    total: AtomicU64,
    cancelled: AtomicBool,
}

impl JobProgress {
    /// Total units of work (bytes, files, …); 0 means unknown.
    pub fn set_total(&self, total: u64) {
        self.total.store(total, Ordering::Relaxed);
    }

    /// Units completed so far.
    pub fn set_done(&self, done: u64) {
        self.done.store(done, Ordering::Relaxed);
    }

    /// Adds to the completed count.
    pub fn advance(&self, units: u64) {
        self.done.fetch_add(units, Ordering::Relaxed);
    }

    pub fn done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }

    /// Total units, or `None` when the job never reported one.
    pub fn total(&self) -> Option<u64> {
        match self.total.load(Ordering::Relaxed) {
            0 => None,
            total => Some(total),
        }
    }

    /// Completion in `0.0..=1.0`, or `None` when the total is unknown.
    pub fn fraction(&self) -> Option<f32> {
        self.total()
            .map(|total| (self.done() as f32 / total as f32).clamp(0.0, 1.0))
    }

    /// Asks the job to stop. Purely advisory: a blocking job body only stops
    /// where it checks [`is_cancelled`](Self::is_cancelled).
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// Written once per finished job, whether it succeeded, failed or was
/// cancelled.
///
/// The result is behind [`result`](Self::result) / [`take`](Self::take) rather
/// than a public field so a non-`Clone` `T` can still be moved out, via
/// `MessageMutator<JobFinished<T>>`. Taking it hides it from every other reader
/// of the same message, so only take when this system owns the job.
pub struct JobFinished<T: Send + Sync + 'static> {
    pub id: JobId,
    /// The label passed to [`Jobs::spawn`], for logs and HUD text.
    pub name: Arc<str>,
    result: Option<Result<T, JobError>>,
}

impl<T: Send + Sync + 'static> Message for JobFinished<T> {}

impl<T: Send + Sync + 'static> JobFinished<T> {
    /// Borrows the result; `None` once some system has [`take`](Self::take)n it.
    pub fn result(&self) -> Option<&Result<T, JobError>> {
        self.result.as_ref()
    }

    /// Moves the result out. Needs `MessageMutator<JobFinished<T>>`, since
    /// `MessageReader` only hands out shared references.
    pub fn take(&mut self) -> Option<Result<T, JobError>> {
        self.result.take()
    }

    /// True if the job produced a value and nothing has taken it yet.
    pub fn is_ok(&self) -> bool {
        matches!(self.result, Some(Ok(_)))
    }
}

/// A job that hasn't finished yet.
pub struct ActiveJob<'a> {
    pub id: JobId,
    pub name: &'a str,
    pub progress: &'a JobProgress,
}

/// One background job, owned by whoever spawned it.
///
/// This is the handle form: hold it in a component or a struct field and call
/// [`poll`](Self::poll) each frame. Use it when the result belongs to one
/// specific owner — a per-entity download, say — where routing it back through
/// a global [`JobFinished`] message would only mean filtering by id again.
/// [`Jobs<T>`] is the collective form, built on this.
///
/// Dropping the handle abandons the job: the blocking body still runs to
/// completion on its pool thread (nothing can interrupt it), but its result is
/// discarded. Call [`cancel`](Self::cancel) first if the body checks for it.
pub struct Job<T> {
    name: Arc<str>,
    task: Task<Result<T, JobError>>,
    progress: Arc<JobProgress>,
    /// Set once [`poll`](Self::poll) has handed the result over. Polling an
    /// `async_task::Task` after it has completed panics, so this is what makes
    /// a repeated `poll` return `None` instead.
    taken: bool,
}

impl<T: Send + 'static> Job<T> {
    /// Runs `work` on the I/O pool and returns immediately.
    ///
    /// `work` is ordinary blocking code. It gets a [`JobProgress`] to report
    /// through and to poll for cancellation — a job that never checks
    /// [`JobProgress::is_cancelled`] simply runs to completion and has its
    /// result discarded, since dropping a [`Task`] whose body never awaits
    /// cannot interrupt it.
    pub fn spawn<F>(name: impl Into<Arc<str>>, work: F) -> Self
    where
        F: FnOnce(&JobProgress) -> anyhow::Result<T> + Send + 'static,
    {
        let name = name.into();
        let progress = Arc::new(JobProgress::default());

        let task_progress = Arc::clone(&progress);
        let task = IoTaskPool::get().spawn(async move {
            // Not an async body in any real sense: no `.await` appears below,
            // the blocking call just occupies one I/O-pool thread until it
            // returns. `IoTaskPool` is sized for exactly that.
            if task_progress.is_cancelled() {
                return Err(JobError::Cancelled);
            }
            let result = work(&task_progress);
            if task_progress.is_cancelled() {
                return Err(JobError::Cancelled);
            }
            result.map_err(JobError::Failed)
        });

        Self {
            name,
            task,
            progress,
            taken: false,
        }
    }

    /// Returns the result if the job has finished, `None` while it is still
    /// running. Never blocks, so this is what a per-frame system calls.
    ///
    /// Returns `None` forever once the result has been handed over.
    pub fn poll(&mut self) -> Option<Result<T, JobError>> {
        if self.taken {
            return None;
        }
        let result = check_ready(&mut self.task)?;
        self.taken = true;
        Some(result)
    }

    /// True until [`poll`](Self::poll) has produced the result.
    pub fn is_running(&self) -> bool {
        !self.taken
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn progress(&self) -> &JobProgress {
        &self.progress
    }

    /// Asks the job to stop; it reports [`JobError::Cancelled`] once the body
    /// returns. Advisory — see [`JobProgress::cancel`].
    pub fn cancel(&self) {
        self.progress.cancel();
    }
}

struct RunningJob<T> {
    id: JobId,
    job: Job<T>,
}

/// The jobs currently in flight that produce a `T`.
///
/// One resource per result type; [`JobsPlugin`] registers `Jobs<PathBuf>` and
/// [`AppJobsExt::add_job_type`] adds others.
#[derive(Resource)]
pub struct Jobs<T: Send + Sync + 'static> {
    next_id: u64,
    running: Vec<RunningJob<T>>,
}

// Manual: `derive(Default)` would demand `T: Default`, which job results need
// not be.
impl<T: Send + Sync + 'static> Default for Jobs<T> {
    fn default() -> Self {
        Self {
            next_id: 0,
            running: Vec::new(),
        }
    }
}

impl<T: Send + Sync + 'static> Jobs<T> {
    /// Runs `work` on the I/O pool and returns immediately. See
    /// [`Job::spawn`] for what `work` may do.
    ///
    /// Completion arrives as a [`JobFinished<T>`] message in `Update`. To own
    /// the result directly instead, spawn a [`Job<T>`] and poll it yourself.
    pub fn spawn<F>(&mut self, name: impl Into<Arc<str>>, work: F) -> JobId
    where
        F: FnOnce(&JobProgress) -> anyhow::Result<T> + Send + 'static,
    {
        let id = JobId(self.next_id);
        self.next_id += 1;
        self.running.push(RunningJob {
            id,
            job: Job::spawn(name, work),
        });
        id
    }

    /// Signals `id` to stop. Takes `&self` because the flag is atomic, so a
    /// system holding `Res<Jobs<T>>` can cancel without triggering change
    /// detection. The job still reports back, as [`JobError::Cancelled`].
    pub fn cancel(&self, id: JobId) {
        if let Some(entry) = self.running.iter().find(|entry| entry.id == id) {
            entry.job.cancel();
        }
    }

    pub fn cancel_all(&self) {
        for entry in &self.running {
            entry.job.cancel();
        }
    }

    /// Progress handle for `id`, while it is still running.
    pub fn progress(&self, id: JobId) -> Option<&JobProgress> {
        self.running
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.job.progress())
    }

    pub fn is_running(&self, id: JobId) -> bool {
        self.running.iter().any(|entry| entry.id == id)
    }

    /// Every unfinished job, oldest first — for a HUD listing what's in flight.
    pub fn active(&self) -> impl Iterator<Item = ActiveJob<'_>> {
        self.running.iter().map(|entry| ActiveJob {
            id: entry.id,
            name: entry.job.name(),
            progress: entry.job.progress(),
        })
    }

    pub fn len(&self) -> usize {
        self.running.len()
    }

    pub fn is_empty(&self) -> bool {
        self.running.is_empty()
    }
}

impl Jobs<PathBuf> {
    /// Downloads `url` into the shared download cache and reports the cached
    /// path. Byte progress is reported when the server gives a size.
    ///
    /// A URL already in the cache still round-trips through the pool; it just
    /// finishes on the next frame or two.
    pub fn download(&mut self, url: impl Into<String>) -> JobId {
        let url = url.into();
        self.spawn(url.clone(), move |progress| {
            crate::fetch::fetch_url_with_progress(&url, &|done, total| {
                progress.set_done(done);
                progress.set_total(total.unwrap_or(0));
            })
        })
    }

    /// Unpacks `archive` into `target_dir`, reporting `target_dir` on success.
    ///
    /// Fails if the file isn't a recognised archive, where
    /// [`crate::utils::unpack_into`] would return `Ok(false)` — a job that
    /// silently did nothing is harder to act on than an error.
    pub fn unpack(&mut self, archive: impl AsRef<Path>, target_dir: impl AsRef<Path>) -> JobId {
        let archive = archive.as_ref().to_path_buf();
        let target = target_dir.as_ref().to_path_buf();
        let name: Arc<str> = Arc::from(format!("unpack {}", archive.display()));
        self.spawn(name, move |_progress| {
            if crate::utils::unpack_into(&archive, &target)? {
                Ok(target)
            } else {
                anyhow::bail!("not a supported archive: {}", archive.display())
            }
        })
    }

    /// Downloads `url` and unpacks it into a fresh temp directory, reporting
    /// that directory.
    ///
    /// The temp dir outlives the job (as [`crate::fetch::gather_files`] does
    /// with its own), so the caller owns it and is responsible for removing it.
    /// Non-archive downloads are left alone and the file is simply copied in.
    pub fn download_and_unpack(&mut self, url: impl Into<String>) -> JobId {
        let url = url.into();
        self.spawn(url.clone(), move |progress| {
            let archive = crate::fetch::fetch_url_with_progress(&url, &|done, total| {
                progress.set_done(done);
                progress.set_total(total.unwrap_or(0));
            })?;
            // Unpacking has no useful byte count; flip back to indeterminate so
            // a progress bar doesn't sit at 100% for the rest of the job.
            progress.set_total(0);
            progress.set_done(0);

            let dir = tempfile::Builder::new().prefix("demarc-").tempdir()?.keep();
            if !crate::utils::unpack_into(&archive, &dir)? {
                let name = archive.file_name().unwrap_or(archive.as_os_str());
                std::fs::copy(&archive, dir.join(name))?;
            }
            Ok(dir)
        })
    }
}

/// Moves every finished job's result into a [`JobFinished<T>`] message. One
/// instance runs per registered result type.
fn poll_jobs<T: Send + Sync + 'static>(
    mut jobs: ResMut<Jobs<T>>,
    mut finished: MessageWriter<JobFinished<T>>,
) {
    // Read through `Deref` first: with nothing in flight this leaves
    // `Jobs<T>` unchanged, so the resource isn't marked changed every frame.
    if jobs.running.is_empty() {
        return;
    }

    let mut done = Vec::new();
    jobs.running.retain_mut(|entry| match entry.job.poll() {
        Some(result) => {
            done.push(JobFinished {
                id: entry.id,
                name: Arc::clone(&entry.job.name),
                result: Some(result),
            });
            false
        }
        None => true,
    });

    for msg in done {
        match msg.result() {
            Some(Err(err)) => warn!("Job '{}' failed: {err}", msg.name),
            _ => debug!("Job '{}' finished", msg.name),
        }
        finished.write(msg);
    }
}

#[cfg(test)]
#[path = "tests/jobs_tests.rs"]
mod tests;
