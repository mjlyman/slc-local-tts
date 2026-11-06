//! The private espeak-ng-data directory and its startup check.
//!
//! `piper-phonemize` unpacks its embedded espeak-ng-data into
//! `std::env::temp_dir()` and reuses any copy it finds there. A foreign copy
//! in the shared `%TEMP%` silently gives wrong phonemes (we measured it), so
//! `server::start` points `TMP`/`TEMP` at a private, versioned folder for the
//! startup check only, and the check proves the data there is sound.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The `piper-phonemize` version this build pins in `Cargo.toml`. It names
/// the private temp folder, so a crate upgrade gets a fresh copy of the data.
pub const PIPER_PHONEMIZE_VERSION: &str = "1.5.9";

/// Text the startup check phonemizes with `en-us`.
pub const CHECK_TEXT: &str = "Fort Lauderdale";

/// The IPA that sound espeak data produces for `CHECK_TEXT`.
pub const CHECK_IPA: &str = "fˈɔːɹt lˈɔːdɚdˌeɪl";

/// Stamp file written into the private dir after a passing check.
const STAMP_FILE: &str = "slc-stamp.txt";

#[derive(Debug, thiserror::Error)]
pub enum EspeakError {
    /// The phonemizer gave the wrong IPA for `CHECK_TEXT`. The dir has been
    /// deleted; the crate initialises only once per process, so the fix
    /// needs a restart.
    #[error("espeak data was damaged and has been reset — please start SLC Local TTS again")]
    Damaged { got: String },
    #[error(
        "SLC Local TTS couldn't prepare its espeak speech data folder ({0}). Close any other copy of SLC Local TTS and start it again. If this keeps happening, delete the folder %LocalAppData%\\SLC-Local-TTS\\espeak and try once more."
    )]
    Io(#[from] io::Error),
}

/// The private temp folder for this crate version: `<espeak_root>\1.5.9`.
pub fn private_temp_dir(espeak_root: &Path) -> PathBuf {
    espeak_root.join(PIPER_PHONEMIZE_VERSION)
}

/// Puts `TMP` and `TEMP` back to their values from before [`redirect_temp`]
/// when dropped.
///
/// The redirect is a guard (like a C++ RAII lock) so it can't outlive the
/// one call that needs it: piper-phonemize reads `TMP` only during its
/// one-time init, and anything that reads the environment later, such as
/// Velopack's updater or another child process, must see the user's real
/// temp folder.
#[must_use = "dropping it puts TMP and TEMP back at once"]
pub struct TempRedirect {
    /// The values before the redirect; `None` means the variable was unset.
    tmp: Option<OsString>,
    temp: Option<OsString>,
}

impl Drop for TempRedirect {
    fn drop(&mut self) {
        // SAFETY: always safe on Windows (see `redirect_temp`); the caller of
        // `redirect_temp` promised the rest when it created this guard.
        unsafe {
            restore_var("TMP", self.tmp.take());
            restore_var("TEMP", self.temp.take());
        }
    }
}

/// Sets `name` to `value`, or removes it when `value` is `None`.
///
/// # Safety
///
/// The same as [`redirect_temp`].
unsafe fn restore_var(name: &str, value: Option<OsString>) {
    // SAFETY: passed on to the caller (see above).
    unsafe {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }
}

/// Creates `dir` and points this process's `TMP` and `TEMP` at it, so
/// `piper-phonemize` unpacks its espeak-ng-data there instead of the shared
/// `%TEMP%`. The returned guard puts both variables back when dropped.
///
/// # Safety
///
/// - `std::env::set_var` and `remove_var` are `unsafe` in Rust 2024 because
///   on some platforms (not Windows) another thread reading the environment
///   at the same moment can see freed memory. On Windows, std documents them
///   as always safe, even with other threads running, because Windows' own
///   environment functions are thread-safe.
/// - The caller holds the guard across the process's first phonemize call
///   (the first [`phonemize`], [`ipa`] or [`verify_startup`]).
///   piper-phonemize 1.5.9 reads `temp_dir()` only in that call's one-time
///   init, and never again, so the guard can be dropped once it returns.
pub unsafe fn redirect_temp(dir: &Path) -> TempRedirect {
    let saved = TempRedirect {
        tmp: std::env::var_os("TMP"),
        temp: std::env::var_os("TEMP"),
    };
    // Best effort: if this fails, piper-phonemize creates the folder itself
    // (it calls create_dir_all), and any real problem shows up in the
    // startup check.
    let _ = fs::create_dir_all(dir);
    // SAFETY: always safe on Windows (see above); the caller promises the
    // ordering.
    unsafe {
        std::env::set_var("TMP", dir);
        std::env::set_var("TEMP", dir);
    }
    saved
}

