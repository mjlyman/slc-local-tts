//! A repair or reinstall keeps the user's voices and settings.
//!
//! When Setup.exe runs over an existing install, it renames the whole install
//! folder (which is also our data folder) to `<root>.<16 random letters and
//! digits>`, extracts a fresh copy, runs our `--veloapp-install` hook, and
//! then deletes the renamed copy. Without this hook, a repair would delete
//! every downloaded voice and the user's `config.toml`. The hook restores the
//! user's files from the renamed copy into the fresh folder first.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::models::install::rename_with_retry;
use crate::paths::DataDir;

/// Length of the random suffix Setup.exe gives the rollback copy
/// (Velopack's `misc::random_string(16)`: ASCII letters and digits).
const SUFFIX_LEN: usize = 16;

/// The rollback copy Setup.exe left next to `root`, if there is one: a
/// sibling folder named `<root's name>.<16 ASCII letters or digits>`. If
/// there are several (an earlier repair that was interrupted), the most
/// recently modified one wins.
///
/// The name rule takes everything after `<root's name>.` as the suffix. That
/// works only because the root's own name has no dot in it, which always
/// holds: the root is the install folder, `SLC-Local-TTS`.
pub(crate) fn find_rollback(root: &Path) -> Option<PathBuf> {
    let name = root.file_name()?.to_str()?;
    let parent = root.parent()?;
    let prefix = format!("{name}.");

    std::fs::read_dir(parent)
        .ok()?
        // `flatten` skips entries that couldn't be read.
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|n| n.strip_prefix(&prefix))
                .is_some_and(|suffix| {
                    suffix.len() == SUFFIX_LEN && suffix.bytes().all(|b| b.is_ascii_alphanumeric())
                })
        })
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .max_by_key(|entry| {
            // `fs::metadata` opens the folder itself. `entry.metadata()`
            // would be quicker, but on Windows it reuses the times from the
            // parent's folder listing, which NTFS updates late, so a folder
            // that just changed could still look older than it is.
            std::fs::metadata(entry.path())
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH)
        })
        .map(|entry| entry.path())
}

/// Restores the user's files (`DataDir::user_data`) from `from` to `to`,
/// one pair at a time, when the `from` path exists and the `to` path
/// doesn't. It never overwrites or deletes anything, and one that fails is
/// skipped. Returns the `to` paths it restored.
///
/// `config.toml` is copied; everything else (the folders and the two marker
/// files) is moved. Setup.exe can still fail after the hook (registering the
/// uninstaller, starting the app), and then it deletes the new root and puts
/// its rollback copy back. A copied `config.toml` survives that, because the
/// rollback copy still has it. Copying the folders (gigabytes of voices)
/// could outlast the 30 s Setup.exe allows and leave a partial restore, so
/// they are renamed, which is instant. The remaining risk: if Setup.exe
/// fails after the hook, the voices are gone and must be downloaded again.
pub(crate) fn restore(from: &DataDir, to: &DataDir) -> Vec<PathBuf> {
    let config = to.config_file();
    from.user_data()
        .into_iter()
        .zip(to.user_data())
        .filter(|(source, target)| source.exists() && !target.exists())
        .filter_map(|(source, target)| {
            let restored = if target == config {
                // `fs::copy` returns the byte count, which isn't needed.
                std::fs::copy(&source, &target).map(|_| ())
            } else {
                rename_with_retry(&source, &target)
            };
            restored.ok().map(|()| target)
        })
        .collect()
}

/// The install hook's work, for the exe at `exe`. Does nothing unless `exe`
/// sits in a Velopack install: in a folder named `current` whose parent (the
/// install root) holds `Update.exe`. So a dev build, or a copy of the exe
/// somewhere else, never moves anything.
pub(crate) fn keep_user_data_from(exe: &Path) {
    let Some(root) = crate::autostart::velopack_root(exe) else {
        return;
    };
    // Setup.exe deletes the rollback copy right after this hook returns, so
    // this is the only chance to save the user's files. If Setup.exe fails
    // after the hook, it puts the rollback copy back instead: `restore`
    // copies `config.toml` so the settings survive that, but the moved
    // voices are lost and must be downloaded again. Logging isn't
    // running yet (the hook runs before `main` sets it up), so a failed move
    // can't be reported anywhere; it's skipped. The rollback copy and the
    // new root share a parent, so each move, of a folder or a file, is an
    // instant rename, well inside the 30 s Setup.exe gives the hook before
    // killing it.
    if let Some(rollback) = find_rollback(root) {
        restore(&DataDir::at(rollback), &DataDir::at(root.to_path_buf()));
    }
}

