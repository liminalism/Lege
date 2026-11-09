//! Background page decoding with prefetch and generation ids.
//!
//! The app replaces the whole wanted list on every navigation (current page
//! first, then neighbours). Workers take jobs from the front; queued jobs for
//! pages no longer wanted are simply gone, so rapid arrow-key navigation
//! never decodes every page it passed. Every result carries the generation
//! it was requested in, and the app only *displays* a result for the page
//! that is current — a slow decode of page 12 cannot replace page 15.
//! (It is still cached: the pixels are valid.)

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::decode::{PageImage, decode_page};

#[derive(Clone, Debug)]
pub struct Job {
    pub page_key: String,
    pub path: PathBuf,
    pub generation: u64,
}

pub struct Loaded {
    pub page_key: String,
    pub generation: u64,
    pub result: Result<Arc<PageImage>, String>,
    pub decode_time: Duration,
}

struct Queue {
    jobs: VecDeque<Job>,
    in_flight: HashSet<String>,
    stop: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
}

pub struct Loader {
    shared: Arc<Shared>,
    pub results: Receiver<Loaded>,
    threads: Vec<JoinHandle<()>>,
}

impl Loader {
    pub fn spawn(threads: usize, wake: Arc<dyn Fn() + Send + Sync>) -> Loader {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                jobs: VecDeque::new(),
                in_flight: HashSet::new(),
                stop: false,
            }),
            ready: Condvar::new(),
        });
        let (tx, results) = channel::<Loaded>();
        let mut handles = Vec::new();
        for i in 0..threads.max(1) {
            let shared = shared.clone();
            let tx: Sender<Loaded> = tx.clone();
            let wake = wake.clone();
            let h = std::thread::Builder::new()
                .name(format!("annotator-decode-{i}"))
                .spawn(move || worker(&shared, &tx, &*wake))
                .expect("spawn decode thread");
            handles.push(h);
        }
        Loader {
            shared,
            results,
            threads: handles,
        }
    }

    /// Replace the wanted list. `skip` are pages already cached.
    pub fn want(&self, jobs: Vec<Job>) {
        let Ok(mut q) = self.shared.queue.lock() else {
            return;
        };
        q.jobs.clear();
        for j in jobs {
            if !q.in_flight.contains(&j.page_key) {
                q.jobs.push_back(j);
            }
        }
        drop(q);
        self.shared.ready.notify_all();
    }

    pub fn is_loading(&self, key: &str) -> bool {
        self.shared
            .queue
            .lock()
            .map(|q| q.in_flight.contains(key) || q.jobs.iter().any(|j| j.page_key == key))
            .unwrap_or(false)
    }

    /// Block until a result arrives (tests, benchmarks).
    pub fn wait(&self, timeout: Duration) -> Option<Loaded> {
        self.results.recv_timeout(timeout).ok()
    }
}

fn worker(shared: &Shared, tx: &Sender<Loaded>, wake: &(dyn Fn() + Send + Sync)) {
    loop {
        let job = {
            let Ok(mut q) = shared.queue.lock() else {
                return;
            };
            loop {
                if q.stop {
                    return;
                }
                if let Some(j) = q.jobs.pop_front() {
                    q.in_flight.insert(j.page_key.clone());
                    break j;
                }
                q = match shared.ready.wait(q) {
                    Ok(q) => q,
                    Err(_) => return,
                };
            }
        };
        let t0 = Instant::now();
        let result = std::panic::catch_unwind(|| decode_page(&job.path))
            .unwrap_or_else(|_| Err("decoder panicked".into()))
            .map(Arc::new);
        let decode_time = t0.elapsed();
        if let Ok(mut q) = shared.queue.lock() {
            q.in_flight.remove(&job.page_key);
        }
        if tx
            .send(Loaded {
                page_key: job.page_key,
                generation: job.generation,
                result,
                decode_time,
            })
            .is_err()
        {
            return;
        }
        wake();
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        if let Ok(mut q) = self.shared.queue.lock() {
            q.stop = true;
            q.jobs.clear();
        }
        self.shared.ready.notify_all();
        for h in self.threads.drain(..) {
            let _ = h.join();
        }
    }
}
