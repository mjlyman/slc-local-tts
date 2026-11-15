//! What the Status page shows: the state line, SLC's address, the trace of
//! the last hour's announcements, the failure notice, and whether the
//! "Connect SLC" steps replace the trace.

use std::time::SystemTime;

use eframe::egui::Color32;

use crate::api::slc::dto::{ActivityDto, StatusDto};
use crate::config::Config;
use crate::names::{format_mb, row_title, voice_name};
use crate::status::records::{RequestRecord, Route};
use crate::status::{FAILURE_WINDOW, from_unix_ms};
use crate::timefmt;
use crate::ui::backend::{Connection, Snapshot};
use crate::ui::theme::Palette;

/// The "Connect SLC" steps, in order. Step 2 names SLC's address, so it's
/// built by [`connect_step_2`] instead.
pub const CONNECT_STEP_1: &str = "In SLC, open the ElevenLabs voice settings.";
pub const CONNECT_STEP_3: &str = "Enter any API key; it isn't checked.";
pub const CONNECT_STEP_4: &str =
    "Refresh the voice list and pick a voice. Press its preview button once to download it.";

/// The second "Connect SLC" step, for SLC's address `address`.
pub fn connect_step_2(address: &str) -> String {
    format!("Set Custom API URL to {address}.")
}

/// Audio this long, or longer, draws a full-height bar.
const FULL_BAR_SECONDS: f32 = 12.0;
/// The shortest bar, as a share of the full height, so even a short line
/// shows.
const MIN_BAR: f32 = 0.08;
/// A failed line's bar height: an outline at half height.
const FAILED_BAR: f32 = 0.5;

/// The lamp beside the headline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lamp {
    /// The server answers and is idle or speaking.
    Ready,
    /// A voice is downloading.
    Busy,
    /// The server has stopped answering.
    Stopped,
    /// No answer yet since the window opened.
    Unknown,
}

impl Lamp {
    /// The lamp's colour in `palette`.
    pub fn color(self, palette: &Palette) -> Color32 {
        match self {
            Lamp::Ready => palette.ok,
            Lamp::Busy => palette.act,
            Lamp::Stopped => palette.warn,
            Lamp::Unknown => palette.ink_dim,
        }
    }
}

/// What one bar of the trace stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarKind {
    Spoken,
    Failed,
    /// The line being spoken right now.
    Speaking,
}

/// One bar of the trace.
#[derive(Clone, Debug, PartialEq)]
pub struct TraceBar {
    /// The record it shows, or `None` for the line being spoken.
    pub seq: Option<u64>,
    /// A share of the trace's full height, 0.08 to 1.
    pub height: f32,
    pub kind: BarKind,
    /// What a screen reader (and the bar's tooltip) says.
    pub label: String,
}

/// Everything the Status page draws.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusView {
    pub lamp: Lamp,
    pub headline: String,
    pub detail: String,
    /// SLC's Custom API URL for this server.
    pub address: String,
    /// True when the server has stopped, to offer starting it again.
    pub start_button: bool,
    /// True until SLC's first request: the setup steps replace the trace.
    pub connect_steps: bool,
    pub failure_notice: Option<String>,
    pub trace: Vec<TraceBar>,
    /// True while the detail says how long ago something was, so the page
    /// redraws now and then for the minutes to tick over.
    pub ticking: bool,
}

/// The Status page for snapshot `s` at time `now`.
pub fn status_view(s: &Snapshot, now: SystemTime) -> StatusView {
    let status = s.status.as_ref();
    let line = state_line(s.connection, status, now);
    // The port the server is listening on, which can differ from the one
    // in config.toml until it restarts. Before its first answer, the
    // usual port.
    let port = status.map_or(Config::default().port, |status| status.port);
    // Only a server that answers is speaking: a stopped one's last status
    // may still say it was.
    let speaking = s.connection == Connection::Connected
        && matches!(
            status.map(|status| &status.state),
            Some(ActivityDto::Speaking { .. })
        );
    StatusView {
        lamp: line.lamp,
        headline: line.headline.to_string(),
        detail: line.detail,
        address: format!("http://127.0.0.1:{port}"),
        start_button: s.connection == Connection::Lost,
        connect_steps: status.is_some_and(|status| !status.ever_connected),
        failure_notice: status.and_then(|status| failure_notice(status.recent_failures)),
        trace: trace_bars(&s.records, speaking, now, timefmt::local_time),
        ticking: line.ticking,
    }
}

/// The lamp, headline and detail, and whether the detail is an "ago" time.
struct StateLine {
    lamp: Lamp,
    headline: &'static str,
    detail: String,
    ticking: bool,
}