/// The install hook: `keep_user_data_from` for this exe. Does nothing if
/// Windows can't tell us where the exe is.
pub(crate) fn keep_user_data() {
    if let Ok(exe) = std::env::current_exe() {
        keep_user_data_from(&exe);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    const ROOT: &str = "José Ü @root";
    const SUFFIX: &str = "Ab3dEf6hIj9kLm2n";

    /// A fresh temp dir and the install root's path inside it (not created).
    fn setup() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(ROOT);
        (tmp, root)
    }

    /// Writes `contents` to `path`, creating its parent folders.
    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn finds_the_rollback_sibling() {
        let (tmp, root) = setup();
        fs::create_dir(&root).unwrap();
        let real = tmp.path().join(format!("{ROOT}.{SUFFIX}"));
        fs::create_dir(&real).unwrap();
        for decoy in [
            format!("{ROOT}.old"),
            format!("{ROOT}.{SUFFIX}X"),
            format!("{ROOT}.Ab3dEf6hIj9kLm-n"),
            format!("other.{SUFFIX}"),
        ] {
            fs::create_dir(tmp.path().join(decoy)).unwrap();
        }
        assert_eq!(find_rollback(&root), Some(real));
    }

    #[test]
    fn no_rollback_is_none() {
        let (_tmp, root) = setup();
        fs::create_dir(&root).unwrap();
        assert_eq!(find_rollback(&root), None);
    }

    /// Makes two rollback siblings of `root`, `older` first, then makes
    /// `newer` the more recently modified one. Returns `newer`'s path.
    fn two_rollbacks(tmp: &Path, older: &str, newer: &str) -> PathBuf {
        let older = tmp.join(format!("{ROOT}.{older}"));
        let newer = tmp.join(format!("{ROOT}.{newer}"));
        fs::create_dir(&older).unwrap();
        fs::create_dir(&newer).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        // A new file updates its folder's modified time.
        fs::write(newer.join("newer"), "").unwrap();
        newer
    }

    #[test]
    fn newest_rollback_wins() {
        // The newer folder's name sorts first in one case and last in the
        // other, so neither a first-by-name nor a last-by-name pick passes
        // both: only picking by time does.
        for (older, newer) in [("zzzzzzzzzzzzzzz9", SUFFIX), (SUFFIX, "zzzzzzzzzzzzzzz9")] {
            let (tmp, root) = setup();
            fs::create_dir(&root).unwrap();
            let newer = two_rollbacks(tmp.path(), older, newer);
            assert_eq!(find_rollback(&root), Some(newer));
        }
    }

    #[test]
    fn a_file_with_the_rollback_name_is_ignored() {
        let (tmp, root) = setup();
        fs::create_dir(&root).unwrap();
        fs::write(tmp.path().join(format!("{ROOT}.{SUFFIX}")), "").unwrap();
        assert_eq!(find_rollback(&root), None);
    }

    #[test]
    fn restore_moves_user_data_and_nothing_else() {
        let (tmp, root) = setup();
        let rollback = tmp.path().join(format!("{ROOT}.{SUFFIX}"));
        write(&rollback.join("config.toml"), "port = 9000");
        write(&rollback.join(r"models\joe\model.onnx"), "model");
        write(&rollback.join(r"logs\a.log"), "log");
        write(&rollback.join(r"espeak\1.5.9\x"), "x");
        write(&rollback.join("slc-connected"), "");
        write(&rollback.join("tray-intro-shown"), "");
        write(&rollback.join("Update.exe"), "updater");
        write(&rollback.join(r"current\old.exe"), "old");
        fs::create_dir(&root).unwrap();

        let from = DataDir::at(rollback.clone());
        let to = DataDir::at(root.clone());
        let moved = restore(&from, &to);

        let read = |path: &str| fs::read_to_string(root.join(path)).unwrap();
        assert_eq!(read("config.toml"), "port = 9000");
        assert_eq!(read(r"models\joe\model.onnx"), "model");
        assert_eq!(read(r"logs\a.log"), "log");
        assert_eq!(read(r"espeak\1.5.9\x"), "x");
        // config.toml is copied, not moved, so a failed Setup can't lose it.
        assert_eq!(
            fs::read_to_string(rollback.join("config.toml")).unwrap(),
            "port = 9000"
        );
        assert!(!rollback.join("models").exists());
        assert!(rollback.join("Update.exe").is_file());
        assert!(rollback.join(r"current\old.exe").is_file());
        assert!(!root.join("Update.exe").exists());
        assert!(!root.join("current").exists());
        assert_eq!(moved, to.user_data().to_vec());
    }

    #[test]
    fn restore_moves_marker_files() {
        let (tmp, root) = setup();
        let rollback = tmp.path().join(format!("{ROOT}.{SUFFIX}"));
        write(&rollback.join("slc-connected"), "");
        write(&rollback.join("tray-intro-shown"), "");
        write(&rollback.join(r"models\x"), "x");
        fs::create_dir(&root).unwrap();

        restore(&DataDir::at(rollback.clone()), &DataDir::at(root.clone()));

        assert!(root.join(r"models\x").is_file());
        for marker in ["slc-connected", "tray-intro-shown"] {
            assert!(root.join(marker).is_file(), "{marker} restored");
            assert!(!rollback.join(marker).exists(), "{marker} is moved");
        }
    }

    #[test]
    fn restore_never_overwrites() {
        let (tmp, root) = setup();
        let rollback = tmp.path().join(format!("{ROOT}.{SUFFIX}"));
        write(&rollback.join("config.toml"), "old");
        write(&root.join("config.toml"), "new");

        let moved = restore(&DataDir::at(rollback.clone()), &DataDir::at(root.clone()));

        assert_eq!(fs::read_to_string(root.join("config.toml")).unwrap(), "new");
        assert_eq!(
            fs::read_to_string(rollback.join("config.toml")).unwrap(),
            "old"
        );
        assert!(moved.is_empty(), "{moved:?}");
    }

    /// An install root holding `Update.exe`, the exe at `exe_dir\`, and a
    /// rollback sibling with a config file and a models folder. Returns the
    /// exe's path and the rollback's.
    fn install_layout(root: &Path, exe_dir: &str) -> (PathBuf, PathBuf) {
        let exe = root.join(exe_dir).join("slc-local-tts.exe");
        write(&exe, "");
        write(&root.join("Update.exe"), "");
        let rollback = root.with_file_name(format!("{ROOT}.{SUFFIX}"));
        write(&rollback.join("config.toml"), "port = 9000");
        write(&rollback.join(r"models\joe\model.onnx"), "model");
        (exe, rollback)
    }

    #[test]
    fn keeps_user_data_inside_an_install() {
        let (_tmp, root) = setup();
        let (exe, rollback) = install_layout(&root, "current");

        keep_user_data_from(&exe);

        assert!(root.join("config.toml").is_file());
        assert!(root.join(r"models\joe\model.onnx").is_file());
        assert!(
            rollback.join("config.toml").is_file(),
            "config.toml is copied"
        );
        assert!(!rollback.join("models").exists(), "models is moved");
    }

    #[test]
    fn outside_an_install_moves_nothing() {
        let (_tmp, root) = setup();
        let (exe, rollback) = install_layout(&root, "bin");

        keep_user_data_from(&exe);

        assert!(!root.join("config.toml").exists());
        assert!(!root.join("models").exists());
        assert!(rollback.join("config.toml").is_file());
        assert!(rollback.join(r"models\joe\model.onnx").is_file());
    }

    #[test]
    fn current_folder_name_is_case_insensitive() {
        let (_tmp, root) = setup();
        let (exe, _rollback) = install_layout(&root, "CURRENT");

        keep_user_data_from(&exe);

        assert!(root.join("config.toml").is_file());
        assert!(root.join(r"models\joe\model.onnx").is_file());
    }

    #[test]
    fn no_update_exe_moves_nothing() {
        let (_tmp, root) = setup();
        let (exe, rollback) = install_layout(&root, "current");
        fs::remove_file(root.join("Update.exe")).unwrap();

        keep_user_data_from(&exe);

        assert!(!root.join("config.toml").exists());
        assert!(!root.join("models").exists());
        assert!(rollback.join("config.toml").is_file());
        assert!(rollback.join(r"models\joe\model.onnx").is_file());
    }
}
