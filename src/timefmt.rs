//! Times in words, such as "2 minutes ago", and times of day in the
//! user's own format.

use std::time::SystemTime;

use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows_sys::Win32::Globalization::{GetTimeFormatEx, TIME_NOSECONDS};
use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

/// The Unix epoch (1970) as a Windows file time: 100-nanosecond intervals
/// since 1601.
const UNIX_EPOCH_AS_FILETIME: u64 = 116_444_736_000_000_000;

/// How long before `now` the moment `then` was, in words: "just now" under
/// a minute, then whole minutes, hours or days ("1 minute ago",
/// "3 hours ago"). A `then` after `now` (the clock moved back) is "just now".
pub fn ago(then: SystemTime, now: SystemTime) -> String {
    // `duration_since` fails when `then` is later than `now`.
    let secs = now.duration_since(then).map_or(0, |gone| gone.as_secs());
    match secs {
        0..60 => "just now".to_string(),
        60..3_600 => count(secs / 60, "minute"),
        3_600..86_400 => count(secs / 3_600, "hour"),
        _ => count(secs / 86_400, "day"),
    }
}

/// The time of day at `unix_ms` (milliseconds since the Unix epoch) in the
/// user's time zone and Windows' own time format, without seconds: "14:05"
/// or "2:05 PM".
pub fn local_time(unix_ms: u64) -> String {
    windows_local_time(unix_ms).unwrap_or_else(|| utc_time(unix_ms))
}

/// [`local_time`] through Windows, or `None` if any step fails.
fn windows_local_time(unix_ms: u64) -> Option<String> {
    let ticks = unix_ms
        .saturating_mul(10_000)
        .saturating_add(UNIX_EPOCH_AS_FILETIME);
    // A FILETIME is one 64-bit number split into two 32-bit halves.
    let file_time = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    // SAFETY (all three calls): every pointer is to a live local of the
    // type Windows expects, and the buffer's length is passed with it.
    // SYSTEMTIME is plain numbers, so all zeros is a valid value to start
    // from.
    let mut utc: SYSTEMTIME = unsafe { std::mem::zeroed() };
    if unsafe { FileTimeToSystemTime(&file_time, &mut utc) } == 0 {
        return None;
    }
    let mut local: SYSTEMTIME = unsafe { std::mem::zeroed() };
    // A null time zone means the user's current one.
    if unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) } == 0 {
        return None;
    }
    let mut buffer = [0u16; 64];
    // A null locale name is LOCALE_NAME_USER_DEFAULT, and a null format
    // means the locale's own. The result counts the closing NUL.
    let written = unsafe {
        GetTimeFormatEx(
            std::ptr::null(),
            TIME_NOSECONDS,
            &local,
            std::ptr::null(),
            buffer.as_mut_ptr(),
            buffer.len() as i32,
        )
    };
    if written <= 1 {
        return None;
    }
    Some(String::from_utf16_lossy(&buffer[..written as usize - 1]))
}

/// The time of day at `unix_ms` in UTC, "14:05 UTC": a fallback that
/// never fails.
fn utc_time(unix_ms: u64) -> String {
    let minutes_of_day = (unix_ms / 60_000) % (24 * 60);
    format!("{:02}:{:02} UTC", minutes_of_day / 60, minutes_of_day % 60)
}

/// "1 minute ago", "2 minutes ago".
fn count(n: u64, unit: &str) -> String {
    let plural = if n == 1 { "" } else { "s" };
    format!("{n} {unit}{plural} ago")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ago_secs(secs: u64) -> String {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        ago(now - Duration::from_secs(secs), now)
    }

    #[test]
    fn ago_formats() {
        assert_eq!(ago_secs(0), "just now");
        assert_eq!(ago_secs(59), "just now");
        assert_eq!(ago_secs(60), "1 minute ago");
        assert_eq!(ago_secs(119), "1 minute ago");
        assert_eq!(ago_secs(120), "2 minutes ago");
        assert_eq!(ago_secs(3_599), "59 minutes ago");
        assert_eq!(ago_secs(3_600), "1 hour ago");
        assert_eq!(ago_secs(7_200), "2 hours ago");
        assert_eq!(ago_secs(86_399), "23 hours ago");
        assert_eq!(ago_secs(86_400), "1 day ago");
        assert_eq!(ago_secs(3 * 86_400), "3 days ago");
    }

    #[test]
    fn local_time_is_nonempty_and_has_digits() {
        for unix_ms in [0, 1_800_000_000_000, 1_800_000_000_000 + 13 * 3_600_000] {
            let text = local_time(unix_ms);
            assert!(!text.is_empty(), "{unix_ms}");
            assert!(text.chars().any(|c| c.is_ascii_digit()), "{text}");
            // Through Windows, not the UTC fallback, which has digits too.
            assert!(windows_local_time(unix_ms).is_some(), "{unix_ms}");
        }
    }

    #[test]
    fn utc_fallback_formats() {
        assert_eq!(utc_time(0), "00:00 UTC");
        assert_eq!(utc_time((14 * 60 + 5) * 60_000 + 59_999), "14:05 UTC");
    }

    #[test]
    fn a_time_in_the_future_is_just_now() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        assert_eq!(ago(now + Duration::from_secs(90), now), "just now");
    }
}
