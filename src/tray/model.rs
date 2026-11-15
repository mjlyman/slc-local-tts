//! What the tray shows, worked out from the server's status: the badge on
//! the icon, the tooltip, the menu's state line and its update item. Also
//! the quit warning and the rule for the icon's GUID. Everything here is
//! plain logic, so the tests can check every text without a real tray icon.

use std::path::Path;
use std::time::SystemTime;

use crate::api::slc::dto::StatusDto;
use crate::autostart;
use crate::names;
use crate::timefmt;
use crate::update::UpdateState;

/// Windows shows at most 127 UTF-16 units of a tray icon's tooltip.
pub const TOOLTIP_MAX: usize = 127;

/// The tray icon's fixed identity for an installed copy, so Windows
/// remembers the user's "show in taskbar" choice across updates.
pub const TRAY_GUID: u128 = 0x0faa9ece_4fe1_407b_9c2a_42a8049675e9;

/// The small picture over the icon's corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    None,
    /// A cyan down-arrow: a voice is downloading.
    Downloading,
    /// An amber dot: an announcement failed in the last hour, or a download
    /// failed.
    Attention,
}

/// Everything the tray shows that depends on the status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayView {
    pub badge: Badge,
    pub tooltip: String,
    /// The menu's first, disabled line.
    pub state_line: String,
    /// The menu item that restarts to install a downloaded update, while
    /// one is ready and this program can restart itself.
    pub update_item: Option<String>,
}

/// What the tray shows for status `s`. Something that needs attention
/// beats a download, and a download beats "ready".
pub fn tray_view(s: &StatusDto) -> TrayView {
    let (badge, tip, state_line) = if s.recent_failures > 0 {
        let n = s.recent_failures;
        let announcements = if n == 1 {
            "announcement"
        } else {
            "announcements"
        };
        (
            Badge::Attention,
            format!("{n} {announcements} failed. Click for details."),
            format!("{n} {announcements} failed in the last hour"),
        )
    } else if s.failed_downloads > 0 {
        (
            Badge::Attention,
            "a voice download failed. Click for details.".to_string(),
            "A voice download failed".to_string(),
        )
    } else if let Some(download) = s.downloads.first() {
        let title = names::row_title(&download.stem);
        let percent = percent(download.done, download.total);
        (
            Badge::Downloading,
            format!("downloading {title}, {percent}%"),
            format!("Downloading {title}, {percent}%"),
        )
    } else {
        (
            Badge::None,
            format!("ready on port {}", s.port),
            format!("Ready for SLC on port {}", s.port),
        )
    };
    TrayView {
        badge,
        tooltip: truncate_utf16(&format!("SLC Local TTS: {tip}"), TOOLTIP_MAX),
        state_line,
        update_item: ready_update(s).map(|version| format!("Restart to update to {version}…")),
    }
}

/// The version of the downloaded update that "Restart to update" would
/// install now: `None` unless one is ready and this program can restart
/// itself.
pub fn ready_update(s: &StatusDto) -> Option<&str> {
    match &s.update {
        UpdateState::Ready { version } if s.can_restart_to_update => Some(version),
        _ => None,
    }
}

/// `done` out of `total` as a whole percentage, rounded down so it never
/// shows 100% before the end: 0 when the size isn't known, at most 100.
fn percent(done: u64, total: u64) -> u64 {
    if total == 0 {
        return 0;
    }
    // In u128, `done * 100` can't overflow.
    let percent = u128::from(done) * 100 / u128::from(total);
    percent.min(100) as u64
}

/// `s` cut to at most `max_units` UTF-16 units, ending with "…" when it was
/// cut. It cuts between characters, so it never splits a surrogate pair
/// (the two units of an emoji, for example).
pub fn truncate_utf16(s: &str, max_units: usize) -> String {
    if s.encode_utf16().count() <= max_units {
        return s.to_string();
    }
    // Room for the text once "…" (one unit) is added.
    let Some(room) = max_units.checked_sub(1) else {
        return String::new();
    };
    let mut cut = String::new();
    let mut used = 0;
    // Whole characters only: one that needs two units is either kept
    // whole or left out.
    for c in s.chars() {
        if used + c.len_utf16() > room {
            break;
        }
        used += c.len_utf16();
        cut.push(c);
    }
    cut.push('…');
    cut
}

