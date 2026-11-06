//! Runs the real `piper-phonemize` crate against a private temp dir. This is
//! its own test binary, so its own process: redirecting `TMP`/`TEMP` here
//! can't leak into other tests.

use slc_local_tts::frontend::espeak::{
    CHECK_IPA, CHECK_TEXT, PIPER_PHONEMIZE_VERSION, ipa, phonemize, redirect_temp,
};

#[test]
fn real_phonemizer_uses_private_dir() {
    // A local, so the TempDir (and the espeak-ng-data unpacked into it) is
    // deleted when the test ends.
    let tmp = tempfile::tempdir().unwrap();
    // SAFETY: this test binary contains only this one test, so no other
    // thread reads the environment while we change it. `_temp` is held
    // until the first phonemize call below has returned.
    let _temp = unsafe { redirect_temp(tmp.path()) };

    assert_eq!(ipa(CHECK_TEXT, "en-us").as_deref(), Some(CHECK_IPA));
    assert!(
        tmp.path()
            .join("piper-phonemize-espeak-ng-data")
            .join("espeak-ng-data")
            .join("phontab")
            .exists()
    );
    assert_eq!(piper_phonemize::get_version(), PIPER_PHONEMIZE_VERSION);
    // Punctuation only: espeak-ng works, but there is nothing to say.
    assert_eq!(phonemize("...", "en-us"), Ok(None));
}
