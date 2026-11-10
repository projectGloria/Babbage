//! Dedicated thread pools and the query-in-flight flag.
//!
//! The crawl and interactive search used to share rayon's global pool. Rayon
//! serves jobs injected from outside the pool only after local and stolen
//! work, so a query's parallel stage waited for a crawl worker to run dry, and
//! with the crawl yielding to `query_in_flight` the two could wait on each
//! other forever. Each side now has its own pool, so neither can block the
//! other's scheduling.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Threads for the interactive query pool. Measured on a 16-thread Windows
/// machine, the fuzzy scan at 100k entries took 0.5 ms with 4 threads versus
/// 7.6 ms with the default 16: past a few threads the scan is pool overhead.
const QUERY_THREADS: usize = 4;

fn cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// Pool for latency-sensitive query work (fuzzy scan, facets, semantic scan,
/// snippets). Never used by indexing.
pub fn query_pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(QUERY_THREADS.min(cores()).max(1))
            .thread_name(|i| format!("babbage-query-{i}"))
            .build()
            .expect("query thread pool")
    })
}

/// Pool for crawl extraction and inline embedding. Leaves one core for the
/// UI and queries.
pub fn crawl_pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(cores().saturating_sub(1).max(2))
            .thread_name(|i| format!("babbage-crawl-{i}"))
            .build()
            .expect("crawl thread pool")
    })
}

/// Count of queries currently running. Background work polls [`is_set`] and
/// yields while it is non-zero.
///
/// A counter, not a boolean: with two overlapping searches the first to finish
/// must not clear the flag for the second. Set only through [`QueryFlag::enter`],
/// whose guard decrements on drop, so a panic in the query path cannot leave
/// it stuck.
///
/// [`is_set`]: QueryFlag::is_set
#[derive(Debug, Default)]
pub struct QueryFlag {
    active: AtomicUsize,
}

impl QueryFlag {
    pub fn new() -> Self {
        Self::default()
    }

    /// Compatibility with the former `AtomicBool`: true while any query runs.
    pub fn load(&self, ordering: Ordering) -> bool {
        self.active.load(ordering) > 0
    }

    pub fn is_set(&self) -> bool {
        self.load(Ordering::Relaxed)
    }

    pub fn enter(&self) -> QueryFlagGuard<'_> {
        self.active.fetch_add(1, Ordering::SeqCst);
        QueryFlagGuard { flag: self }
    }

    /// Sleeps while a query runs, for at most `max_wait`. Bounded so a
    /// pathological query cannot stall background work indefinitely.
    pub fn wait_clear(&self, poll: Duration, max_wait: Duration) {
        let started = Instant::now();
        while self.is_set() && started.elapsed() < max_wait {
            std::thread::sleep(poll);
        }
    }
}

pub struct QueryFlagGuard<'a> {
    flag: &'a QueryFlag,
}

impl Drop for QueryFlagGuard<'_> {
    fn drop(&mut self) {
        self.flag.active.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_guards_keep_flag_set() {
        let flag = QueryFlag::new();
        let a = flag.enter();
        let b = flag.enter();
        drop(a);
        assert!(flag.is_set());
        drop(b);
        assert!(!flag.is_set());
    }

    #[test]
    fn panic_clears_flag() {
        let flag = std::sync::Arc::new(QueryFlag::new());
        let f = std::sync::Arc::clone(&flag);
        let _ = std::thread::spawn(move || {
            let _g = f.enter();
            panic!("query panicked");
        })
        .join();
        assert!(!flag.is_set());
    }

    #[test]
    fn query_pool_runs_while_crawl_pool_is_saturated() {
        use rayon::prelude::*;
        let (tx, rx) = std::sync::mpsc::channel();
        let busy = std::thread::spawn(|| {
            crawl_pool().install(|| {
                (0..crawl_pool().current_num_threads() * 4)
                    .into_par_iter()
                    .for_each(|_| std::thread::sleep(Duration::from_millis(150)));
            })
        });
        std::thread::sleep(Duration::from_millis(20));
        let started = Instant::now();
        let sum: u64 = query_pool().install(|| (0..10_000u64).into_par_iter().sum());
        tx.send(started.elapsed()).unwrap();
        assert_eq!(sum, 49_995_000);
        assert!(rx.recv().unwrap() < Duration::from_millis(100));
        busy.join().unwrap();
    }
}
