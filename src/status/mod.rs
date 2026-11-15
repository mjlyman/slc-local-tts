//! Live status the tray and the settings window show: what the server is
//! speaking, when SLC last asked for something, whether SLC has ever
//! connected, and the recent requests.

pub mod records;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::watch;

use records::{RecordLog, RequestRecord, Route};

/// How long a failed announcement keeps counting as a recent failure.
pub const FAILURE_WINDOW: Duration = Duration::from_secs(3600);

/// Milliseconds since the Unix epoch, or 0 for a time before it.
pub fn unix_ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// The inverse of [`unix_ms`].
pub fn from_unix_ms(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms)
}

/// A line the speech engine is working on right now.
#[derive(Clone, Debug, PartialEq)]
pub struct Speaking {
    pub voice_id: &'static str,
    /// Length of the prepared text, in characters.
    pub chars: usize,
    pub started: SystemTime,
}

/// A copy of the live status at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveSnapshot {
    pub speaking: Option<Speaking>,
    pub last_request: Option<SystemTime>,
    /// True once SLC has ever sent a request, in this run or an earlier one.
    pub ever_connected: bool,
    /// The `seq` of the newest request record (0 before the first).
    pub latest_seq: u64,
}

/// The status fields other than the records.
#[derive(Debug, Default)]
struct Status {
    speaking: Option<Speaking>,
    last_request: Option<SystemTime>,
    ever_connected: bool,
}

/// The server's live status, shared by the request handlers, the tray and
/// the private routes the settings window reads.
///
/// The fields are behind std (blocking) mutexes rather than tokio's async
/// ones. Every method locks for a few field reads or writes and never holds
/// a lock across an `.await`, so a lock is never held long enough to stall
/// an async worker. So the methods can be called from plain threads (the
/// tray) and blocking tasks (the speech engine) as well as async code,
/// except [`Live::request_arrived`], which needs the tokio runtime.
pub struct Live {
    status: Mutex<Status>,
    records: Mutex<RecordLog>,
    /// Where to note that SLC has ever connected; `None` in tests.
    seen_marker: Option<PathBuf>,
    /// A change counter. Anything that wants to hear about changes holds a
    /// receiver from [`Live::subscribe`] and wakes when the counter moves.
    changes: watch::Sender<u64>,
}

impl Live {
    /// A fresh status. `ever_connected` starts true if `seen_marker` exists,
    /// so it remembers SLC across restarts.
    pub fn new(seen_marker: Option<PathBuf>) -> Arc<Live> {
        let ever_connected = seen_marker.as_ref().is_some_and(|path| path.exists());
        Arc::new(Live {
            status: Mutex::new(Status {
                ever_connected,
                ..Status::default()
            }),
            records: Mutex::new(RecordLog::default()),
            seen_marker,
            changes: watch::Sender::new(0),
        })
    }

    /// Notes that a request arrived at `at`. The first time SLC ever asks,
    /// this also writes the marker file, on a blocking thread so the request
    /// isn't held up by the disk. Must be called inside a tokio runtime.
    pub fn request_arrived(&self, at: SystemTime) {
        let first_ever = {
            let mut status = self.lock_status();
            status.last_request = Some(at);
            !std::mem::replace(&mut status.ever_connected, true)
        };
        if first_ever && let Some(marker) = self.seen_marker.clone() {
            // A failed write only means "Connect SLC" shows again after a
            // restart, so it's a warning and never an error for SLC.
            tokio::task::spawn_blocking(move || {
                if let Err(err) = std::fs::write(&marker, b"") {
                    tracing::warn!(
                        path = %marker.display(),
                        error = %err,
                        "Couldn't note that SLC has connected; the settings window may show its \
                         \"Connect SLC\" steps again after a restart."
                    );
                }
            });
        }
        self.notify();
    }