/// Deletes the folders left by older `piper-phonemize` versions under
/// `espeak_root`, keeping the current version's folder and any plain files.
/// A missing `espeak_root` is fine.
pub fn prune_old_versions(espeak_root: &Path) -> io::Result<()> {
    let entries = match fs::read_dir(espeak_root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.file_name() != PIPER_PHONEMIZE_VERSION {
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

/// The startup check. `dir` is the private temp folder
/// and `phonemize_ipa(text, espeak_voice)` is normally [`ipa`]; tests pass a
/// fake.
///
/// 1. If the stamp is missing or stale, empty `dir` so the crate unpacks a
///    fresh copy of the data.
/// 2. Always phonemize `CHECK_TEXT` with `en-us` and compare to `CHECK_IPA`.
/// 3. On a match, write the stamp. On a mismatch, delete `dir` and return
///    [`EspeakError::Damaged`].
pub fn verify_startup(
    dir: &Path,
    phonemize_ipa: impl Fn(&str, &str) -> Option<String>,
) -> Result<(), EspeakError> {
    let stamp_path = dir.join(STAMP_FILE);
    let expected = stamp_content();
    let stamp_ok = fs::read_to_string(&stamp_path).is_ok_and(|s| s == expected);
    if !stamp_ok {
        remove_dir_if_present(dir)?;
        fs::create_dir_all(dir)?;
    }

    let got = phonemize_ipa(CHECK_TEXT, "en-us").unwrap_or_default();
    if got != CHECK_IPA {
        remove_dir_if_present(dir)?;
        return Err(EspeakError::Damaged { got });
    }

    // The phonemizer may have just unpacked into `dir`; make sure it exists
    // before writing the stamp next to the data.
    fs::create_dir_all(dir)?;
    fs::write(&stamp_path, expected)?;
    Ok(())
}

/// One espeak-ng call at a time, process-wide. espeak-ng keeps global state
/// and isn't reentrant, and piper-phonemize 1.5.9 only guards its one-time
/// init, so two requests phonemizing at once could crash the whole server.
/// Separate from the synthesis lock, so phonemizing never waits for a
/// synthesis to finish.
static PHONEMIZE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// espeak-ng itself failed on a line (the crate gives no reason). Unlike a
/// line with nothing to say, this is a real failure: it must never turn
/// into silence that SLC would cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("espeak-ng couldn't phonemize this line")]
pub struct PhonemizeError;

/// Phonemizes `text` with espeak-ng, one code-point list per sentence.
///
/// - `Ok(Some(..))`: the spoken sentences, with empty ones dropped.
/// - `Ok(None)`: espeak-ng worked, but there is nothing to say.
/// - `Err`: espeak-ng failed.
pub fn phonemize(text: &str, espeak_voice: &str) -> Result<Option<Vec<Vec<i32>>>, PhonemizeError> {
    let result = {
        // Held for the call into espeak-ng only: the result is an owned C++
        // object that doesn't touch espeak-ng's global state, so it can be
        // read after the lock is released.
        //
        // A poisoned lock just means an earlier call panicked; the lock
        // guards no data of its own, so it is safe to keep using.
        let _lock = PHONEMIZE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        piper_phonemize::phonemize(text, espeak_voice)
    };
    let sentences = result.map(|result| {
        (0..result.num_sentences())
            .filter_map(|i| result.get_phonemes(i))
            .collect()
    });
    keep_spoken_sentences(sentences)
}

/// The decision behind [`phonemize`], apart from espeak-ng so tests can
/// drive it: the crate's `None` is a failure, and empty sentences are
/// dropped, leaving `Ok(None)` if none is left.
fn keep_spoken_sentences(
    sentences: Option<Vec<Vec<i32>>>,
) -> Result<Option<Vec<Vec<i32>>>, PhonemizeError> {
    let mut sentences = sentences.ok_or(PhonemizeError)?;
    sentences.retain(|s| !s.is_empty());
    Ok((!sentences.is_empty()).then_some(sentences))
}

/// Phonemizes `text` as IPA, sentences joined with a space; `None` if
/// espeak-ng failed or there is nothing to say. Used by [`verify_startup`].
pub fn ipa(text: &str, espeak_voice: &str) -> Option<String> {
    // Built on `phonemize` (not the crate's `get_phonemes_as_string`) to
    // reuse its empty-sentence filtering.
    let sentences = phonemize(text, espeak_voice).ok().flatten()?;
    let strings: Vec<String> = sentences
        .iter()
        .map(|codepoints| {
            codepoints
                .iter()
                .filter_map(|&cp| u32::try_from(cp).ok().and_then(char::from_u32))
                .collect()
        })
        .collect();
    Some(strings.join(" "))
}

/// The exact stamp text: the crate version, a newline, then `CHECK_IPA`.
fn stamp_content() -> String {
    format!("{PIPER_PHONEMIZE_VERSION}\n{CHECK_IPA}")
}

/// `remove_dir_all` that treats an already-missing dir as success. A file
/// still open elsewhere (Windows) comes back as an `io::Error`, not a panic.
fn remove_dir_if_present(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn expected_stamp() -> String {
        format!("{PIPER_PHONEMIZE_VERSION}\n{CHECK_IPA}")
    }

    fn good_fake(_text: &str, _voice: &str) -> Option<String> {
        Some(CHECK_IPA.to_string())
    }

    #[test]
    fn fresh_dir_writes_stamp() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(PIPER_PHONEMIZE_VERSION);
        fs::create_dir_all(&dir).unwrap();

        verify_startup(&dir, good_fake).unwrap();

        let stamp = fs::read_to_string(dir.join("slc-stamp.txt")).unwrap();
        assert_eq!(stamp, expected_stamp());
    }

    #[test]
    fn stale_stamp_clears_dir_first() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(PIPER_PHONEMIZE_VERSION);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("stale.txt"), "old").unwrap();
        fs::write(dir.join("slc-stamp.txt"), "1.5.8\nx").unwrap();

        verify_startup(&dir, good_fake).unwrap();

        assert!(!dir.join("stale.txt").exists());
        let stamp = fs::read_to_string(dir.join("slc-stamp.txt")).unwrap();
        assert_eq!(stamp, expected_stamp());
    }

    #[test]
    fn wrong_ipa_deletes_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(PIPER_PHONEMIZE_VERSION);
        fs::create_dir_all(&dir).unwrap();

        let result = verify_startup(&dir, |_, _| Some("fɪrt".to_string()));

        match result {
            Err(EspeakError::Damaged { got }) => assert_eq!(got, "fɪrt"),
            other => panic!("expected Damaged, got {other:?}"),
        }
        assert!(!dir.exists());
    }

    #[test]
    fn current_stamp_keeps_dir_and_still_checks() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(PIPER_PHONEMIZE_VERSION);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("slc-stamp.txt"), expected_stamp()).unwrap();
        fs::write(dir.join("keep.txt"), "data").unwrap();
        // Cell lets the `Fn` closure record the call through a shared borrow.
        let called = std::cell::Cell::new(false);

        let result = verify_startup(&dir, |text, voice| {
            called.set(true);
            good_fake(text, voice)
        });

        assert!(result.is_ok());
        assert!(dir.join("keep.txt").exists());
        assert!(called.get(), "the check must run even with a current stamp");
    }

    #[test]
    fn current_stamp_wrong_ipa_is_damaged() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(PIPER_PHONEMIZE_VERSION);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("slc-stamp.txt"), expected_stamp()).unwrap();

        let result = verify_startup(&dir, |_, _| Some("fɪrt".to_string()));

        assert!(matches!(result, Err(EspeakError::Damaged { .. })));
        assert!(!dir.exists());
    }

    #[test]
    fn espeak_failure_is_an_error() {
        assert_eq!(keep_spoken_sentences(None), Err(PhonemizeError));
    }

    #[test]
    fn only_empty_sentences_is_nothing_to_say() {
        assert_eq!(keep_spoken_sentences(Some(vec![])), Ok(None));
        assert_eq!(keep_spoken_sentences(Some(vec![vec![], vec![]])), Ok(None));
    }

    #[test]
    fn spoken_sentences_are_kept_and_empty_ones_dropped() {
        assert_eq!(
            keep_spoken_sentences(Some(vec![vec![1, 2], vec![], vec![3]])),
            Ok(Some(vec![vec![1, 2], vec![3]]))
        );
    }

    /// espeak-ng opens its data files through narrow (`char*`) C APIs, which
    /// decode paths in the process's ANSI code page. `build.rs` embeds a
    /// manifest that makes that code page UTF-8 (65001) in every binary this
    /// package links, this test executable included, so a data folder under
    /// `C:\Users\Jürgen\...` still works.
    #[test]
    fn process_uses_the_utf8_code_page() {
        // SAFETY: GetACP takes no arguments and only reads process state.
        let code_page = unsafe { windows_sys::Win32::Globalization::GetACP() };
        assert_eq!(code_page, windows_sys::Win32::Globalization::CP_UTF8);
    }

    #[test]
    fn prune_missing_root_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        prune_old_versions(&tmp.path().join("espeak")).unwrap();
    }

    #[test]
    fn prune_keeps_current_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("espeak");
        fs::create_dir_all(root.join("1.5.8")).unwrap();
        fs::create_dir_all(root.join("1.5.9")).unwrap();
        fs::write(root.join("notes.txt"), "keep me").unwrap();

        prune_old_versions(&root).unwrap();

        assert!(!root.join("1.5.8").exists());
        assert!(root.join("1.5.9").is_dir());
        assert!(root.join("notes.txt").is_file());
    }
}
