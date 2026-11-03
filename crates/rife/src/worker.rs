//! A [`Rife`] on a thread of its own, interpolating frames before they are shown.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

use crate::Rife;

pub type Frame = Arc<Vec<u32>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Loading,
    Ready,
    Failed,
}

/// A pair of frames of one view, by the addresses the `Arc`s keep alive.
type Pair<K> = (K, usize, usize);

/// One frame to make: a pair and the phase between them, as `f32` bits.
type Key<K> = (Pair<K>, u32);

struct Job<K> {
    view: K,
    a: Frame,
    b: Frame,
    size: (u32, u32),
    t: f32,
}

struct State<K> {
    status: Status,
    error: Option<String>,
    wanted: HashSet<Key<K>>,
    done: HashMap<Key<K>, (Frame, Frame, Frame)>,
    stop: bool,
}

pub struct Worker<K> {
    jobs: Option<mpsc::Sender<Job<K>>>,
    state: Arc<Mutex<State<K>>>,
    requested: HashSet<Key<K>>,
    thread: Option<JoinHandle<()>>,
}

fn pair<K>(view: K, a: &Frame, b: &Frame) -> Pair<K> {
    (view, Arc::as_ptr(a) as usize, Arc::as_ptr(b) as usize)
}

impl<K: Copy + Eq + Hash + Send + 'static> Worker<K> {
    /// Start the thread, which makes its [`Rife`] with `load` first.
    pub fn start(load: impl FnOnce() -> anyhow::Result<Rife> + Send + 'static) -> Self {
        let (jobs, recv) = mpsc::channel::<Job<K>>();
        let state = Arc::new(Mutex::new(State {
            status: Status::Loading,
            error: None,
            wanted: HashSet::new(),
            done: HashMap::new(),
            stop: false,
        }));
        let shared = Arc::clone(&state);
        let thread = std::thread::spawn(move || {
            let mut rife = match load() {
                Ok(rife) => {
                    shared.lock().unwrap().status = Status::Ready;
                    rife
                }
                Err(err) => {
                    let mut state = shared.lock().unwrap();
                    state.status = Status::Failed;
                    state.error = Some(format!("{err:#}"));
                    return;
                }
            };
            let mut out = Vec::new();
            for job in recv {
                let key = (pair(job.view, &job.a, &job.b), job.t.to_bits());
                {
                    let state = shared.lock().unwrap();
                    if state.stop {
                        return;
                    }
                    if !state.wanted.contains(&key) {
                        continue;
                    }
                }
                let (width, height) = job.size;
                if let Err(err) = rife.interpolate(&job.a, &job.b, width, height, job.t, &mut out) {
                    let mut state = shared.lock().unwrap();
                    state.status = Status::Failed;
                    state.error = Some(format!("{err:#}"));
                    return;
                }
                let mut state = shared.lock().unwrap();
                if state.wanted.contains(&key) {
                    let frame = Arc::new(std::mem::take(&mut out));
                    state.done.insert(key, (job.a, job.b, frame));
                }
            }
        });
        Self {
            jobs: Some(jobs),
            state,
            requested: HashSet::new(),
            thread: Some(thread),
        }
    }

    /// The status, and why it failed if it did.
    pub fn status(&self) -> (Status, Option<String>) {
        let state = self.state.lock().unwrap();
        (state.status, state.error.clone())
    }

    /// Forget every frame but those at `phases` of each pair, which are the ones
    /// still to be shown.
    pub fn keep<'a>(
        &mut self,
        pairs: impl IntoIterator<Item = (K, &'a Frame, &'a Frame, &'a [f32])>,
    ) {
        let wanted: HashSet<_> = pairs
            .into_iter()
            .flat_map(|(view, a, b, phases)| {
                let key = pair(view, a, b);
                phases.iter().map(move |t| (key, t.to_bits()))
            })
            .collect();
        self.requested.retain(|key| wanted.contains(key));
        let mut state = self.state.lock().unwrap();
        state.done.retain(|key, _| wanted.contains(key));
        state.wanted = wanted;
    }

    /// Ask for the frames at each of `phases` from `a` to `b`, in order. A frame
    /// has to be kept by [`Self::keep`] to be made.
    pub fn request(&mut self, view: K, a: &Frame, b: &Frame, size: (u32, u32), phases: &[f32]) {
        for &t in phases {
            if self.requested.insert((pair(view, a, b), t.to_bits())) {
                let job = Job {
                    view,
                    a: Arc::clone(a),
                    b: Arc::clone(b),
                    size,
                    t,
                };
                if let Some(jobs) = &self.jobs {
                    let _ = jobs.send(job);
                }
            }
        }
    }

    /// The frame at `t` from `a` to `b`, if it has been made.
    pub fn get(&self, view: K, a: &Frame, b: &Frame, t: f32) -> Option<Frame> {
        let state = self.state.lock().unwrap();
        let (_, _, frame) = state.done.get(&(pair(view, a, b), t.to_bits()))?;
        Some(Arc::clone(frame))
    }
}

impl<K> Drop for Worker<K> {
    /// Waits for the frame being made, so the thread is not in the GPU when the
    /// process exits.
    fn drop(&mut self) {
        self.state.lock().unwrap().stop = true;
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