    /// Marks `voice_id` as speaking until the returned guard is dropped.
    ///
    /// `self: &Arc<Self>` means this is called on an `Arc<Live>`: the guard
    /// keeps its own clone of the `Arc`, so it can move into a blocking task
    /// that outlives the caller.
    pub fn start_speaking(self: &Arc<Self>, voice_id: &'static str, chars: usize) -> SpeakingGuard {
        self.lock_status().speaking = Some(Speaking {
            voice_id,
            chars,
            started: SystemTime::now(),
        });
        self.notify();
        SpeakingGuard {
            live: Arc::clone(self),
        }
    }

    /// Stores a finished request and returns its `seq`, counting from 1.
    /// The text is cut to `records::TEXT_CHARS` characters, and the oldest
    /// record goes once there are more than `records::CAPACITY`.
    pub fn record(&self, record: RequestRecord) -> u64 {
        let seq = self.lock_records().push(record);
        self.notify();
        seq
    }

    /// The records newer than `seq`, ascending by `seq`.
    pub fn records_after(&self, seq: u64) -> Vec<RequestRecord> {
        self.lock_records().after(seq)
    }

    /// How many announcements failed within [`FAILURE_WINDOW`] before
    /// `now`. Previews don't count: SLC never caches them, so a failed
    /// preview costs the user nothing.
    pub fn recent_failures(&self, now: SystemTime) -> u32 {
        let records = self.lock_records();
        recent_failure_records(&records, now).count() as u32
    }

    /// When the next failure [`recent_failures`](Self::recent_failures)
    /// counts leaves the window, which lowers that count: the earliest
    /// arrival among them plus [`FAILURE_WINDOW`]. `None` if it counts none.
    pub fn next_failure_expiry(&self, now: SystemTime) -> Option<SystemTime> {
        let records = self.lock_records();
        recent_failure_records(&records, now)
            .map(|record| record.at_ms)
            .min()
            .map(|at_ms| from_unix_ms(at_ms) + FAILURE_WINDOW)
    }

    pub fn snapshot(&self) -> LiveSnapshot {
        // Read the records' seq first and drop that lock before taking the
        // status lock, so no method ever holds both.
        let latest_seq = self.lock_records().latest_seq();
        let status = self.lock_status();
        LiveSnapshot {
            speaking: status.speaking.clone(),
            last_request: status.last_request,
            ever_connected: status.ever_connected,
            latest_seq,
        }
    }

    /// A receiver that wakes (`changed().await`) whenever the status changes.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// Bumps the change counter, waking every subscriber.
    pub fn notify(&self) {
        // `send_modify` updates the value even when nobody is subscribed
        // (plain `send` would fail then), so a later subscriber starts from
        // the right count.
        self.changes.send_modify(|generation| *generation += 1);
    }

    fn lock_status(&self) -> MutexGuard<'_, Status> {
        // A panic while holding the lock "poisons" it. The data is a few
        // plain fields that can't be left half-written, so carry on with it.
        self.status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_records(&self) -> MutexGuard<'_, RecordLog> {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The failed announcements that arrived less than [`FAILURE_WINDOW`]
/// before `now`. Shared by `recent_failures` and `next_failure_expiry`, so
/// the count and its next change always agree.
fn recent_failure_records(
    records: &RecordLog,
    now: SystemTime,
) -> impl Iterator<Item = &RequestRecord> {
    records.iter().filter(move |record| {
        record.route == Route::Speech
            && record.failed()
            && from_unix_ms(record.at_ms) + FAILURE_WINDOW > now
    })
}

/// Clears "speaking" when dropped. See [`Live::start_speaking`].
#[must_use = "\"speaking\" clears as soon as the guard is dropped"]
pub struct SpeakingGuard {
    live: Arc<Live>,
}

impl Drop for SpeakingGuard {
    fn drop(&mut self) {
        self.live.lock_status().speaking = None;
        self.live.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::error::BudgetPhase;
    use records::{RequestRecord, Route};

    /// A fixed "now", well after the Unix epoch.
    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(10 * 24 * 3600)
    }

    fn minutes_ago(minutes: u64) -> SystemTime {
        now() - Duration::from_secs(minutes * 60)
    }

