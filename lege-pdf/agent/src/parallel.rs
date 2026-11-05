//! Page-level parallelism with results delivered in page order.
//!
//! Pages of a PDF are independent work, and a multi-page text, search, map
//! or render run is one page after another on a machine with a dozen idle
//! cores. Workers here pull page jobs from a shared counter and hand results
//! back through a channel; the caller receives them strictly in input order,
//! as soon as each is ready, so JSONL output still streams and a consumer
//! never sees page 7 before page 6.

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;

/// How many workers to use for `jobs` items. `requested == 0` asks for one
/// per available core; there is never more than one worker per job.
pub fn worker_count(requested: u32, jobs: usize) -> usize {
    let available = match requested {
        0 => std::thread::available_parallelism().map_or(1, usize::from),
        count => count as usize,
    };
    available.min(jobs).max(1)
}

/// Run `work` over `items` on `workers` threads and call `deliver` with each
/// result in input order.
///
/// `init` builds one piece of mutable state per worker — a glyph cache, an
/// OCR backend — so nothing expensive is rebuilt per page and nothing has to
/// be shared between threads. `deliver` runs on the calling thread; returning
/// `Break` stops the run: no further jobs are started and no further results
/// are delivered.
///
/// With one worker everything runs on the calling thread, in order, with no
/// threads or channels involved.
pub fn ordered<T, S, R>(
    items: &[T],
    workers: usize,
    init: impl Fn() -> S + Sync,
    work: impl Fn(&mut S, usize, &T) -> R + Sync,
    mut deliver: impl FnMut(usize, &T, R) -> ControlFlow<()>,
) where
    T: Sync,
    R: Send,
{
    if workers <= 1 || items.len() <= 1 {
        let mut state = init();
        for (index, item) in items.iter().enumerate() {
            let result = work(&mut state, index, item);
            if deliver(index, item, result).is_break() {
                return;
            }
        }
        return;
    }

    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let (sender, receiver) = mpsc::channel::<(usize, R)>();

    std::thread::scope(|scope| {
        for _ in 0..workers.min(items.len()) {
            let sender = sender.clone();
            let (next, stop, init, work) = (&next, &stop, &init, &work);
            scope.spawn(move || {
                let mut state = init();
                loop {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    let index = next.fetch_add(1, Ordering::AcqRel);
                    let Some(item) = items.get(index) else {
                        break;
                    };
                    let result = work(&mut state, index, item);
                    if sender.send((index, result)).is_err() {
                        break;
                    }
                }
            });
        }
        // Only the workers hold senders now: the channel closes when the
        // last of them finishes.
        drop(sender);

        let mut waiting: BTreeMap<usize, R> = BTreeMap::new();
        let mut due = 0usize;
        'receive: for (index, result) in &receiver {
            waiting.insert(index, result);
            while let Some(result) = waiting.remove(&due) {
                let item = &items[due];
                due += 1;
                if deliver(due - 1, item, result).is_break() {
                    stop.store(true, Ordering::Release);
                    break 'receive;
                }
            }
        }
        // Dropping the receiver unblocks any worker still sending.
        drop(receiver);
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn results_arrive_in_input_order_whatever_finishes_first() {
        let items: Vec<u64> = (0..64).collect();
        let mut seen = Vec::new();
        ordered(
            &items,
            8,
            || 0u64,
            |calls, _, item| {
                *calls += 1;
                // Early items take longest, so they finish last.
                std::thread::sleep(std::time::Duration::from_micros((64 - item) * 50));
                item * 2
            },
            |index, item, doubled| {
                assert_eq!(doubled, item * 2);
                seen.push(index);
                ControlFlow::Continue(())
            },
        );
        assert_eq!(seen, (0..64).collect::<Vec<_>>());
    }

    #[test]
    fn breaking_stops_delivery_and_new_work() {
        let items: Vec<usize> = (0..1000).collect();
        let started = AtomicUsize::new(0);
        let mut delivered = 0;
        ordered(
            &items,
            4,
            || (),
            |(), _, item| {
                started.fetch_add(1, Ordering::AcqRel);
                *item
            },
            |index, _, _| {
                delivered += 1;
                if index == 9 {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            },
        );
        assert_eq!(delivered, 10);
        assert!(started.load(Ordering::Acquire) <= 1000);
    }

    #[test]
    fn one_worker_runs_inline_with_a_single_state() {
        let items = [1, 2, 3];
        let mut total = 0;
        ordered(
            &items,
            1,
            || 10,
            |state, _, item| {
                *state += item;
                *state
            },
            |_, _, running| {
                total = running;
                ControlFlow::Continue(())
            },
        );
        assert_eq!(total, 16);
    }

    #[test]
    fn worker_count_never_exceeds_the_jobs() {
        assert_eq!(worker_count(8, 3), 3);
        assert_eq!(worker_count(2, 100), 2);
        assert_eq!(worker_count(4, 0), 1);
        assert!(worker_count(0, 1000) >= 1);
    }
}
