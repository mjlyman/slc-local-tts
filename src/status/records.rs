//! What the server remembers about recent requests, for the Activity list.
//! Kept in memory only: a restart starts an empty list.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::api::error::BudgetPhase;

/// How many records are kept; older ones are dropped.
pub const CAPACITY: usize = 200;
/// Longest piece of a request's text a record keeps, in characters.
pub const TEXT_CHARS: usize = 160;

/// Which route a request came in on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    /// SLC speaking an announcement.
    Speech,
    /// A voice preview.
    Preview,
}

/// One finished request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RequestRecord {
    /// Counts from 1 in the order requests finished. Assigned by
    /// [`RecordLog::push`]; whatever the caller sets is replaced.
    pub seq: u64,
    /// When the request arrived, in milliseconds since the Unix epoch.
    pub at_ms: u64,
    pub route: Route,
    pub voice_id: String,
    /// The cleaned text, cut to [`TEXT_CHARS`] characters.
    pub text: String,
    /// `spoken`, `silent` or an error code (`AppError::code`).
    pub outcome: String,
    /// For `budget_exceeded`: where the time ran out.
    pub phase: Option<BudgetPhase>,
    /// The error's own detail, when it has one (for example "no internet").
    pub reason: Option<String>,
    /// The error's user-facing message.
    pub message: Option<String>,
    /// Length of the audio sent, for a spoken line.
    pub audio_seconds: Option<f32>,
    /// How long the request took, in milliseconds.
    pub ms: u64,
}

impl RequestRecord {
    /// True for a failure: anything that wasn't spoken and wasn't
    /// legitimately silent.
    pub fn failed(&self) -> bool {
        self.outcome != "spoken" && self.outcome != "silent"
    }
}

/// The newest [`CAPACITY`] records, oldest first.
#[derive(Debug, Default)]
pub(crate) struct RecordLog {
    records: VecDeque<RequestRecord>,
    /// The `seq` of the newest record ever pushed (0 before the first).
    latest_seq: u64,
}

impl RecordLog {
    /// Stores `record` with the next `seq` and its text cut to
    /// [`TEXT_CHARS`], dropping the oldest record when full. Returns the seq.
    pub(crate) fn push(&mut self, mut record: RequestRecord) -> u64 {
        self.latest_seq += 1;
        record.seq = self.latest_seq;
        // Cut by characters, not bytes: a byte cut could split a
        // multi-byte character such as "é".
        if record.text.chars().count() > TEXT_CHARS {
            record.text = record.text.chars().take(TEXT_CHARS).collect();
        }
        if self.records.len() == CAPACITY {
            self.records.pop_front();
        }
        self.records.push_back(record);
        self.latest_seq
    }

    /// Copies of the records newer than `seq`, ascending by `seq`.
    pub(crate) fn after(&self, seq: u64) -> Vec<RequestRecord> {
        self.records
            .iter()
            .filter(|record| record.seq > seq)
            .cloned()
            .collect()
    }

    /// The records, oldest first.
    pub(crate) fn iter(&self) -> impl DoubleEndedIterator<Item = &RequestRecord> {
        self.records.iter()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }

    pub(crate) fn latest_seq(&self) -> u64 {
        self.latest_seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(text: &str) -> RequestRecord {
        RequestRecord {
            seq: 0,
            at_ms: 1_000,
            route: Route::Speech,
            voice_id: "M1".to_string(),
            text: text.to_string(),
            outcome: "spoken".to_string(),
            phase: None,
            reason: None,
            message: None,
            audio_seconds: Some(1.5),
            ms: 20,
        }
    }

    #[test]
    fn records_keep_last_200() {
        let mut log = RecordLog::default();
        for _ in 0..205 {
            log.push(record("Hello"));
        }
        assert_eq!(log.len(), CAPACITY);
        assert_eq!(log.iter().next().unwrap().seq, 6);
        assert_eq!(log.after(0).len(), 200);
        assert_eq!(log.latest_seq(), 205);
    }

    #[test]
    fn records_after_returns_newer_ascending() {
        let mut log = RecordLog::default();
        for _ in 0..5 {
            log.push(record("Hello"));
        }
        let seqs: Vec<u64> = log.after(2).iter().map(|r| r.seq).collect();
        assert_eq!(seqs, vec![3, 4, 5]);
        assert!(log.after(5).is_empty());
    }

    #[test]
    fn record_text_cut_to_160_chars() {
        let mut log = RecordLog::default();
        let seq = log.push(record(&"é".repeat(200)));
        assert_eq!(seq, 1);
        let kept = &log.after(0)[0].text;
        assert_eq!(kept.chars().count(), TEXT_CHARS);
        assert!(kept.chars().all(|c| c == 'é'));
    }

    #[test]
    fn record_serializes_in_snake_case() {
        let mut failed = record("Hi");
        failed.route = Route::Preview;
        failed.phase = Some(BudgetPhase::Downloading);
        let json = serde_json::to_value(&failed).unwrap();
        assert_eq!(json["route"], "preview");
        assert_eq!(json["phase"], "downloading");
        let back: RequestRecord = serde_json::from_value(json).unwrap();
        assert_eq!(back, failed);
    }
}
