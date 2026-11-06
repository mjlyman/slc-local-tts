//! `server::start` unpacks espeak-ng-data into the data folder's private
//! espeak folder, then puts `TMP` and `TEMP` back. This is the only test in
//! its binary, so its own process: piper-phonemize initialises once per
//! process, and only the first phonemize call decides where its data goes.

use slc_local_tts::config::Config;
use slc_local_tts::paths::DataDir;
use slc_local_tts::server;

#[tokio::test]
async fn start_unpacks_privately_and_restores_tmp() {
    let tmp = tempfile::tempdir().unwrap();
    // Spaces, `@` and non-ASCII letters, like a real user's folder can have.
    let home = tmp.path().join("José Ü @home");
    let saved_tmp = std::env::var_os("TMP");
    let saved_temp = std::env::var_os("TEMP");

    let started = server::start(
        DataDir::at(home.clone()),
        Config {
            port: 0,
            ..Config::default()
        },
    )
    .await
    .expect("server::start failed");

    assert_eq!(std::env::var_os("TMP"), saved_tmp);
    assert_eq!(std::env::var_os("TEMP"), saved_temp);
    let phontab = home
        .join("espeak")
        .join("1.5.9")
        .join("piper-phonemize-espeak-ng-data")
        .join("espeak-ng-data")
        .join("phontab");
    assert!(phontab.exists(), "{} is missing", phontab.display());
    drop(started);
}
