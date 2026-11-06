//! Starts the real `slc-local-tts.exe` and checks what it does before any of
//! its own work begins: Velopack's hook arguments must be handled and exit
//! at once, before the command line is parsed or the data folder is touched,
//! and `serve` must refuse to start while another copy holds the instance
//! mutex.

use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// How long one child process may run before the test gives up on it.
const TIMEOUT: Duration = Duration::from_secs(15);
/// How often to check whether the child has exited.
const POLL: Duration = Duration::from_millis(50);

/// Runs the exe with `args` and the data folder `home`, and returns its exit
/// status and everything it printed (stdout, then stderr).
fn run_exe(args: &[&str], home: &Path) -> (ExitStatus, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_slc-local-tts"))
        .args(args)
        .env("SLC_LOCAL_TTS_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("couldn't start slc-local-tts.exe");

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("couldn't poll the child") {
            break status;
        }
        if started.elapsed() > TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            panic!("slc-local-tts.exe {args:?} was still running after {TIMEOUT:?}");
        }
        std::thread::sleep(POLL);
    };

    // The child has exited, so its pipes are closed and these reads end. The
    // tests here print almost nothing, far below a pipe's buffer, so the
    // child can't have blocked on a full pipe while we polled.
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        stdout.read_to_string(&mut output).unwrap();
    }
    if let Some(mut stderr) = child.stderr.take() {
        stderr.read_to_string(&mut output).unwrap();
    }
    (status, output)
}

#[test]
fn velopack_hook_args_exit_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    // Not created: the exe must exit before it would create its data folder.
    let home = tmp.path().join("José Ü @home");
    for hook in [
        "--veloapp-install",
        "--veloapp-updated",
        "--veloapp-obsolete",
        "--veloapp-uninstall",
    ] {
        let (status, output) = run_exe(&[hook, "0.2.0"], &home);
        assert_eq!(status.code(), Some(0), "{hook}: {output}");
        assert_eq!(output, "", "{hook} printed something");
        assert!(!home.exists(), "{hook} created the data folder");
    }
}

/// Holds the server's instance mutex for the rest of this test process, as a
/// running server would.
///
/// The guard is forgotten, not dropped: the tests in this file run in
/// parallel in one process, so one test dropping its guard would let another
/// test's child take the mutex and start serving.
///
/// If a real server on this machine already holds the mutex, `acquire` fails
/// and the child sees that server's mutex instead, so the tests still hold.
fn hold_server_mutex() {
    if let Ok(guard) = slc_local_tts::instance::acquire() {
        std::mem::forget(guard);
    }
}

/// Makes a data folder whose `config.toml` sets a free port, so a child that
/// wrongly starts serving can't clash with a real server on 8765.
fn home_with_free_port(tmp: &Path) -> PathBuf {
    let home = tmp.join("José Ü @home");
    std::fs::create_dir_all(&home).unwrap();
    // Port 0 asks Windows for any free port; the listener closes at the end
    // of this statement, leaving the port free for the child.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    std::fs::write(home.join("config.toml"), format!("port = {port}\n")).unwrap();
    home
}

#[test]
fn serve_exits_when_already_running() {
    hold_server_mutex();
    let tmp = tempfile::tempdir().unwrap();
    let home = home_with_free_port(tmp.path());

    let (status, output) = run_exe(&["serve"], &home);
    assert_eq!(status.code(), Some(1), "{output}");
    assert!(output.contains("already running"), "{output}");
    // No models or espeak folder means the child stopped before
    // `server::start`, the first code that creates either.
    assert!(
        !home.join("models").exists(),
        "serve got as far as starting"
    );
    assert!(
        !home.join("espeak").exists(),
        "serve got as far as the espeak check"
    );
}

#[test]
fn voices_runs_while_a_server_runs() {
    hold_server_mutex();
    let tmp = tempfile::tempdir().unwrap();
    let home = home_with_free_port(tmp.path());

    let (status, output) = run_exe(&["voices"], &home);
    assert_eq!(status.code(), Some(0), "{output}");
    assert!(output.contains("en_US-joe-medium"), "{output}");
}
