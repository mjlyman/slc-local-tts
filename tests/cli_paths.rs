//! Runs the real `slc-local-tts-cli.exe` (whose own path, inside this repo,
//! has spaces and `@`) against a data folder with spaces, `@` and
//! non-ASCII letters, to check that every path survives intact.

use std::process::{Command, Stdio};

#[test]
fn cli_runs_from_unicode_data_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("José Ü @data");

    let output = Command::new(env!("CARGO_BIN_EXE_slc-local-tts-cli"))
        .arg("voices")
        .env("SLC_LOCAL_TTS_HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("couldn't start slc-local-tts-cli.exe");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(stdout.contains("en_US-joe-medium"), "{stdout}");

    let names: Vec<String> = std::fs::read_dir(home.join("logs"))
        .expect("no logs folder in the data folder")
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names
            .iter()
            .any(|name| name.starts_with("slc-local-tts.") && name.ends_with(".log")),
        "no server log file in {names:?}"
    );
}
