//! The uninstall hook: removes the start-with-Windows entry, so Windows
//! doesn't try to start a program that's gone. The user's files are left
//! to the uninstaller, which deletes the whole install folder.

use std::path::Path;

use crate::autostart::{self, RunEntry};

/// The uninstall hook's work. It never panics and never fails the
/// uninstall. Logging isn't set up while the installer's hooks run, so a
/// failure can't be reported anywhere; it's ignored.
pub(crate) fn before_uninstall() {
    // If Windows can't say where the exe is, the empty path isn't an
    // install, so nothing is touched.
    let exe = std::env::current_exe().unwrap_or_default();
    before_uninstall_from(&exe, &RunEntry::user());
}

/// The uninstall hook's work for the exe at `exe`, on `entry`. Does nothing
/// unless `exe` sits in a Velopack install. Velopack runs this hook whenever
/// the exe starts with its uninstall argument, and tests and dev builds do
/// that too: without the check, they would delete an installed copy's
/// start-with-Windows entry, which lives in the same registry key.
pub(crate) fn before_uninstall_from(exe: &Path, entry: &RunEntry) {
    if autostart::velopack_root(exe).is_some() {
        let _ = autostart::set_enabled(entry, exe, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autostart::scratch::ScratchKey;
    use std::fs;

    /// A scratch entry that is on, pointing at some installed copy.
    fn enabled_scratch(test: &str) -> ScratchKey {
        let scratch = ScratchKey::new(test);
        autostart::set_enabled(
            &scratch.entry,
            Path::new(r"C:\Installed\SLC Local TTS.exe"),
            true,
        )
        .unwrap();
        scratch
    }

    #[test]
    fn dev_build_leaves_the_entry() {
        let scratch = enabled_scratch("uninstall-dev");
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp
            .path()
            .join("target")
            .join("debug")
            .join("slc-local-tts.exe");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "").unwrap();

        before_uninstall_from(&exe, &scratch.entry);
        assert!(autostart::is_enabled(&scratch.entry).unwrap());
    }

    #[test]
    fn installed_copy_removes_the_entry() {
        let scratch = enabled_scratch("uninstall-installed");
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("José Ü @root");
        let exe = root.join("current").join("slc-local-tts.exe");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "").unwrap();
        fs::write(root.join("Update.exe"), "").unwrap();

        before_uninstall_from(&exe, &scratch.entry);
        assert!(!autostart::is_enabled(&scratch.entry).unwrap());
    }

    #[test]
    fn current_without_update_exe_leaves_the_entry() {
        let scratch = enabled_scratch("uninstall-no-update");
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("current").join("slc-local-tts.exe");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "").unwrap();

        before_uninstall_from(&exe, &scratch.entry);
        assert!(autostart::is_enabled(&scratch.entry).unwrap());
    }
}
