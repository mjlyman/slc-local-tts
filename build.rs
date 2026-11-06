//! Cargo runs this build script before compiling the package. It embeds the
//! Windows application manifest `windows/slc-local-tts.manifest`, which
//! switches the process's ANSI code page to UTF-8, so espeak-ng's and
//! sherpa-onnx's narrow (`char*`) file APIs can open paths with non-ASCII
//! letters. The manifest file explains the details.

use std::path::Path;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR");
    let manifest = Path::new(&manifest_dir)
        .join("windows")
        .join("slc-local-tts.manifest");

    // `rustc-link-arg` (unlike `rustc-link-arg-bins`) applies to every binary
    // this package links, the test executables included, so the tests run
    // with the same code page as the real exe. rustc quotes each argument
    // for link.exe, so the spaces and `@` in the repo path are safe.
    //
    // /MANIFEST:EMBED puts the manifest inside the binary as a resource,
    // instead of a separate `.exe.manifest` file next to it; /MANIFESTINPUT
    // merges our file into the one link.exe generates (its UAC section).
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-changed=build.rs");
}