    fn record(route: Route, outcome: &str, at: SystemTime) -> RequestRecord {
        RequestRecord {
            seq: 0,
            at_ms: unix_ms(at),
            route,
            voice_id: "M1".to_string(),
            text: "Hello".to_string(),
            outcome: outcome.to_string(),
            phase: None,
            reason: None,
            message: None,
            audio_seconds: None,
            ms: 10,
        }
    }

    #[test]
    fn recent_failures_counts_speech_failures_in_last_hour() {
        let live = Live::new(None);
        live.record(record(Route::Speech, "budget_exceeded", minutes_ago(30)));
        assert_eq!(live.recent_failures(now()), 1);

        let live = Live::new(None);
        live.record(record(Route::Speech, "budget_exceeded", minutes_ago(61)));
        assert_eq!(live.recent_failures(now()), 0);

        let live = Live::new(None);
        let mut preview = record(Route::Preview, "budget_exceeded", minutes_ago(5));
        preview.phase = Some(BudgetPhase::Downloading);
        live.record(preview);
        live.record(record(Route::Speech, "spoken", minutes_ago(5)));
        live.record(record(Route::Speech, "silent", minutes_ago(5)));
        assert_eq!(live.recent_failures(now()), 0);
    }

    #[test]
    fn next_failure_expiry_is_an_hour_after_oldest_counted_failure() {
        let live = Live::new(None);
        assert_eq!(live.next_failure_expiry(now()), None);
        live.record(record(Route::Speech, "synthesis_failed", minutes_ago(50)));
        live.record(record(Route::Speech, "budget_exceeded", minutes_ago(20)));
        // Not counted: a success and a failed preview, both older.
        live.record(record(Route::Speech, "spoken", minutes_ago(55)));
        live.record(record(Route::Preview, "internal_error", minutes_ago(55)));
        let minutes = |m: u64| Duration::from_secs(m * 60);
        // The 50-minute-old failure leaves the window first.
        assert_eq!(live.next_failure_expiry(now()), Some(now() + minutes(10)));
        // Once it has gone, the 20-minute-old one is next.
        let later = now() + minutes(11);
        assert_eq!(live.recent_failures(later), 1);
        assert_eq!(live.next_failure_expiry(later), Some(now() + minutes(40)));
        // Once every failure has left the window, there is nothing to wait for.
        assert_eq!(live.next_failure_expiry(now() + FAILURE_WINDOW), None);
    }

    #[tokio::test]
    async fn speaking_guard_sets_clears_and_notifies() {
        let live = Live::new(None);
        let mut changes = live.subscribe();
        assert!(live.snapshot().speaking.is_none());

        let guard = live.start_speaking("M1", 42);
        changes.changed().await.unwrap();
        let speaking = live.snapshot().speaking.unwrap();
        assert_eq!(speaking.voice_id, "M1");
        assert_eq!(speaking.chars, 42);

        drop(guard);
        changes.changed().await.unwrap();
        assert!(live.snapshot().speaking.is_none());
    }

    #[test]
    fn record_assigns_seq_and_notifies() {
        let live = Live::new(None);
        let changes = live.subscribe();
        assert_eq!(live.record(record(Route::Speech, "spoken", now())), 1);
        assert_eq!(live.record(record(Route::Speech, "spoken", now())), 2);
        assert!(changes.has_changed().unwrap());
        assert_eq!(live.snapshot().latest_seq, 2);
        let seqs: Vec<u64> = live.records_after(1).iter().map(|r| r.seq).collect();
        assert_eq!(seqs, vec![2]);
    }

    #[test]
    fn marker_written_once() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("slc-connected");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let live = Live::new(Some(marker.clone()));
            assert!(!live.snapshot().ever_connected);
            live.request_arrived(now());
            let snapshot = live.snapshot();
            assert!(snapshot.ever_connected);
            assert_eq!(snapshot.last_request, Some(now()));
            // The write runs on a blocking thread; give it a moment.
            for _ in 0..200 {
                if marker.is_file() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(marker.is_file());
        });
        let again = Live::new(Some(marker));
        assert!(again.snapshot().ever_connected);
    }
}