fn state_line(connection: Connection, status: Option<&StatusDto>, now: SystemTime) -> StateLine {
    let line = |lamp, headline, detail: String| StateLine {
        lamp,
        headline,
        detail,
        ticking: false,
    };
    // Before the first answer there's no status to describe, and a lost
    // server's last status is out of date.
    let status = match (connection, status) {
        (Connection::Lost, _) => {
            return line(
                Lamp::Stopped,
                "SLC Local TTS has stopped.",
                "SLC can't get speech until it starts again: it plays its failure sound \
                 instead of each announcement, and pressing a voice preview in SLC will \
                 crash it."
                    .to_string(),
            );
        }
        (Connection::Connected, Some(status)) => status,
        (Connection::Connecting | Connection::Connected, _) => {
            return line(
                Lamp::Unknown,
                "Connecting.",
                "Waiting for SLC Local TTS to answer.".to_string(),
            );
        }
    };
    match &status.state {
        ActivityDto::Speaking {
            voice_id, chars, ..
        } => line(
            Lamp::Ready,
            "Speaking.",
            format!("{}, {chars} characters.", voice_name(voice_id)),
        ),
        ActivityDto::Downloading { stem, done, total } => line(
            Lamp::Busy,
            "Downloading a voice.",
            format!(
                "{}: {} of {} MB.",
                row_title(stem),
                format_mb(*done),
                format_mb(*total)
            ),
        ),
        ActivityDto::Idle => match status.last_request_ms {
            None => line(
                Lamp::Ready,
                "Ready for SLC.",
                "Waiting for SLC's first announcement.".to_string(),
            ),
            Some(at_ms) => StateLine {
                ticking: true,
                ..line(
                    Lamp::Ready,
                    "Ready for SLC.",
                    format!(
                        "Last announcement {}.",
                        timefmt::ago(from_unix_ms(at_ms), now)
                    ),
                )
            },
        },
    }
}

/// The amber line under the trace, when announcements failed in the last
/// hour.
fn failure_notice(failures: u32) -> Option<String> {
    let noun = match failures {
        0 => return None,
        1 => "announcement",
        _ => "announcements",
    };
    Some(format!(
        "{failures} {noun} failed in the last hour, and SLC played its failure sound instead."
    ))
}

