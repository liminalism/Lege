//! Wall-clock budget for one command or one service request.
//!
//! The budget is cooperative. Arming it starts a watchdog that raises one
//! shared flag when the time is up; page compilation and rasterization
//! already poll such a flag at operation boundaries, and the page loops check
//! it between pages. Work that polls nothing — an OCR call, the document
//! health scan behind `inspect` — runs to completion and is caught at the
//! next page boundary.
//!
//! The flag is process-wide rather than threaded through every call: the tool
//! serves one request at a time, and every `ParseContext` and render request
//! it builds belongs to that request.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

static CURRENT: OnceLock<Mutex<Armed>> = OnceLock::new();

#[derive(Clone)]
struct Armed {
    flag: Arc<AtomicBool>,
    timeout_secs: u64,
}

fn current() -> Armed {
    let slot = CURRENT.get_or_init(|| {
        Mutex::new(Armed {
            flag: Arc::new(AtomicBool::new(false)),
            timeout_secs: 0,
        })
    });
    match slot.lock() {
        Ok(armed) => armed.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// Keeps the budget armed. Dropping it stops the watchdog, so a request that
/// finishes early leaves no thread behind and cannot trip a later request.
#[derive(Debug)]
pub struct DeadlineGuard {
    _disarm: Option<mpsc::Sender<()>>,
}

/// Start the budget for the work that follows. `0` means no limit.
///
/// Each call installs a fresh flag, so a stale watchdog from an earlier
/// request can only raise a flag nothing reads any more.
pub fn arm(timeout_secs: u64) -> DeadlineGuard {
    let flag = Arc::new(AtomicBool::new(false));
    let armed = Armed {
        flag: Arc::clone(&flag),
        timeout_secs,
    };
    let slot = CURRENT.get_or_init(|| Mutex::new(armed.clone()));
    match slot.lock() {
        Ok(mut current) => *current = armed,
        Err(poisoned) => *poisoned.into_inner() = armed,
    }
    if timeout_secs == 0 {
        return DeadlineGuard { _disarm: None };
    }

    let (disarm, disarmed) = mpsc::channel::<()>();
    let spawned = std::thread::Builder::new()
        .name("lege-pdf-deadline".into())
        .spawn(move || {
            // The guard never sends: a disconnect means the work finished.
            if disarmed.recv_timeout(Duration::from_secs(timeout_secs))
                == Err(RecvTimeoutError::Timeout)
            {
                flag.store(true, Ordering::Release);
            }
        });
    DeadlineGuard {
        // No watchdog means no limit, which is the pre-existing behaviour.
        _disarm: spawned.ok().map(|_| disarm),
    }
}

/// Whether the armed budget has run out.
pub fn expired() -> bool {
    current().flag.load(Ordering::Acquire)
}

/// The flag to hand to a `ParseContext`.
pub fn flag() -> Arc<AtomicBool> {
    current().flag
}

/// The same flag as a render cancellation token.
pub fn render_token() -> pdf_render_api::CancellationToken {
    pdf_render_api::CancellationToken::from_shared(flag())
}

/// One sentence saying the budget ran out and what was left undone.
pub fn message(done: usize, selected: usize) -> String {
    format!(
        "timeout of {}s reached after {done} of {selected} selected page(s); the rest were not processed (raise --timeout, or pass 0 for no limit)",
        current().timeout_secs
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test: the budget is process-wide, so separate tests would race.
    #[test]
    fn a_budget_expires_once_and_a_new_one_starts_clean() {
        let unlimited = arm(0);
        assert!(!expired());
        drop(unlimited);

        let guard = arm(1);
        let stale = flag();
        assert!(!expired());
        std::thread::sleep(Duration::from_millis(1300));
        assert!(expired());
        assert!(message(2, 5).contains("timeout of 1s reached after 2 of 5"));
        drop(guard);

        let _next = arm(30);
        assert!(!expired(), "a new request must not inherit an expired flag");
        assert!(stale.load(Ordering::Acquire));
    }
}
