//! The settings window logs to a file of its own. This test is alone in its
//! binary because `logging::init` installs the process-wide subscriber,
//! which can happen only once per process.

use slc_local_tts::config::LogLevel;
use slc_local_tts::logging::{self, ConsoleLog, WINDOW_LOG_PREFIX};

#[test]
fn window_prefix_names_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let logs = tmp.path().join("logs");

    let guard = logging::init(&logs, WINDOW_LOG_PREFIX, LogLevel::Info, ConsoleLog::Off).unwrap();
    // A warning, because this test crate isn't `slc_local_tts`: other
    // crates' lines are logged only from `warn` up.
    tracing::warn!("a line from the window");
    // Dropping the guard flushes the file writer.
    drop(guard);

    let names: Vec<String> = std::fs::read_dir(&logs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let file = names
        .iter()
        .find(|name| name.starts_with("slc-local-tts-window.") && name.ends_with(".log"))
        .unwrap_or_else(|| panic!("no window log file in {names:?}"));
    let text = std::fs::read_to_string(logs.join(file)).unwrap();
    assert!(text.contains("a line from the window"), "{text}");
}
