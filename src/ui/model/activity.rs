//! What the Activity page shows: one entry per request the server
//! remembers, newest first, each with a one-line summary and, for a
//! failure, why it failed in plain English.

use crate::api::error::BudgetPhase;
use crate::names::voice_name;
use crate::status::records::{RequestRecord, Route};

/// Follows the reason for every failed announcement. SLC never keeps a
/// failed answer, so the user learns it will try again by itself.
pub const NOT_KEPT: &str = "SLC played its failure sound and didn't keep it, so it asks again the next time this announcement comes up.";

/// How much of a line's text an entry shows, in characters.
const TEXT_START_CHARS: usize = 60;

/// One request, as the Activity list shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// The record's `seq`, so the page can pick out the highlighted one.
    pub seq: u64,
    /// When the request arrived, as `time_of` wrote it.
    pub time: String,
    /// The voice's name, or its ID when the catalog doesn't know it.
    pub voice: String,
    /// The start of the line's text.
    pub text_start: String,
    /// What happened, in one line.
    pub summary: String,
    /// True for a failure, which the page shows in the caution colour.
    pub problem: bool,
    /// Why it failed, for a failure.
    pub reason: Option<String>,
}

/// The entries for `records` (oldest first, as the server sends them),
/// newest first. With `problems_only`, only the failures. `time_of` turns a
/// record's Unix milliseconds into the time shown.
pub fn entries(
    records: &[RequestRecord],
    problems_only: bool,
    time_of: impl Fn(u64) -> String,
) -> Vec<Entry> {
    records
        .iter()
        .rev()
        .filter(|r| !problems_only || r.failed())
        .map(|r| Entry {
            seq: r.seq,
            time: time_of(r.at_ms),
            voice: voice_name(&r.voice_id),
            text_start: text_start(&r.text),
            summary: summary(r),
            problem: r.failed(),
            reason: r.failed().then(|| failure_reason(r)),
        })
        .collect()
}

/// Why `r` failed, in plain English. For an announcement, it goes on to
/// say that SLC will ask again ([`NOT_KEPT`]); SLC never keeps a preview,
/// so a preview's reason stops before that.
pub fn failure_reason(r: &RequestRecord) -> String {
    // `as_deref` turns the `Option<String>` into an `Option<&str>`, so
    // nothing is copied until `format!` builds the text.
    let reason = r.reason.as_deref().unwrap_or("no reason was given");
    let text = match r.outcome.as_str() {
        "voice_not_found" => format!(
            "SLC asked for a voice called \"{}\", which this version doesn't have. \
             Pick another voice in SLC.",
            r.voice_id
        ),
        // A budget miss always records where the time ran out; without
        // that, the voice still downloading is the likelier cause.
        "budget_exceeded" if r.phase == Some(BudgetPhase::Speaking) => {
            "This announcement was too long for the voice to finish within SLC's \
             10-second limit. Kokoro voices are the slowest; Piper and Supertonic voices \
             handle long announcements."
                .to_string()
        }
        "budget_exceeded" => "The voice wasn't installed yet, and SLC waits only 10 seconds \
             for a line. The download keeps going, and later announcements will speak once it \
             finishes. Install voices on the Voices page before a flight."
            .to_string(),
        "model_download_failed" => format!(
            "The voice couldn't be downloaded: {reason}. \
             Press Download on the Voices page to try again."
        ),
        "model_load_failed" => format!("The voice's files couldn't be opened: {reason}."),
        "synthesis_failed" | "internal_error" => "Speech generation failed for this line. \
             If it keeps happening with this voice, send the log folder to the developer."
            .to_string(),
        code => format!(
            "Something went wrong: {}",
            r.message.as_deref().unwrap_or(code)
        ),
    };
    match r.route {
        Route::Speech => format!("{text} {NOT_KEPT}"),
        Route::Preview => text,
    }
}

/// The first [`TEXT_START_CHARS`] characters of `text`, with "…" when
/// there's more.
fn text_start(text: &str) -> String {
    // Counted in characters, not bytes, so "é" is never cut in half.
    if text.chars().count() > TEXT_START_CHARS {
        let start: String = text.chars().take(TEXT_START_CHARS).collect();
        format!("{start}…")
    } else {
        text.to_string()
    }
}

