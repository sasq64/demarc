//! The frontend's wgpu device, for backends that hand over frames as textures.

use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread;
use std::time::Duration;

#[derive(Clone)]
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

static GPU: OnceLock<Gpu> = OnceLock::new();

pub fn set_gpu(device: wgpu::Device, queue: wgpu::Queue) {
    let _ = GPU.set(Gpu { device, queue });
}

pub fn gpu() -> Option<&'static Gpu> {
    GPU.get()
}

/// How long to wait for a submit. wgpu holds the queue's fence while the render
/// thread waits on the swapchain, which is seconds when the window gets no
/// frame callbacks.
const SUBMIT_WAIT: Duration = Duration::from_millis(4);

type Job = (wgpu::CommandBuffer, Box<dyn FnOnce() + Send>);

/// Submits on a thread of its own, so a blocked queue does not block the caller.
pub struct Submitter {
    jobs: mpsc::Sender<Job>,
    submitted: Mutex<mpsc::Receiver<()>>,
    in_flight: bool,
}

impl Submitter {
    pub fn new(queue: wgpu::Queue) -> Option<Self> {
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let (done_tx, submitted) = mpsc::channel();
        thread::Builder::new()
            .name("gpu-submit".into())
            .spawn(move || {
                for (commands, done) in job_rx {
                    queue.submit([commands]);
                    queue.on_submitted_work_done(done);
                    if done_tx.send(()).is_err() {
                        break;
                    }
                }
            })
            .ok()?;
        Some(Self {
            jobs,
            submitted: Mutex::new(submitted),
            in_flight: false,
        })
    }

    fn receiver(&mut self) -> &mpsc::Receiver<()> {
        self.submitted.get_mut().unwrap_or_else(|e| e.into_inner())
    }

    pub fn ready(&mut self) -> bool {
        if self.in_flight && self.receiver().try_recv().is_ok() {
            self.in_flight = false;
        }
        !self.in_flight
    }

    /// `done` runs once the GPU has finished the work.
    pub fn submit(&mut self, commands: wgpu::CommandBuffer, done: Box<dyn FnOnce() + Send>) {
        if self.jobs.send((commands, done)).is_ok() {
            self.in_flight = self.receiver().recv_timeout(SUBMIT_WAIT).is_err();
        }
    }
}

/// Free textures kept in the pool past what is in use.
const SPARE: usize = 2;

/// Textures handed out as frames, each with data of the caller's, reused once
/// nothing else holds the texture.
pub struct TexturePool<T> {
    items: Vec<(Arc<wgpu::Texture>, T)>,
}

impl<T> Default for TexturePool<T> {
    fn default() -> Self {
        Self { items: Vec::new() }
    }
}

impl<T> TexturePool<T> {
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// A pooled texture nothing else holds, or a new one from `make`.
    pub fn get(&mut self, make: impl FnOnce() -> (wgpu::Texture, T)) -> (Arc<wgpu::Texture>, &T) {
        let unused = |t: &Arc<wgpu::Texture>| Arc::strong_count(t) == 1;
        let mut free = self.items.iter().filter(|(t, _)| unused(t)).count();
        self.items.retain(|(t, _)| {
            let drop = unused(t) && free > SPARE + 1;
            free -= drop as usize;
            !drop
        });
        let i = match self.items.iter().position(|(t, _)| unused(t)) {
            Some(i) => i,
            None => {
                let (texture, data) = make();
                self.items.push((Arc::new(texture), data));
                self.items.len() - 1
            }
        };
        let (texture, data) = &self.items[i];
        (Arc::clone(texture), data)
    }
}

pub fn u32_bytes(v: &[u32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast(), std::mem::size_of_val(v)) }
}
