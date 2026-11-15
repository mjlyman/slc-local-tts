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

const CRATE: &str = "piper-phonemize-sys";

/// True for the archive in `piper-phonemize-sys`'s build output, in either
/// of cargo's layouts: `.../build/piper-phonemize-sys-<hash>/out/` (up to
/// Rust 1.99) or `.../build/piper-phonemize-sys/<hash>/out/` (Rust 1.100).
fn is_build_archive(path: &Path) -> bool {
    if path.file_name().is_none_or(|name| name != ARCHIVE_NAME) {
        return false;
    }
    // The path's folder names, in order, so neighbours can be compared.
    let names: Vec<&str> = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect();
    let crate_with_hash = format!("{CRATE}-");
    names.windows(2).any(|pair| {
        pair[0] == "build" && (pair[1] == CRATE || pair[1].starts_with(&crate_with_hash))
    })
}

fn sha256_hex(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn finds_the_archive_in_either_build_layout() {
    // Cargo up to Rust 1.99 puts a build script's output in
    // `build/<crate>-<hash>/out/`; Rust 1.100 puts it in
    // `build/<crate>/<hash>/out/`.
    let old =
        Path::new("target/debug/build/piper-phonemize-sys-0123abcd/out/espeak-ng-data.tar.bz2");
    let new =
        Path::new("target/debug/build/piper-phonemize-sys/0123abcd/out/espeak-ng-data.tar.bz2");
    assert!(is_build_archive(old));
    assert!(is_build_archive(new));

    let other_crate =
        Path::new("target/debug/build/espeak-sys/0123abcd/out/espeak-ng-data.tar.bz2");
    let not_in_build = Path::new("target/piper-phonemize-sys/espeak-ng-data.tar.bz2");
    let other_file = Path::new("target/debug/build/piper-phonemize-sys/0123abcd/out/other.tar.bz2");
    assert!(!is_build_archive(other_crate));
    assert!(!is_build_archive(not_in_build));
    assert!(!is_build_archive(other_file));
}

#[test]
fn every_built_espeak_archive_matches_pin() {
    // `CARGO_TARGET_TMPDIR` is `<target folder>/tmp`, so its parent is the
    // folder this build writes to, even when `CARGO_TARGET_DIR` moves it.
    let target = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .expect("CARGO_TARGET_TMPDIR is inside the target folder");
    let archives: Vec<PathBuf> = WalkDir::new(target)
        .into_iter()
        .filter_map(Result::ok)
        .map(|entry| entry.into_path())
        .filter(|path| is_build_archive(path))
        .collect();

    assert!(
        !archives.is_empty(),
        "no {ARCHIVE_NAME} found in {CRATE}'s build output under {}; \
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