/// What happened to `r`, in one line.
fn summary(r: &RequestRecord) -> String {
    match (r.outcome.as_str(), r.route) {
        ("spoken", _) => format!(
            "Spoke {:.1} s in {:.1} s",
            r.audio_seconds.unwrap_or(0.0),
            r.ms as f64 / 1000.0
        ),
        ("silent", _) => "Nothing to say: the line had no letters or digits".to_string(),
        (_, Route::Speech) => "Failed: SLC played its failure sound".to_string(),
        (_, Route::Preview) => "Preview played as silence".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(seq: u64, route: Route, outcome: &str) -> RequestRecord {
        RequestRecord {
            seq,
            at_ms: 1_000 * seq,
            route,
            voice_id: "en_US-joe-medium".to_string(),
            text: "Ladies and gentlemen, welcome aboard.".to_string(),
            outcome: outcome.to_string(),
            phase: None,
            reason: None,
            message: None,
            audio_seconds: None,
            ms: 400,
        }
    }

    /// A preview failure with `outcome`, so the reason has no `NOT_KEPT`.
    fn preview_failure(outcome: &str) -> RequestRecord {
        record(1, Route::Preview, outcome)
    }

    fn time_of(ms: u64) -> String {
        format!("t{ms}")
    }

    #[test]
    fn reason_texts_per_code() {
        let mut r = preview_failure("voice_not_found");
        r.voice_id = "X9".to_string();
        assert_eq!(
            failure_reason(&r),
            "SLC asked for a voice called \"X9\", which this version doesn't have. \
             Pick another voice in SLC."
        );

        let mut r = preview_failure("budget_exceeded");
        r.phase = Some(BudgetPhase::Downloading);
        assert_eq!(
            failure_reason(&r),
            "The voice wasn't installed yet, and SLC waits only 10 seconds for a line. \
             The download keeps going, and later announcements will speak once it finishes. \
             Install voices on the Voices page before a flight."
        );

        r.phase = Some(BudgetPhase::Speaking);
        assert_eq!(
            failure_reason(&r),
            "This announcement was too long for the voice to finish within SLC's \
             10-second limit. Kokoro voices are the slowest; Piper and Supertonic voices \
             handle long announcements."
        );

        let mut r = preview_failure("model_download_failed");
        r.reason = Some("no internet connection".to_string());
        assert_eq!(
            failure_reason(&r),
            "The voice couldn't be downloaded: no internet connection. \
             Press Download on the Voices page to try again."
        );

        let mut r = preview_failure("model_load_failed");
        // The reason the server gives for every load failure.
        r.reason = Some(
            "the files may be damaged; if this keeps happening, delete the voice on the \
             Voices page (or its folder under models\\) so it downloads again"
                .to_string(),
        );
        assert_eq!(
            failure_reason(&r),
            "The voice's files couldn't be opened: the files may be damaged; if this keeps \
             happening, delete the voice on the Voices page (or its folder under models\\) so \
             it downloads again."
        );

        let shared = "Speech generation failed for this line. If it keeps happening with \
                      this voice, send the log folder to the developer.";
        assert_eq!(failure_reason(&preview_failure("synthesis_failed")), shared);
        assert_eq!(failure_reason(&preview_failure("internal_error")), shared);

        let mut r = preview_failure("something_new");
        r.message = Some("The server ran out of disk space.".to_string());
        assert_eq!(
            failure_reason(&r),
            "Something went wrong: The server ran out of disk space."
        );
    }

    #[test]
    fn speech_failure_appends_not_kept() {
        let mut r = record(1, Route::Speech, "budget_exceeded");
        r.phase = Some(BudgetPhase::Speaking);
        let reason = failure_reason(&r);
        assert!(reason.ends_with(&format!("handle long announcements. {NOT_KEPT}")));
        let reason = failure_reason(&record(1, Route::Speech, "synthesis_failed"));
        assert!(reason.ends_with(&format!(" {NOT_KEPT}")));
    }

    #[test]
    fn preview_failure_has_no_not_kept() {
        let reason = failure_reason(&preview_failure("synthesis_failed"));
        assert!(!reason.contains(NOT_KEPT));
        let shown = entries(&[preview_failure("synthesis_failed")], false, time_of);
        assert_eq!(shown[0].summary, "Preview played as silence");
        assert_eq!(shown[0].reason.as_deref(), Some(reason.as_str()));
        assert!(shown[0].problem);
    }

    #[test]
    fn spoken_summary() {
        let mut r = record(1, Route::Speech, "spoken");
        r.audio_seconds = Some(6.1);
        r.ms = 400;
        let shown = entries(&[r], false, time_of);
        assert_eq!(shown[0].summary, "Spoke 6.1 s in 0.4 s");
        assert!(!shown[0].problem);
        assert_eq!(shown[0].reason, None);
        assert_eq!(shown[0].time, "t1000");
        assert_eq!(shown[0].voice, "Joe (US, medium, Piper)");
    }

    #[test]
    fn silent_summary() {
        let shown = entries(&[record(1, Route::Speech, "silent")], false, time_of);
        assert_eq!(
            shown[0].summary,
            "Nothing to say: the line had no letters or digits"
        );
        assert!(!shown[0].problem);
        assert_eq!(shown[0].reason, None);
    }

    #[test]
    fn newest_first_and_problems_only() {
        let records = vec![
            record(1, Route::Speech, "spoken"),
            record(2, Route::Speech, "budget_exceeded"),
            record(3, Route::Speech, "silent"),
            record(4, Route::Preview, "model_load_failed"),
        ];
        let seqs = |shown: Vec<Entry>| shown.iter().map(|e| e.seq).collect::<Vec<_>>();
        assert_eq!(seqs(entries(&records, false, time_of)), vec![4, 3, 2, 1]);
        assert_eq!(seqs(entries(&records, true, time_of)), vec![4, 2]);
        let failed = entries(&records, true, time_of);
        assert_eq!(failed[1].summary, "Failed: SLC played its failure sound");
    }

    #[test]
    fn text_start_cut_at_60() {
        let mut r = record(1, Route::Speech, "spoken");
        // 60 characters, "é" among them, so a byte cut would split one.
        let sixty = "é".repeat(10) + &"a".repeat(50);
        r.text = sixty.clone();
        assert_eq!(entries(&[r.clone()], false, time_of)[0].text_start, sixty);
        r.text = format!("{sixty}b");
        assert_eq!(
            entries(&[r], false, time_of)[0].text_start,
            format!("{sixty}…")
        );
    }
}
