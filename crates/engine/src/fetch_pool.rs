//! Process-wide workers for asynchronous fetches. Waiting requests occupy
//! queue entries, not threads (WASIX allocates each stack in linear memory).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;

/// Two saturated hosts can make progress without growing the thread count.
const WORKERS: usize = 12;
const WORKER_STACK: usize = 2 * 1024 * 1024;
type Work = Box<dyn FnOnce() + Send + 'static>;

pub(crate) fn submit(host: String, work: impl FnOnce() + Send + 'static) -> Result<(), String> {
    static POOL: OnceLock<Result<Pool, String>> = OnceLock::new();
    match POOL.get_or_init(|| Pool::new(WORKERS, crate::fetch::MAX_CONNECTIONS_PER_HOST)) {
        Ok(pool) => {
            pool.submit(host, Box::new(work));
            Ok(())
        }
        Err(error) => Err(error.clone()),
    }
}

struct Job {
    host: String,
    work: Work,
}

#[derive(Default)]
struct Queue {
    pending: VecDeque<Job>,
    busy: HashMap<String, usize>,
    closed: bool,
}

impl Queue {
    /// Skip saturated hosts instead of parking workers behind them.
    fn take_ready(&mut self, per_host: usize) -> Option<Job> {
        let index = self
            .pending
            .iter()
            .position(|job| self.busy.get(&job.host).copied().unwrap_or(0) < per_host)?;
        let job = self.pending.remove(index)?;
        *self.busy.entry(job.host.clone()).or_default() += 1;
        Some(job)
    }

    fn finish(&mut self, host: &str) {
        if let Some(count) = self.busy.get_mut(host) {
            *count -= 1;
            if *count == 0 {
                self.busy.remove(host);
            }
        }
    }
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    changed: Condvar,
}

struct Pool {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl Pool {
    fn new(workers: usize, per_host: usize) -> Result<Self, String> {
        let mut pool = Pool {
            shared: Arc::new(Shared::default()),
            threads: Vec::new(),
        };
        for index in 0..workers {
            let shared = Arc::clone(&pool.shared);
            let thread = std::thread::Builder::new()
                .name(format!("fetch-{index}"))
                .stack_size(WORKER_STACK)
                .spawn(move || worker(shared, per_host))
                .map_err(|e| format!("could not start the fetch pool: {e}"))?;
            pool.threads.push(thread);
        }
        Ok(pool)
    }

    fn submit(&self, host: String, work: Work) {
        let mut queue = self.shared.queue.lock().unwrap_or_else(|p| p.into_inner());
        queue.pending.push_back(Job { host, work });
        self.shared.changed.notify_one();
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        // Also cleans up a partially started pool if spawning fails. Workers
        // own Shared, not Pool, so they cannot keep their owner alive.
        let pending = {
            let mut queue = self.shared.queue.lock().unwrap_or_else(|p| p.into_inner());
            queue.closed = true;
            std::mem::take(&mut queue.pending)
        };
        drop(pending);
        self.shared.changed.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn worker(shared: Arc<Shared>, per_host: usize) {
    loop {
        let job = {
            let mut queue = shared.queue.lock().unwrap_or_else(|p| p.into_inner());
            loop {
                if queue.closed {
                    return;
                }
                if let Some(job) = queue.take_ready(per_host) {
                    break job;
                }
                queue = shared
                    .changed
                    .wait(queue)
                    .unwrap_or_else(|p| p.into_inner());
            }
        };
        // Network I/O and paused Fetch.* requests never hold the queue lock.
        // With unwinding enabled, a failed job must release its host slot
        // and leave the worker usable. Abort builds still abort on panic.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job.work)).is_err() {
            eprintln!("fetch worker: a request panicked before answering");
        }
        let mut queue = shared.queue.lock().unwrap_or_else(|p| p.into_inner());
        queue.finish(&job.host);
        shared.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn a_saturated_host_does_not_hide_another_hosts_work() {
        let mut queue = Queue::default();
        for host in ["a", "a", "a", "b"] {
            queue.pending.push_back(Job {
                host: host.into(),
                work: Box::new(|| {}),
            });
        }
        assert_eq!(queue.take_ready(2).unwrap().host, "a");
        assert_eq!(queue.take_ready(2).unwrap().host, "a");
        assert_eq!(queue.take_ready(2).unwrap().host, "b");
        assert!(queue.take_ready(2).is_none());
        queue.finish("a");
        assert_eq!(queue.take_ready(2).unwrap().host, "a");
        queue.finish("a");
        queue.finish("a");
        queue.finish("b");
        assert!(queue.busy.is_empty(), "finished hosts must not accumulate");
    }

    #[test]
    fn repeated_batches_reuse_a_bounded_set_of_workers() {
        let pool = Pool::new(3, 2).unwrap();
        let (tx, rx) = mpsc::channel();
        let mut threads = HashSet::new();
        for _ in 0..10 {
            for n in 0..80 {
                let tx = tx.clone();
                pool.submit(
                    format!("host-{}", n % 4),
                    Box::new(move || {
                        tx.send(std::thread::current().id()).unwrap();
                    }),
                );
            }
            for _ in 0..80 {
                threads.insert(rx.recv_timeout(Duration::from_secs(3)).unwrap());
            }
        }
        assert!(!threads.is_empty());
        assert!(
            threads.len() <= 3,
            "{} threads for 800 requests",
            threads.len()
        );
        let shared = Arc::downgrade(&pool.shared);
        drop(pool);
        assert!(
            shared.upgrade().is_none(),
            "workers must exit with their owner"
        );
    }

    #[test]
    fn a_panicking_job_releases_its_slot_and_the_worker_survives() {
        let pool = Pool::new(1, 1).unwrap();
        pool.submit("a".into(), Box::new(|| panic!("failed request")));
        let (tx, rx) = mpsc::channel();
        pool.submit(
            "a".into(),
            Box::new(move || {
                tx.send(()).unwrap();
            }),
        );
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn blocked_jobs_leave_workers_for_other_hosts() {
        let pool = Pool::new(3, 2).unwrap();
        let (started, arrivals) = mpsc::channel();
        let mut releases = Vec::new();
        for _ in 0..3 {
            let (release, wait) = mpsc::channel();
            releases.push(release);
            let started = started.clone();
            pool.submit(
                "a".into(),
                Box::new(move || {
                    started.send("a").unwrap();
                    // Bound the wait so a broken scheduler fails rather than hangs.
                    let _ = wait.recv_timeout(Duration::from_secs(3));
                }),
            );
        }
        assert_eq!(arrivals.recv_timeout(Duration::from_secs(1)).unwrap(), "a");
        assert_eq!(arrivals.recv_timeout(Duration::from_secs(1)).unwrap(), "a");
        pool.submit(
            "b".into(),
            Box::new(move || {
                started.send("b").unwrap();
            }),
        );
        assert_eq!(arrivals.recv_timeout(Duration::from_secs(1)).unwrap(), "b");
        for release in releases {
            let _ = release.send(());
        }
        assert_eq!(arrivals.recv_timeout(Duration::from_secs(1)).unwrap(), "a");
    }
}
