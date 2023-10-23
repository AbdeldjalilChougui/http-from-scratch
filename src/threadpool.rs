//! Fixed-size thread pool (the classic channel + shared receiver design).

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

type Job = Box<dyn FnOnce() + Send + 'static>;

pub struct ThreadPool {
    sender: Option<Sender<Job>>,
    workers: Vec<JoinHandle<()>>,
}

impl ThreadPool {
    /// Create a pool with `size` worker threads. Panics if `size == 0`.
    pub fn new(size: usize) -> ThreadPool {
        assert!(size > 0, "thread pool needs at least one worker");
        let (sender, receiver) = channel::<Job>();
        let receiver: Arc<Mutex<Receiver<Job>>> = Arc::new(Mutex::new(receiver));
        let workers = (0..size)
            .map(|i| {
                let rx = Arc::clone(&receiver);
                std::thread::Builder::new()
                    .name(format!("hfs-worker-{i}"))
                    .spawn(move || loop {
                        // Hold the lock only while receiving, not while running the job.
                        let job = match rx.lock() {
                            Ok(guard) => guard.recv(),
                            Err(_) => return,
                        };
                        match job {
                            Ok(job) => {
                                // A panicking job must not kill the worker.
                                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                            }
                            Err(_) => return, // channel closed: shut down
                        }
                    })
                    .expect("spawn worker")
            })
            .collect();
        ThreadPool { sender: Some(sender), workers }
    }

    pub fn execute(&self, f: impl FnOnce() + Send + 'static) {
        if let Some(s) = &self.sender {
            let _ = s.send(Box::new(f));
        }
    }
}

impl Drop for ThreadPool {
    /// Graceful shutdown: close the channel, let queued jobs finish, join.
    fn drop(&mut self) {
        drop(self.sender.take());
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn runs_all_jobs_before_drop_returns() {
        let n = Arc::new(AtomicUsize::new(0));
        {
            let pool = ThreadPool::new(4);
            for _ in 0..100 {
                let n = n.clone();
                pool.execute(move || {
                    n.fetch_add(1, Ordering::SeqCst);
                });
            }
        }
        assert_eq!(n.load(Ordering::SeqCst), 100);
    }

    #[test]
    fn survives_panicking_job() {
        let n = Arc::new(AtomicUsize::new(0));
        {
            let pool = ThreadPool::new(1);
            pool.execute(|| panic!("boom"));
            let n2 = n.clone();
            pool.execute(move || {
                n2.fetch_add(1, Ordering::SeqCst);
            });
        }
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }
}
