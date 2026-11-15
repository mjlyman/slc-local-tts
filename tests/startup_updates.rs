//! `server::start` starts the update check itself and shares its state with
//! the routes. This is the only test in its binary, so its own process:
//! `start` points `TMP` and `TEMP` at a private folder for its espeak check,
//! which would disturb other tests making temporary folders meanwhile.

use slc_local_tts::config::Config;
use slc_local_tts::paths::DataDir;
use slc_local_tts::server::{self, ServerOptions};
use slc_local_tts::update::UpdateState;

#[tokio::test]
async fn start_publishes_update_state() {
    let tmp = tempfile::tempdir().unwrap();
    let options = ServerOptions {
        updated_to: Some("1.4.0".to_string()),
        can_restart_to_update: true,
        ..ServerOptions::default()
    };

    let started = server::start(
        DataDir::at(tmp.path().join("home")),
        Config {
            port: 0,
            ..Config::default()
        },
        options,
    )
    .await
    .expect("server::start failed");

    // A test build wasn't installed with Setup.exe.
    assert_eq!(*started.updates().borrow(), UpdateState::NotInstalled);
    let state = started.state();
    assert_eq!(*state.app_info.updates.borrow(), UpdateState::NotInstalled);
    assert_eq!(state.app_info.updated_to.as_deref(), Some("1.4.0"));
    assert!(state.app_info.can_restart_to_update);
    assert_eq!(state.app_info.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(state.settings.port, started.port());
    drop(started);
}
