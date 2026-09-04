use std::sync::mpsc;
use std::time::{Duration, Instant};

use bevy::MinimalPlugins;
use bevy::ecs::message::Messages;

use super::*;

/// An app with the job machinery for `T` and nothing else.
fn test_app<T: Send + Sync + 'static>() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins).add_job_type::<T>();
    app
}

/// Pumps the app until a job finishes, and returns the message.
///
/// Panics rather than looping forever if the job never lands: a hang here
/// would mean the poll system stopped draining tasks, which is exactly the
/// bug worth failing loudly on.
fn run_until_finished<T: Send + Sync + 'static>(app: &mut App) -> JobFinished<T> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.update();
        let mut messages = app.world_mut().resource_mut::<Messages<JobFinished<T>>>();
        if let Some(msg) = messages.drain().next() {
            return msg;
        }
        assert!(Instant::now() < deadline, "job never finished");
    }
}

#[test]
fn reports_a_result() {
    let mut app = test_app::<u32>();
    app.world_mut()
        .resource_mut::<Jobs<u32>>()
        .spawn("double", |_| Ok(21 * 2));

    let mut msg = run_until_finished::<u32>(&mut app);
    assert_eq!(&*msg.name, "double");
    assert!(matches!(msg.take(), Some(Ok(42))));
    // The result moves out with the message; nothing is left behind.
    assert!(app.world().resource::<Jobs<u32>>().is_empty());
}

#[test]
fn reports_a_failure() {
    let mut app = test_app::<u32>();
    app.world_mut()
        .resource_mut::<Jobs<u32>>()
        .spawn("boom", |_| anyhow::bail!("nope"));

    let msg = run_until_finished::<u32>(&mut app);
    let Some(Err(JobError::Failed(err))) = msg.result() else {
        panic!(
            "expected a failure, got {:?}",
            msg.result().map(|r| r.is_ok())
        );
    };
    assert_eq!(err.to_string(), "nope");
}

/// A cancelled job still reports back rather than vanishing, so a caller
/// waiting on it always gets an answer.
///
/// Whether the body runs at all is a race — the pool may enter it before
/// `cancel` lands — so only the outcome is asserted here. The body blocks
/// until the cancel has been issued so that it cannot *finish* first, which
/// would be a genuine `Ok` and made this test flaky; both the pre-start and
/// the post-body cancel check must report `Cancelled`.
#[test]
fn cancelling_reports_cancelled() {
    let mut app = test_app::<u32>();
    let (release_tx, release_rx) = mpsc::channel::<()>();

    let mut jobs = app.world_mut().resource_mut::<Jobs<u32>>();
    let id = jobs.spawn("slow", move |_| {
        // Ignores the flag deliberately: the value is what gets discarded.
        let _ = release_rx.recv();
        Ok(1)
    });
    // Same frame, before `poll_jobs` has ever run.
    jobs.cancel(id);
    // Errors when the pre-start check already returned and dropped the body.
    let _ = release_tx.send(());

    let msg = run_until_finished::<u32>(&mut app);
    assert_eq!(msg.id, id);
    assert!(matches!(msg.result(), Some(Err(JobError::Cancelled))));
}

/// A body that ignores the cancel flag still runs to completion — dropping
/// a `Task` can't interrupt blocking code — but its value is discarded.
#[test]
fn cancelling_mid_flight_discards_the_value() {
    let mut app = test_app::<u32>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (started_tx, started_rx) = mpsc::channel::<()>();

    let id = app
        .world_mut()
        .resource_mut::<Jobs<u32>>()
        .spawn("blocked", move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(7)
        });

    // Wait for the pool to actually enter the body, so the cancel below is
    // genuinely mid-flight rather than the pre-start case above.
    started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    app.update();
    let jobs = app.world().resource::<Jobs<u32>>();
    assert!(jobs.is_running(id), "job should still be in flight");
    assert_eq!(jobs.active().count(), 1);
    jobs.cancel(id);
    release_tx.send(()).unwrap();

    let msg = run_until_finished::<u32>(&mut app);
    assert!(matches!(msg.result(), Some(Err(JobError::Cancelled))));
}

#[test]
fn progress_is_readable_while_running() {
    let mut app = test_app::<u32>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (reported_tx, reported_rx) = mpsc::channel::<()>();

    let id = app
        .world_mut()
        .resource_mut::<Jobs<u32>>()
        .spawn("counting", move |progress| {
            progress.set_total(200);
            progress.advance(50);
            reported_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(0)
        });

    reported_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    app.update();
    let progress = app
        .world()
        .resource::<Jobs<u32>>()
        .progress(id)
        .expect("job is still running");
    assert_eq!(progress.done(), 50);
    assert_eq!(progress.total(), Some(200));
    assert_eq!(progress.fraction(), Some(0.25));

    release_tx.send(()).unwrap();
    run_until_finished::<u32>(&mut app);
}

/// An unreported total means indeterminate, not zero-length.
#[test]
fn unknown_total_has_no_fraction() {
    let progress = JobProgress::default();
    progress.advance(10);
    assert_eq!(progress.total(), None);
    assert_eq!(progress.fraction(), None);
}

/// Ids are unique, so `cancel`/`progress` can't collide across jobs.
#[test]
fn ids_are_distinct() {
    let mut jobs = Jobs::<u32>::default();
    let mut app = test_app::<u32>();
    app.update(); // initialises the task pools
    let a = jobs.spawn("a", |_| Ok(1));
    let b = jobs.spawn("b", |_| Ok(2));
    assert_ne!(a, b);
    jobs.cancel_all();
}
