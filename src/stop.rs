//! Telling the server to stop, and learning that it has.
//!
//! [`StopSignal`] carries the request: Ctrl+C in the console program, Quit
//! in the tray, Windows ending the session, or "Restart and update now".
//! [`ServedFlag`] carries the answer back to a thread outside the async
//! runtime (such as the tray's message loop), which can wait for it with a
//! time limit.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::watch;

/// Why the server was asked to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// Ctrl+C in the console program.
    CtrlC,
    /// The user picked Quit.
    Quit,
    /// Windows is signing out or shutting down.
    SessionEnding,
    /// The user chose "Restart and update now": stop, install the
    /// downloaded update, and start again.
    RestartToUpdate,
}

/// A one-shot "please stop" that any thread can send and any task can wait
/// for. Clones share one signal.
///
/// It wraps a tokio `watch` channel's sender, which holds the current value
/// (`None` until a stop is requested). Waiting subscribes a fresh receiver,
/// so any number of tasks can wait, and a wait that starts after the request
/// still sees it. The sender lives in an `Arc`, so every clone talks to the
/// same channel, and the channel can't close while a `StopSignal` exists.
#[derive(Debug, Clone)]
pub struct StopSignal {
    sender: Arc<watch::Sender<Option<StopReason>>>,
}

impl StopSignal {
    pub fn new() -> StopSignal {
        // The first receiver is dropped at once: `wait` makes its own.
        let (sender, _receiver) = watch::channel(None);
        StopSignal {
            sender: Arc::new(sender),
        }
    }

    /// Asks the server to stop. Only the first request counts; later ones
    /// change nothing, so the reason reported is the one that started it.
    pub fn request(&self, reason: StopReason) {
        // `send_if_modified` works even with no receiver, and it only wakes
        // waiters when the closure returns true.
        self.sender.send_if_modified(|current| {
            if current.is_some() {
                return false;
            }
            *current = Some(reason);
            true
        });
    }

    /// The reason a stop was requested, if one was.
    pub fn requested(&self) -> Option<StopReason> {
        *self.sender.borrow()
    }

    /// Waits until a stop is requested, and returns its reason. Returns at
    /// once if one already was.
    pub async fn wait(&self) -> StopReason {
        let mut receiver = self.sender.subscribe();
        // `wait_for` checks the current value first, then each new one. It
        // fails only if the sender is gone, and `self` holds it. It returns
        // a read lock on the value; `*reason` copies the value out so the
        // lock is released at once, before anything else is awaited.
        let reason = receiver
            .wait_for(Option::is_some)
            .await
            .map(|reason| *reason);
        match reason {
            Ok(Some(reason)) => reason,
            // Neither can happen (see above); never resolving is the safe
            // answer, since it never stops the server by mistake.
            Ok(None) | Err(_) => std::future::pending().await,
        }
    }
}

impl Default for StopSignal {
    fn default() -> StopSignal {
        StopSignal::new()
    }
}

/// A flag that is set once, when the server has finished serving, and that
/// a plain thread can wait for with a time limit. Clones share one flag.
///
/// This is the classic condition-variable pattern, as with C++'s
/// `std::condition_variable` or Java's `wait`/`notifyAll`: the `bool` lives
/// in a `Mutex`, and the `Condvar` lets a waiter sleep until another thread
/// changes it. The waiter always re-checks the `bool` under the lock, so a
/// wake-up that comes early or for nothing ("spurious") does no harm, and a
/// `set` that happens before the wait starts is never missed.
#[derive(Debug, Clone, Default)]
pub struct ServedFlag {
    inner: Arc<(Mutex<bool>, Condvar)>,
}

impl ServedFlag {
    /// Marks the server as stopped and wakes every waiter.
    pub fn set(&self) {
        let (served, wake) = &*self.inner;
        *lock(served) = true;
        wake.notify_all();
    }

    /// Waits up to `limit` for [`set`](ServedFlag::set). True if it was set
    /// in time (or already), false if the time ran out.
    pub fn wait_timeout(&self, limit: Duration) -> bool {
        let (served, wake) = &*self.inner;
        // `wait_timeout_while` releases the lock while it sleeps and takes
        // it back before each check, looping until the closure says stop
        // waiting or the time is up.
        let (served, _timed_out) = wake
            .wait_timeout_while(lock(served), limit, |served| !*served)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *served
    }
}

/// A poisoned lock only means a thread panicked while holding it; a `bool`
/// can't be left half-written, so it is safe to keep using.
fn lock(flag: &Mutex<bool>) -> MutexGuard<'_, bool> {
    flag.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_reason_wins() {
        let stop = StopSignal::new();
        assert_eq!(stop.requested(), None);
        stop.request(StopReason::SessionEnding);
        stop.clone().request(StopReason::Quit);
        assert_eq!(stop.requested(), Some(StopReason::SessionEnding));
    }

    #[tokio::test]
    async fn wait_resolves_after_request() {
        // Requested while a task is already waiting.
        let stop = StopSignal::new();
        let waiter = tokio::spawn({
            let stop = stop.clone();
            async move { stop.wait().await }
        });
        tokio::task::yield_now().await;
        stop.request(StopReason::Quit);
        assert_eq!(waiter.await.unwrap(), StopReason::Quit);

        // Requested before anyone waits.
        let early = StopSignal::new();
        early.request(StopReason::CtrlC);
        let reason = tokio::time::timeout(Duration::from_secs(5), early.wait())
            .await
            .expect("an earlier request must end the wait at once");
        assert_eq!(reason, StopReason::CtrlC);
    }

    #[test]
    fn served_flag_wait_times_out_then_succeeds() {
        let flag = ServedFlag::default();
        assert!(!flag.wait_timeout(Duration::from_millis(20)));

        let setter = flag.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            setter.set();
        });
        assert!(flag.wait_timeout(Duration::from_secs(5)));
        thread.join().unwrap();
        // Once set, it stays set.
        assert!(flag.wait_timeout(Duration::ZERO));
    }
}
