//! The hand-off from a second launch to the copy that's already running.
//!
//! The running server writes its port to `running-port.txt` while it serves
//! ([`write_port_file`]). A second launch of the tray program doesn't start
//! another server: it reads that file ([`resolve_port`]) and opens the
//! settings window on that port ([`spawn_window`]). The file, rather than
//! `config.toml`, is the truth, because the port in `config.toml` may have
//! been changed since the server started.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};

use crate::paths::DataDir;

/// The port file for one run. Dropping it deletes the file, so the file
/// exists only while the server serves.
#[derive(Debug)]
pub struct PortFile {
    path: PathBuf,
}

impl Drop for PortFile {
    fn drop(&mut self) {
        // Best effort: a file left behind (after a crash, say) is
        // overwritten when the server next starts.
        let _ = fs::remove_file(&self.path);
    }
}

/// Writes `port` to the data folder's port file. Keep the returned
/// [`PortFile`] alive while serving.
pub fn write_port_file(data: &DataDir, port: u16) -> io::Result<PortFile> {
    let path = data.port_file();
    fs::write(&path, port.to_string())?;
    Ok(PortFile { path })
}

/// The port in the data folder's port file, if there is a readable one.
pub fn read_port_file(data: &DataDir) -> Option<u16> {
    fs::read_to_string(data.port_file())
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The port a second launch should open the window on: what `read` returns,
/// trying up to `tries` times `wait` apart, else `config_port`.
///
/// The retries cover a server that has taken the instance mutex but hasn't
/// bound its port yet, so hasn't written the file (it starts within a few
/// seconds). `read` is passed in so tests needn't touch the disk or wait.
pub fn resolve_port(
    mut read: impl FnMut() -> Option<u16>,
    config_port: u16,
    tries: u32,
    wait: Duration,
) -> u16 {
    for attempt in 0..tries {
        if let Some(port) = read() {
            return port;
        }
        if attempt + 1 < tries {
            std::thread::sleep(wait);
        }
    }
    config_port
}

/// True the first time it's called for this data folder, false ever after:
/// it creates the "tray explained" marker file. If the marker can't be
/// created (a read-only folder), it says false, so the window never opens
/// at every start.
pub fn take_first_run(data: &DataDir) -> bool {
    // `create_new` fails if the file already exists, so the check and the
    // creation are one step.
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(data.intro_marker())
        .is_ok()
}

/// The command line that opens the settings window on `port`.
pub fn window_args(port: u16, first_run: bool) -> Vec<String> {
    let mut args = vec!["window".to_string(), "--port".to_string(), port.to_string()];
    if first_run {
        args.push("--first-run".to_string());
    }
    args
}

/// Starts this exe again as the settings window, and doesn't wait for it.
pub fn spawn_window(port: u16, first_run: bool) -> io::Result<()> {
    // Windows lets only the program the user is using bring a window to the
    // front. This process was just started by the user, so it may; this
    // passes that right on to the window it starts.
    // SAFETY: a plain Win32 call with no pointers.
    unsafe {
        AllowSetForegroundWindow(ASFW_ANY);
    }
    let exe = std::env::current_exe()?;
    // Dropping the returned `Child` neither waits for it nor ends it.
    Command::new(exe)
        .args(window_args(port, first_run))
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_dir() -> (tempfile::TempDir, DataDir) {
        let tmp = tempfile::tempdir().unwrap();
        let data = DataDir::at(tmp.path().to_path_buf());
        (tmp, data)
    }

    #[test]
    fn port_file_round_trip_and_removed_on_drop() {
        let (_tmp, data) = data_dir();
        assert_eq!(read_port_file(&data), None);

        let file = write_port_file(&data, 9100).unwrap();
        assert_eq!(fs::read_to_string(data.port_file()).unwrap(), "9100");
        assert_eq!(read_port_file(&data), Some(9100));

        drop(file);
        assert!(!data.port_file().exists());
        assert_eq!(read_port_file(&data), None);
    }

    #[test]
    fn handoff_prefers_port_file_over_config() {
        let mut answers = [None, None, Some(9100)].into_iter();
        let mut calls = 0;
        let port = resolve_port(
            || {
                calls += 1;
                answers.next().flatten()
            },
            8765,
            10,
            Duration::ZERO,
        );
        assert_eq!(port, 9100);
        assert_eq!(calls, 3);
    }

    #[test]
    fn handoff_falls_back_to_config_port() {
        let mut calls = 0;
        let port = resolve_port(
            || {
                calls += 1;
                None
            },
            8765,
            10,
            Duration::ZERO,
        );
        assert_eq!(port, 8765);
        assert_eq!(calls, 10);
    }

    #[test]
    fn first_run_true_once() {
        let (_tmp, data) = data_dir();
        assert!(take_first_run(&data));
        assert!(data.intro_marker().is_file());
        assert!(!take_first_run(&data));
    }

    #[test]
    fn window_args_keep_port_and_flag() {
        assert_eq!(window_args(8765, false), ["window", "--port", "8765"]);
        assert_eq!(
            window_args(9100, true),
            ["window", "--port", "9100", "--first-run"]
        );
    }
}