/// The question Quit asks first. Its last sentence says when SLC last asked
/// for speech, so the user can tell whether SLC is still flying.
pub fn quit_prompt(last_request: Option<SystemTime>, now: SystemTime) -> String {
    let last = match last_request {
        Some(at) => format!("SLC last asked for speech {}.", timefmt::ago(at, now)),
        None => "SLC hasn't asked for speech since SLC Local TTS started.".to_string(),
    };
    format!(
        "Quit SLC Local TTS? SLC needs it for every announcement. If SLC is still open, it \
         will play its failure sound instead of each announcement, and pressing a voice \
         preview will crash it. {last}"
    )
}

/// The GUID for the tray icon of the program at `exe`: `Some` only for an
/// installed copy. Windows ties a GUID to the program's path, so a
/// development build using the same GUID would lose its icon.
pub fn tray_guid(exe: &Path) -> Option<u128> {
    autostart::in_install_folder(exe).then_some(TRAY_GUID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::slc::dto::{ActivityDto, DownloadDto};
    use crate::config::Config;
    use std::path::PathBuf;
    use std::time::Duration;

    const KOKORO: &str = "kokoro-multi-lang-v1_0";

    fn ready() -> StatusDto {
        StatusDto {
            app: "SLC Local TTS".to_string(),
            version: "0.2.0".to_string(),
            port: 8765,
            state: ActivityDto::Idle,
            last_request_ms: None,
            ever_connected: false,
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

    fn downloading(stem: &str, done: u64, total: u64) -> StatusDto {
        StatusDto {
            downloads: vec![DownloadDto {
                stem: stem.to_string(),
                done,
                total,
            }],
            ..ready()
        }
    }

    fn failures(n: u32) -> StatusDto {
        StatusDto {
            recent_failures: n,
            ..ready()
        }
    }

    fn failed_download() -> StatusDto {
        StatusDto {
            failed_downloads: 1,
            ..ready()
        }
    }

    #[test]
    fn update_item_only_when_ready_and_restartable() {
        let ready_update = StatusDto {
            update: UpdateState::Ready {
                version: "1.4.0".to_string(),
            },
            ..ready()
        };
        assert_eq!(
            tray_view(&ready_update).update_item.as_deref(),
            Some("Restart to update to 1.4.0…")
        );
        // The console program can't restart itself.
        let under_cli = StatusDto {
            can_restart_to_update: false,
            ..ready_update.clone()
        };
        assert_eq!(tray_view(&under_cli).update_item, None);
        for update in [
            UpdateState::NotInstalled,
            UpdateState::Off,
            UpdateState::UpToDate,
            UpdateState::Downloading,
            UpdateState::Failed {
                reason: "offline".to_string(),
            },
        ] {
            let status = StatusDto { update, ..ready() };
            assert_eq!(tray_view(&status).update_item, None, "{:?}", status.update);
        }
    }

    #[test]
    fn tooltip_texts() {
        assert_eq!(
            tray_view(&ready()).tooltip,
            "SLC Local TTS: ready on port 8765"
        );
        assert_eq!(
            tray_view(&downloading(KOKORO, 161_000_000, 349_886_476)).tooltip,
            "SLC Local TTS: downloading Kokoro voices, 46%"
        );
        assert_eq!(
            tray_view(&failures(1)).tooltip,
            "SLC Local TTS: 1 announcement failed. Click for details."
        );
        assert_eq!(
            tray_view(&failures(3)).tooltip,
            "SLC Local TTS: 3 announcements failed. Click for details."
        );
        assert_eq!(
            tray_view(&failed_download()).tooltip,
            "SLC Local TTS: a voice download failed. Click for details."
        );
    }

    #[test]
    fn state_line_texts() {
        assert_eq!(tray_view(&ready()).state_line, "Ready for SLC on port 8765");
        assert_eq!(
            tray_view(&downloading(KOKORO, 161_000_000, 349_886_476)).state_line,
            "Downloading Kokoro voices, 46%"
        );
        assert_eq!(
            tray_view(&failures(1)).state_line,
            "1 announcement failed in the last hour"
        );
        assert_eq!(
            tray_view(&failures(2)).state_line,
            "2 announcements failed in the last hour"
        );
        assert_eq!(
            tray_view(&failed_download()).state_line,
            "A voice download failed"
        );
    }

    #[test]
    fn badges_follow_the_status() {
        assert_eq!(tray_view(&ready()).badge, Badge::None);
        assert_eq!(
            tray_view(&downloading(KOKORO, 1, 2)).badge,
            Badge::Downloading
        );
        assert_eq!(tray_view(&failures(1)).badge, Badge::Attention);
        assert_eq!(tray_view(&failed_download()).badge, Badge::Attention);
    }

    #[test]
    fn attention_beats_downloading() {
        let both = StatusDto {
            recent_failures: 1,
            ..downloading(KOKORO, 1, 2)
        };
        let view = tray_view(&both);
        assert_eq!(view.badge, Badge::Attention);
        assert_eq!(
            view.tooltip,
            "SLC Local TTS: 1 announcement failed. Click for details."
        );

        let failed_and_downloading = StatusDto {
            failed_downloads: 1,
            ..downloading(KOKORO, 1, 2)
        };
        assert_eq!(tray_view(&failed_and_downloading).badge, Badge::Attention);
        assert_eq!(
            tray_view(&failed_and_downloading).state_line,
            "A voice download failed"
        );
    }

    #[test]
    fn download_percent_is_safe_at_the_edges() {
        // An unknown size reads 0%, and the count never passes 100%.
        assert!(
            tray_view(&downloading(KOKORO, 5, 0))
                .tooltip
                .ends_with(", 0%")
        );
        assert!(
            tray_view(&downloading(KOKORO, 9, 4))
                .tooltip
                .ends_with(", 100%")
        );
        assert!(
            tray_view(&downloading(KOKORO, u64::MAX, u64::MAX))
                .tooltip
                .ends_with(", 100%")
        );
    }

    #[test]
    fn tooltip_is_at_most_127_utf16_units() {
        // An emoji is two UTF-16 units, so a cut in the wrong place would
        // leave half of one.
        let stem: String = "voice🛫".repeat(50);
        assert_eq!(
            names::row_title(&stem),
            stem,
            "the catalog must not know it"
        );
        let tooltip = tray_view(&downloading(&stem, 1, 2)).tooltip;
        assert!(tooltip.encode_utf16().count() <= TOOLTIP_MAX, "{tooltip}");
        assert!(tooltip.ends_with('…'), "{tooltip}");
        // A `String` can't hold half a pair; this checks the units too.
        let units: Vec<u16> = tooltip.encode_utf16().collect();
        assert!(String::from_utf16(&units).is_ok());
    }

    #[test]
    fn truncate_utf16_cuts_only_when_needed() {
        assert_eq!(truncate_utf16("short", 127), "short");
        assert_eq!(truncate_utf16("abcdef", 6), "abcdef");
        assert_eq!(truncate_utf16("abcdefg", 6), "abcde…");
        // The pair would end at unit 4 with room for 3: it is left out whole.
        assert_eq!(truncate_utf16("ab🛫cd", 4), "ab…");
        assert_eq!(truncate_utf16("abc", 0), "");
    }

    #[test]
    fn quit_prompt_texts() {
        const START: &str = "Quit SLC Local TTS? SLC needs it for every announcement.";
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

        let recent = quit_prompt(Some(now - Duration::from_secs(125)), now);
        assert!(recent.starts_with(START), "{recent}");
        assert!(
            recent.ends_with("SLC last asked for speech 2 minutes ago."),
            "{recent}"
        );
        assert_eq!(
            recent,
            "Quit SLC Local TTS? SLC needs it for every announcement. If SLC is still open, \
             it will play its failure sound instead of each announcement, and pressing a voice \
             preview will crash it. SLC last asked for speech 2 minutes ago."
        );

        let never = quit_prompt(None, now);
        assert!(never.starts_with(START), "{never}");
        assert!(
            never.ends_with("SLC hasn't asked for speech since SLC Local TTS started."),
            "{never}"
        );
    }

    #[test]
    fn tray_guid_only_when_installed() {
        let installed =
            PathBuf::from(r"C:\Users\Jürgen\AppData\Local\SLC Local TTS\current\slc-local-tts.exe");
        assert_eq!(tray_guid(&installed), Some(TRAY_GUID));
        let upper = PathBuf::from(r"C:\Apps\SLC\Current\slc-local-tts.exe");
        assert_eq!(tray_guid(&upper), Some(TRAY_GUID));

        let dev = PathBuf::from(r"C:\src\SLC_Local_TTS\target\release\slc-local-tts.exe");
        assert_eq!(tray_guid(&dev), None);
        let nested = PathBuf::from(r"C:\current\bin\slc-local-tts.exe");
        assert_eq!(tray_guid(&nested), None);
        assert_eq!(tray_guid(Path::new("")), None);
    }
}
