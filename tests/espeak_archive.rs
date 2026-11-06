//! Pin check on build output: `piper-phonemize-sys`'s build script downloads
//! `espeak-ng-data.tar.bz2` without verifying it. This test finds
//! every copy it left under `target/` and checks it against the pinned size
//! and sha256, so a changed upstream archive fails the build loudly.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use walkdir::WalkDir;

const ARCHIVE_NAME: &str = "espeak-ng-data.tar.bz2";
const PINNED_SIZE: u64 = 7_252_012;
const PINNED_SHA256: &str = "4135ccf82e1f40613491c0874d4945ae9e9c7840933d8e25a6f9e003d9ebf533";

/// True for `.../build/piper-phonemize-sys-<hash>/out/espeak-ng-data.tar.bz2`.
fn is_build_archive(path: &Path) -> bool {
    let Some(out_dir) = path.parent() else {
        return false;
    };
    let Some(crate_dir) = out_dir.parent() else {
        return false;
    };
    let Some(build_dir) = crate_dir.parent() else {
        return false;
    };
    path.file_name().is_some_and(|n| n == ARCHIVE_NAME)
        && out_dir.file_name().is_some_and(|n| n == "out")
        && crate_dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("piper-phonemize-sys-"))
        && build_dir.file_name().is_some_and(|n| n == "build")
}

fn sha256_hex(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn every_built_espeak_archive_matches_pin() {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    let archives: Vec<PathBuf> = WalkDir::new(&target)
        .into_iter()
        .filter_map(Result::ok)
        .map(|entry| entry.into_path())
        .filter(|path| is_build_archive(path))
        .collect();

    assert!(
        !archives.is_empty(),
        "no {ARCHIVE_NAME} found under {}/**/build/piper-phonemize-sys-*/out/; \
         the build script should have downloaded it",
        target.display()
    );

    for path in &archives {
        let size = std::fs::metadata(path)
            .unwrap_or_else(|e| panic!("reading metadata of {}: {e}", path.display()))
            .len();
        assert_eq!(
            size,
            PINNED_SIZE,
            "{} has size {size}, expected {PINNED_SIZE}",
            path.display()
        );

        let actual = sha256_hex(path);
        assert_eq!(
            actual,
            PINNED_SHA256,
            "{} has sha256 {actual}, expected {PINNED_SHA256}",
            path.display()
        );
    }
}
