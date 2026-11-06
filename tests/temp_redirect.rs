//! `redirect_temp`'s guard puts `TMP` and `TEMP` back exactly as they were.
//! This is the only test in its binary, so its own process: it changes the
//! process environment, which no other test may see.

use std::ffi::OsString;

use slc_local_tts::frontend::espeak::redirect_temp;

/// Sets `name` to `value`, or removes it when `value` is `None`.
///
/// # Safety
///
/// Windows' environment functions are thread-safe, and std documents
/// `set_var` and `remove_var` as always safe on Windows.
unsafe fn restore(name: &str, value: Option<OsString>) {
    // SAFETY: see the function's own Safety section.
    unsafe {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }
}

#[test]
fn redirect_restores_exact_values() {
    let saved_tmp = std::env::var_os("TMP");
    let saved_temp = std::env::var_os("TEMP");

    let tmp = tempfile::tempdir().unwrap();
    let jose = tmp.path().join("José Ü @tmp");
    let private = tmp.path().join("private");
    // SAFETY: always safe on Windows (see `restore`), and this binary has
    // no other test that could read the environment meanwhile.
    unsafe {
        std::env::set_var("TMP", &jose);
        std::env::remove_var("TEMP");
    }

    // SAFETY: as above; nothing in this process phonemizes.
    let guard = unsafe { redirect_temp(&private) };
    assert_eq!(std::env::var_os("TMP"), Some(private.clone().into()));
    assert_eq!(std::env::var_os("TEMP"), Some(private.into()));

    drop(guard);
    assert_eq!(std::env::var_os("TMP"), Some(jose.into()));
    assert_eq!(std::env::var_os("TEMP"), None);

    // SAFETY: as above.
    unsafe {
        restore("TMP", saved_tmp);
        restore("TEMP", saved_temp);
    }
}
