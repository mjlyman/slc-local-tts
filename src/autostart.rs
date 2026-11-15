//! Start with Windows: a value in the user's `Run` registry key.

use std::io;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegQueryValueExW,
    RegSetValueExW,
};

/// The user's `Run` key, under `HKEY_CURRENT_USER`.
pub const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// The name of our value in that key.
pub const VALUE_NAME: &str = "SLC Local TTS";
/// The small launcher Velopack puts in the install folder, next to
/// `current\`. It is named after the package's title, and it always starts
/// the newest installed version, so the Run value points at it rather than
/// at the exe inside `current\`.
pub const ROOT_STUB: &str = "SLC Local TTS.exe";

/// Where the start-with-Windows value lives: a key under
/// `HKEY_CURRENT_USER` and a value name in it.
#[derive(Clone, Debug, PartialEq)]
pub struct RunEntry {
    pub subkey: String,
    pub value: String,
}

impl RunEntry {
    /// The real entry: `VALUE_NAME` in the user's `Run` key.
    pub fn user() -> RunEntry {
        RunEntry {
            subkey: RUN_SUBKEY.to_string(),
            value: VALUE_NAME.to_string(),
        }
    }
}

/// The value's data for `exe`: its path in double quotes.
pub fn command_line(exe: &Path) -> String {
    // The quotes keep Windows from splitting a path with spaces in it.
    format!("\"{}\"", exe.display())
}

/// What Windows should start: Velopack's root stub when `current_exe` runs
/// from an install's `current\` folder and the stub is there, otherwise
/// `current_exe` itself.
pub fn target_exe(current_exe: &Path) -> PathBuf {
    let stub = current_exe
        .parent()
        .filter(|_| in_install_folder(current_exe))
        .and_then(Path::parent)
        .map(|root| root.join(ROOT_STUB));
    match stub {
        Some(stub) if stub.is_file() => stub,
        _ => current_exe.to_path_buf(),
    }
}

/// True when `exe` runs from an installed copy: Velopack keeps the
/// installed program in a folder named `current`. A development build (in
/// `target\`) is never one, so it must leave an installed copy's settings
/// alone.
pub fn in_install_folder(exe: &Path) -> bool {
    exe.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("current"))
}

/// The install root when `exe` sits in a Velopack install: in a folder
/// named `current` whose parent holds `Update.exe`. `None` for anything
/// else, such as a dev build or a copy of the exe somewhere else. The
/// installer's hooks check this, because tests and dev builds run the hooks
/// too, and those must never touch an installed copy's files or settings.
pub fn velopack_root(exe: &Path) -> Option<&Path> {
    let root = exe.parent().filter(|_| in_install_folder(exe))?.parent()?;
    root.join("Update.exe").is_file().then_some(root)
}

/// `target_exe` for this process's exe.
pub fn current_target() -> io::Result<PathBuf> {
    Ok(target_exe(&std::env::current_exe()?))
}

/// Whether the value is there.
pub fn is_enabled(entry: &RunEntry) -> io::Result<bool> {
    Ok(stored(entry)?.is_some())
}

/// Adds the value, pointing at `exe`, or removes it.
pub fn set_enabled(entry: &RunEntry, exe: &Path, on: bool) -> io::Result<()> {
    let key = Key::open(&entry.subkey)?;
    let name = wide(&entry.value);
    if on {
        let data = wide(&command_line(exe));
        // The size is in bytes and includes the closing NUL.
        let bytes = u32::try_from(data.len() * 2).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the program's path is too long",
            )
        })?;
        // SAFETY: `name` and `data` are NUL-terminated and outlive the call,
        // and `bytes` is `data`'s exact size, so Windows reads only `data`.
        check(unsafe {
            RegSetValueExW(key.0, name.as_ptr(), 0, REG_SZ, data.as_ptr().cast(), bytes)
        })
    } else {
        // SAFETY: `name` is NUL-terminated and outlives the call.
        match unsafe { RegDeleteValueW(key.0, name.as_ptr()) } {
            // Already off.
            ERROR_FILE_NOT_FOUND => Ok(()),
            code => check(code),
        }
    }
}

/// Points an existing value at `exe` if it points anywhere else. Does
/// nothing when start with Windows is off.
pub fn refresh(entry: &RunEntry, exe: &Path) -> io::Result<()> {
    match stored(entry)? {
        Some(data) if data != command_line(exe) => set_enabled(entry, exe, true),
        _ => Ok(()),
    }
}

/// The value's data, or `None` if there's no value.
/// A value that isn't text (which we never write) reads as an empty
/// string, so `refresh` replaces it.
fn stored(entry: &RunEntry) -> io::Result<Option<String>> {
    let key = Key::open(&entry.subkey)?;
    let name = wide(&entry.value);
    let mut kind: REG_VALUE_TYPE = 0;
    let mut bytes: u32 = 0;
    // First ask only for the size, in bytes.
    // SAFETY: `name` is NUL-terminated and outlives the call; a null data
    // pointer asks Windows for the size alone, written to `bytes`.
    let code = unsafe {
        RegQueryValueExW(
            key.0,
            name.as_ptr(),
            std::ptr::null(),
            &mut kind,
            std::ptr::null_mut(),
            &mut bytes,
        )
    };
    if code == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    check(code)?;
    if kind != REG_SZ {
        return Ok(Some(String::new()));
    }
    // Room for the data plus a NUL, in case the value was stored without one.
    let mut data = vec![0u16; bytes as usize / 2 + 1];
    let mut size = u32::try_from(data.len() * 2).unwrap_or(u32::MAX);
    // SAFETY: as above, and `data` has room for `size` bytes, which Windows
    // never writes past; it updates `size` to the bytes it wrote.
    check(unsafe {
        RegQueryValueExW(
            key.0,
            name.as_ptr(),
            std::ptr::null(),
            &mut kind,
            data.as_mut_ptr().cast(),
            &mut size,
        )
    })?;
    data.truncate(size as usize / 2);
    // Drop the closing NUL (and any extra ones).
    while data.last() == Some(&0) {
        data.pop();
    }
    Ok(Some(String::from_utf16_lossy(&data)))
}

