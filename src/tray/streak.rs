//! Logging a failure once, however often it repeats.
//!
//! The tray changes its icon and tooltip on every status change, a hundred
//! times during one download. If Windows refuses each change, one warning
//! says so; the same failure again only goes to the debug log, until a
//! change succeeds or the failure changes.

/// One tray call's run of failures.
#[derive(Debug, Default)]
pub struct FailureStreak {
    /// The failure the last call ended in, while it keeps failing.
    last: Option<String>,
}

impl FailureStreak {
    /// Records a failure. True if it should be logged as a warning: the
    /// first failure after a success, or one unlike the last.
    pub fn failed(&mut self, error: &str) -> bool {
        if self.last.as_deref() == Some(error) {
            return false;
        }
        self.last = Some(error.to_string());
        true
    }

    /// Records a success, so the next failure warns again.
    pub fn succeeded(&mut self) {
        self.last = None;
    }

    /// Logs `result`'s failure as `what` (once per streak, as above), and
    /// records its outcome.
    pub fn record<E: std::fmt::Display>(&mut self, what: &str, result: Result<(), E>) {
        match result {
            Ok(()) => self.succeeded(),
            Err(e) => {
                let error = e.to_string();
                if self.failed(&error) {
                    tracing::warn!("{what} ({error})");
                } else {
                    tracing::debug!("{what} again ({error})");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_warn_once() {
        let mut streak = FailureStreak::default();
        assert!(streak.failed("Unspecified error"));
        assert!(!streak.failed("Unspecified error"));
        assert!(!streak.failed("Unspecified error"));
    }

    #[test]
    fn a_success_ends_the_streak() {
        let mut streak = FailureStreak::default();
        assert!(streak.failed("Unspecified error"));
        streak.succeeded();
        assert!(streak.failed("Unspecified error"));
    }

    #[test]
    fn a_different_failure_warns() {
        let mut streak = FailureStreak::default();
        assert!(streak.failed("Unspecified error"));
        assert!(streak.failed("Access is denied"));
    }

    #[test]
    fn record_tracks_results() {
        let mut streak = FailureStreak::default();
        streak.record("couldn't", Err::<(), _>("boom"));
        assert!(!streak.failed("boom"), "already warned");
        streak.record("couldn't", Ok::<(), &str>(()));
        assert!(streak.failed("boom"), "warns again after a success");
    }
}