/// The trace's bars for `records` (oldest first): one per announcement
/// that arrived in the last hour before `now`, oldest first, then a
/// `Speaking` bar while `speaking`. `time_of` formats a record's arrival
/// time (Unix milliseconds) for its label.
pub fn trace_bars(
    records: &[RequestRecord],
    speaking: bool,
    now: SystemTime,
    time_of: impl Fn(u64) -> String,
) -> Vec<TraceBar> {
    let mut bars: Vec<TraceBar> = records
        .iter()
        .filter(|record| {
            // A record from a moment after `now` (the two clocks a little
            // apart) counts as within the hour.
            record.route == Route::Speech && from_unix_ms(record.at_ms) + FAILURE_WINDOW > now
        })
        .map(|record| {
            let time = time_of(record.at_ms);
            if record.failed() {
                TraceBar {
                    seq: Some(record.seq),
                    height: FAILED_BAR,
                    kind: BarKind::Failed,
                    label: format!("{time}, failed"),
                }
            } else {
                // A silent line (text with no letters or digits) has no
                // audio, so it draws the shortest bar.
                let seconds = record.audio_seconds.unwrap_or(0.0);
                let label = if record.outcome == "silent" {
                    format!("{time}, nothing to say")
                } else {
                    format!("{time}, spoke {seconds:.1} s")
                };
                TraceBar {
                    seq: Some(record.seq),
                    height: (seconds / FULL_BAR_SECONDS).clamp(MIN_BAR, 1.0),
                    kind: BarKind::Spoken,
                    label,
                }
            }
        })
        .collect();
    if speaking {
        bars.push(TraceBar {
            seq: None,
            height: 1.0,
            kind: BarKind::Speaking,
            label: "Speaking now".to_string(),
        });
    }
    bars
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{DARK, LIGHT};
    use crate::update::UpdateState;
    use std::time::Duration;

    const NOW_MS: u64 = 1_800_000_000_000;

    fn now() -> SystemTime {
        from_unix_ms(NOW_MS)
    }

    fn status() -> StatusDto {
        StatusDto {
            app: "SLC Local TTS".to_string(),
            version: "0.3.0".to_string(),
            port: 8765,
            state: ActivityDto::Idle,
            last_request_ms: None,
            ever_connected: true,
            recent_failures: 0,
            failed_downloads: 0,
            downloads: Vec::new(),
            latest_seq: 0,
            update: UpdateState::UpToDate,
            can_restart_to_update: true,
            updated_to: None,
            config: Config::default(),
        }
    }

    fn connected(status: StatusDto) -> Snapshot {
        Snapshot {
            connection: Connection::Connected,
            status: Some(status),
            ..Snapshot::default()
        }
    }

    fn record(seq: u64, secs_ago: u64, route: Route, outcome: &str) -> RequestRecord {
        RequestRecord {
            seq,
            at_ms: NOW_MS - secs_ago * 1_000,
            route,
            voice_id: "M1".to_string(),
            text: "Welcome aboard.".to_string(),
            outcome: outcome.to_string(),
            phase: None,
            reason: None,
            message: None,
            audio_seconds: None,
            ms: 400,
        }
    }

    fn spoken(seq: u64, secs_ago: u64, audio: f32) -> RequestRecord {
        RequestRecord {
            audio_seconds: Some(audio),
            ..record(seq, secs_ago, Route::Speech, "spoken")
        }
    }

    fn fixed_time(at_ms: u64) -> String {
        format!("t{}", (NOW_MS - at_ms) / 1_000)
    }

    #[test]
    fn headline_ready_no_lines() {
        let view = status_view(&connected(status()), now());
        assert_eq!(view.lamp, Lamp::Ready);
        assert_eq!(view.headline, "Ready for SLC.");
        assert_eq!(view.detail, "Waiting for SLC's first announcement.");
        assert!(!view.start_button);
        assert!(!view.ticking);
    }

    #[test]
    fn headline_ready_last_two_minutes() {
        let mut s = status();
        s.last_request_ms = Some(NOW_MS - 125_000);
        let view = status_view(&connected(s), now());
        assert_eq!(view.headline, "Ready for SLC.");
        assert_eq!(view.detail, "Last announcement 2 minutes ago.");
        assert!(view.ticking);
    }

    #[test]
    fn headline_speaking_joe() {
        let mut s = status();
        s.last_request_ms = Some(NOW_MS - 1_000);
        s.state = ActivityDto::Speaking {
            voice_id: "en_US-joe-medium".to_string(),
            chars: 87,
            started_ms: NOW_MS - 300,
        };
        let view = status_view(&connected(s), now());
        assert_eq!(view.lamp, Lamp::Ready);
        assert_eq!(view.headline, "Speaking.");
        assert_eq!(view.detail, "Joe (US, medium, Piper), 87 characters.");
        assert!(!view.ticking);
    }

    #[test]
    fn headline_downloading_kokoro() {
        let mut s = status();
        s.state = ActivityDto::Downloading {
            stem: "kokoro-multi-lang-v1_0".to_string(),
            done: 161_000_000,
            total: 349_886_476,
        };
        let view = status_view(&connected(s), now());
        assert_eq!(view.lamp, Lamp::Busy);
        assert_eq!(view.headline, "Downloading a voice.");
        assert_eq!(view.detail, "Kokoro voices: 161 of 350 MB.");
    }

    #[test]
    fn headline_stopped() {
        let mut s = status();
        s.last_request_ms = Some(NOW_MS - 125_000);
        let snapshot = Snapshot {
            connection: Connection::Lost,
            ..connected(s)
        };
        let view = status_view(&snapshot, now());
        assert_eq!(view.lamp, Lamp::Stopped);
        assert_eq!(view.headline, "SLC Local TTS has stopped.");
        assert_eq!(
            view.detail,
            "SLC can't get speech until it starts again: it plays its failure sound instead of \
             each announcement, and pressing a voice preview in SLC will crash it."
        );
        assert!(view.start_button);
        assert!(!view.ticking);
    }

    #[test]
    fn headline_connecting() {
        let view = status_view(&Snapshot::default(), now());
        assert_eq!(view.lamp, Lamp::Unknown);
        assert_eq!(view.headline, "Connecting.");
        assert_eq!(view.detail, "Waiting for SLC Local TTS to answer.");
        assert!(!view.start_button);
        assert!(!view.connect_steps);
        assert!(view.trace.is_empty());
        assert_eq!(view.failure_notice, None);
        assert_eq!(view.address, "http://127.0.0.1:8765");
    }

    #[test]
    fn lamp_mapping() {
        for p in [&DARK, &LIGHT] {
            assert_eq!(Lamp::Ready.color(p), p.ok);
            assert_eq!(Lamp::Busy.color(p), p.act);
            assert_eq!(Lamp::Stopped.color(p), p.warn);
            assert_eq!(Lamp::Unknown.color(p), p.ink_dim);
        }
        let lamp = |snapshot: &Snapshot| status_view(snapshot, now()).lamp;
        assert_eq!(lamp(&Snapshot::default()), Lamp::Unknown);
        assert_eq!(lamp(&connected(status())), Lamp::Ready);
        let mut downloading = status();
        downloading.state = ActivityDto::Downloading {
            stem: "kokoro-multi-lang-v1_0".to_string(),
            done: 1,
            total: 2,
        };
        assert_eq!(lamp(&connected(downloading.clone())), Lamp::Busy);
        let lost = Snapshot {
            connection: Connection::Lost,
            ..connected(downloading)
        };
        assert_eq!(lamp(&lost), Lamp::Stopped);
    }

    #[test]
    fn failure_notice_counts() {
        let notice = |failures: u32| {
            let mut s = status();
            s.recent_failures = failures;
            status_view(&connected(s), now()).failure_notice
        };
        assert_eq!(
            notice(1).as_deref(),
            Some(
                "1 announcement failed in the last hour, and SLC played its failure sound instead."
            )
        );
        assert_eq!(
            notice(2).as_deref(),
            Some(
                "2 announcements failed in the last hour, and SLC played its failure sound instead."
            )
        );
        assert_eq!(notice(0), None);
    }

    #[test]
    fn trace_bars_cover_last_hour_speech_only() {
        let records = vec![
            spoken(1, 3_700, 6.0), // more than an hour ago
            spoken(2, 3_000, 6.0),
            record(3, 2_500, Route::Preview, "spoken"),
            spoken(4, 2_000, 30.0),
            spoken(5, 1_000, 0.1),
            record(6, 500, Route::Speech, "budget_exceeded"),
            record(8, 200, Route::Speech, "silent"),
        ];
        let bars = trace_bars(&records, false, now(), fixed_time);
        assert_eq!(
            bars,
            vec![
                TraceBar {
                    seq: Some(2),
                    height: 0.5,
                    kind: BarKind::Spoken,
                    label: "t3000, spoke 6.0 s".to_string(),
                },
                TraceBar {
                    seq: Some(4),
                    height: 1.0,
                    kind: BarKind::Spoken,
                    label: "t2000, spoke 30.0 s".to_string(),
                },
                TraceBar {
                    seq: Some(5),
                    height: 0.08,
                    kind: BarKind::Spoken,
                    label: "t1000, spoke 0.1 s".to_string(),
                },
                TraceBar {
                    seq: Some(6),
                    height: 0.5,
                    kind: BarKind::Failed,
                    label: "t500, failed".to_string(),
                },
                // Unspeakable text isn't a failure: the shortest bar.
                TraceBar {
                    seq: Some(8),
                    height: 0.08,
                    kind: BarKind::Spoken,
                    label: "t200, nothing to say".to_string(),
                },
            ]
        );

        let bars = trace_bars(&records, true, now(), fixed_time);
        assert_eq!(bars.len(), 6);
        assert_eq!(
            bars.last(),
            Some(&TraceBar {
                seq: None,
                height: 1.0,
                kind: BarKind::Speaking,
                label: "Speaking now".to_string(),
            })
        );

        // A record from just after "now" (the clocks disagree a little)
        // still shows.
        let ahead = RequestRecord {
            at_ms: NOW_MS + 2_000,
            ..spoken(7, 0, 6.0)
        };
        assert_eq!(
            trace_bars(&[ahead], false, now(), |_| String::new()).len(),
            1
        );
    }

    #[test]
    fn status_view_trace_follows_speaking() {
        let mut s = status();
        s.state = ActivityDto::Speaking {
            voice_id: "M1".to_string(),
            chars: 10,
            started_ms: NOW_MS,
        };
        let mut snapshot = connected(s);
        snapshot.records = vec![spoken(1, 60, 6.0)];
        let view = status_view(&snapshot, now());
        let kinds: Vec<BarKind> = view.trace.iter().map(|bar| bar.kind).collect();
        assert_eq!(kinds, vec![BarKind::Spoken, BarKind::Speaking]);

        // Once the server has stopped, nothing is being spoken.
        snapshot.connection = Connection::Lost;
        let view = status_view(&snapshot, now());
        let kinds: Vec<BarKind> = view.trace.iter().map(|bar| bar.kind).collect();
        assert_eq!(kinds, vec![BarKind::Spoken]);
    }

    #[test]
    fn connect_steps_until_ever_connected() {
        let mut s = status();
        s.ever_connected = false;
        assert!(status_view(&connected(s.clone()), now()).connect_steps);
        s.ever_connected = true;
        assert!(!status_view(&connected(s), now()).connect_steps);
        assert_eq!(
            connect_step_2("http://127.0.0.1:8765"),
            "Set Custom API URL to http://127.0.0.1:8765."
        );
    }

    #[test]
    fn address_uses_bound_port() {
        let mut s = status();
        s.port = 9100;
        s.config.port = 9200;
        assert_eq!(
            status_view(&connected(s), now()).address,
            "http://127.0.0.1:9100"
        );
    }

    #[test]
    fn ago_reads_from_the_last_request() {
        let mut s = status();
        s.last_request_ms = Some(NOW_MS - 30_000);
        let view = status_view(&connected(s), now() + Duration::from_secs(60));
        assert_eq!(view.detail, "Last announcement 1 minute ago.");
    }
}