/// An open registry key under `HKEY_CURRENT_USER`, closed when dropped.
struct Key(HKEY);

impl Key {
    /// Opens `subkey` for reading and writing values, creating it if it
    /// isn't there (the real `Run` key always is).
    fn open(subkey: &str) -> io::Result<Key> {
        let subkey = wide(subkey);
        let mut handle: HKEY = std::ptr::null_mut();
        // SAFETY: `subkey` is NUL-terminated and outlives the call; the null
        // pointers are optional inputs and outputs; `handle` receives the
        // key, which `Key`'s `drop` closes.
        check(unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_QUERY_VALUE | KEY_SET_VALUE,
                std::ptr::null(),
                &mut handle,
                std::ptr::null_mut(),
            )
        })?;
        Ok(Key(handle))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle came from `RegCreateKeyExW` and is closed once.
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

/// A registry call's result code as an `io::Result`.
fn check(code: WIN32_ERROR) -> io::Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        // Registry calls return the error code instead of setting the
        // thread's last error.
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

/// `s` as the NUL-terminated UTF-16 string Windows wants.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// Test support for code that reads or writes a start-with-Windows entry,
/// so no test ever touches the real `Run` key.
#[cfg(test)]
pub(crate) mod scratch {
    use super::{RunEntry, VALUE_NAME, wide};

    /// A scratch key, `HKCU\Software\SLC-Local-TTS-tests\<unique>`, deleted
    /// when the guard is dropped, even if the test panics.
    pub struct ScratchKey {
        pub entry: RunEntry,
    }

    impl ScratchKey {
        pub fn new(test: &str) -> ScratchKey {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let unique = format!("{test}-{}-{nanos}", std::process::id());
            ScratchKey {
                entry: RunEntry {
                    subkey: format!(r"{TEST_PARENT}\{unique}"),
                    value: VALUE_NAME.to_string(),
                },
            }
        }
    }

    const TEST_PARENT: &str = r"Software\SLC-Local-TTS-tests";

    impl Drop for ScratchKey {
        fn drop(&mut self) {
            use windows_sys::Win32::System::Registry::{
                HKEY_CURRENT_USER, RegDeleteKeyW, RegDeleteTreeW,
            };
            let key = wide(&self.entry.subkey);
            let parent = wide(TEST_PARENT);
            // SAFETY: both strings are NUL-terminated and outlive the calls.
            // Deleting the parent fails, harmlessly, while another test's
            // scratch key is still in it.
            unsafe {
                RegDeleteTreeW(HKEY_CURRENT_USER, key.as_ptr());
                RegDeleteKeyW(HKEY_CURRENT_USER, key.as_ptr());
                RegDeleteKeyW(HKEY_CURRENT_USER, parent.as_ptr());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::scratch::ScratchKey;
    use super::*;
    use std::fs;

    const EXE: &str = r"C:\Users\José Ü\AppData\Local\SLC-Local-TTS\slc-local-tts.exe";

    #[test]
    fn command_line_quotes_spaces_and_unicode() {
        assert_eq!(command_line(Path::new(EXE)), format!("\"{EXE}\""));
    }

    #[test]
    fn target_exe_prefers_root_stub_in_current() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("José Ü @root");
        let exe = root.join("current").join("slc-local-tts.exe");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "").unwrap();

        // Without the stub: the exe itself.
        assert_eq!(target_exe(&exe), exe);

        // With the stub: the stub.
        fs::write(root.join(ROOT_STUB), "").unwrap();
        assert_eq!(target_exe(&exe), root.join(ROOT_STUB));

        // Not under `current`: the exe itself, even with a stub above it.
        let other = root.join("bin").join("slc-local-tts.exe");
        fs::create_dir_all(other.parent().unwrap()).unwrap();
        fs::write(&other, "").unwrap();
        assert_eq!(target_exe(&other), other);
    }

    #[test]
    fn set_and_clear_in_scratch_key() {
        let scratch = ScratchKey::new("set-and-clear");
        let entry = &scratch.entry;
        let exe = Path::new(EXE);
        assert!(!is_enabled(entry).unwrap());

        set_enabled(entry, exe, true).unwrap();
        assert!(is_enabled(entry).unwrap());
        assert_eq!(stored(entry).unwrap().as_deref(), Some(&*command_line(exe)));

        set_enabled(entry, exe, false).unwrap();
        assert!(!is_enabled(entry).unwrap());
        // Turning it off again is fine.
        set_enabled(entry, exe, false).unwrap();
    }

    #[test]
    fn refresh_rewrites_moved_target() {
        let scratch = ScratchKey::new("refresh");
        let entry = &scratch.entry;
        let old = Path::new(r"C:\Old place\slc-local-tts.exe");
        let new = Path::new(EXE);

        // Off: refresh doesn't turn it on.
        refresh(entry, new).unwrap();
        assert!(!is_enabled(entry).unwrap());

        set_enabled(entry, old, true).unwrap();
        refresh(entry, new).unwrap();
        assert_eq!(stored(entry).unwrap().as_deref(), Some(&*command_line(new)));
    }
}
