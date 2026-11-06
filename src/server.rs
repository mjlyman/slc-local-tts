//! Server startup and serving.
//!
//! [`start`] does everything that can fail, up to and including binding the
//! port, and returns a [`Started`] server. [`Started::serve`] then answers
//! requests until a shutdown future resolves. The split lets `main` print
//! the banner only after a successful bind, and lets a later tray UI stop the
//! server with its own shutdown signal instead of Ctrl+C.

use std::collections::HashSet;
use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::ServiceExt;
use axum::extract::Request;
use tokio::net::TcpListener;
use tokio::time::MissedTickBehavior;

use crate::api::cooldown::LoadCooldown;
use crate::api::{ApiSettings, AppState, PREVIEW_BUDGET, SPEECH_BUDGET, app};
use crate::catalog;
use crate::config::{Config, ConfigError};
use crate::engine::Synthesizer;
use crate::engine::sherpa::SherpaSynthesizer;
use crate::frontend::espeak::{self, EspeakError};
use crate::models::DiskStore;
use crate::models::install::{self, HttpInstaller};
use crate::paths::DataDir;

/// The state type the real server runs with.
type RealState = AppState<DiskStore<HttpInstaller>, SherpaSynthesizer>;

/// How often the idle unloader looks for unused models.
const UNLOAD_TICK: Duration = Duration::from_secs(60);

/// A problem that stops SLC Local TTS from starting. Every message says what
/// happened and what to do.
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error(
        "Port {port} is already in use, so SLC Local TTS can't start. Another copy may already be running: close it, or set a different `port` in {} and use the same port in SLC's Custom API URL.",
        .config_path.display()
    )]
    PortInUse { port: u16, config_path: PathBuf },
    /// Binding failed for another reason. On Windows this is often a port
    /// that Hyper-V or WSL has reserved ("access denied").
    #[error(
        "SLC Local TTS couldn't listen on port {port} ({source}). Set a different `port` in {} and use the same port in SLC's Custom API URL.",
        .config_path.display()
    )]
    Bind {
        port: u16,
        config_path: PathBuf,
        source: io::Error,
    },
    #[error(
        "SLC Local TTS can't write to {}. Check that the folder exists and isn't read-only, then start again.",
        .path.display()
    )]
    DataDirNotWritable { path: PathBuf },
    #[error(transparent)]
    Espeak(#[from] EspeakError),
    // `ConfigError` messages already say what to fix, so this only adds
    // where the file is.
    #[error(
        "{error}\nThe settings file is {}; deleting it brings back the defaults.",
        .path.display()
    )]
    Config { error: ConfigError, path: PathBuf },
    /// A bug, such as a panic during startup.
    #[error(
        "SLC Local TTS hit an unexpected problem while starting ({detail}). Start it again. If it keeps happening, report it and include the newest log file from {}.",
        .logs.display()
    )]
    Unexpected { detail: String, logs: PathBuf },
    #[error("The speech server stopped unexpectedly ({0}). Start SLC Local TTS again.")]
    Serve(io::Error),
}

/// Binds `127.0.0.1:<port>`, never any other address: the server has no
/// authentication, so it must not be reachable from the network.
pub async fn bind(port: u16, config_path: &Path) -> Result<TcpListener, StartupError> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpListener::bind(addr).await.map_err(|source| {
        let config_path = config_path.to_path_buf();
        if source.kind() == io::ErrorKind::AddrInUse {
            StartupError::PortInUse { port, config_path }
        } else {
            StartupError::Bind {
                port,
                config_path,
                source,
            }
        }
    })
}

/// What the console shows once the server is listening.
pub fn banner(version: &str, port: u16) -> String {
    format!(
        "SLC Local TTS {version}\n\
         Paste this into SLC's ElevenLabs \"Custom API URL\": http://127.0.0.1:{port}\n\
         Keep this window open while flying."
    )
}

/// A server that is bound and ready, but not yet answering requests.
pub struct Started {
    listener: TcpListener,
    port: u16,
    state: Arc<RealState>,
    idle_for: Duration,
}

impl Started {
    /// The port the server is listening on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Answers requests until `shutdown` resolves, then lets requests in
    /// progress finish and returns.
    ///
    /// `shutdown` is any future: `main` passes one that waits for Ctrl+C, and
    /// a tray UI could pass one that waits on a channel. It must be
    /// `Send + 'static` because axum keeps it and polls it alongside the
    /// connections, possibly on another worker thread.
    pub async fn serve(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), StartupError> {
        let unloader = tokio::spawn(idle_unloader(
            Arc::clone(&self.state.synth),
            Arc::clone(&self.state.synth_lock),
            self.idle_for,
        ));
        // `app()` wraps the router in `NormalizePath`, a plain tower service
        // rather than a `Router`, so `into_make_service` comes from
        // `axum::ServiceExt`. That trait is generic over the request type,
        // which the compiler can't infer here, hence the `::<Request>`.
        let service = ServiceExt::<Request>::into_make_service(app(self.state));
        let result = axum::serve(self.listener, service)
            .with_graceful_shutdown(shutdown)
            .await;
        // The unloader loops forever; stop it with the server. An unload
        // already running on a blocking thread finishes on its own.
        unloader.abort();
        result.map_err(StartupError::Serve)
    }
}

/// Binds the port, then prepares the data folder and espeak data. Loads no
/// models.
pub async fn start(data: DataDir, config: Config) -> Result<Started, StartupError> {
    // Bind before any cleanup.
    // A second copy started by mistake must stop before it touches any
    // files: its cleanup would delete the first copy's download in progress
    // (`models\.tmp`), and its espeak check could reset the folder the first
    // copy phonemizes from. Requests that arrive meanwhile wait in the OS
    // queue until `serve` starts.
    let listener = bind(config.port, &data.config_file()).await?;
    let port = listener
        .local_addr()
        .map_err(|source| StartupError::Bind {
            port: config.port,
            config_path: data.config_file(),
            source,
        })?
        .port();

    // Folder scans, deletes and the espeak check are blocking file work, so
    // they run on tokio's blocking thread pool. The closure must own what it
    // uses, hence the clone.
    let prepare_data = data.clone();
    tokio::task::spawn_blocking(move || prepare(&prepare_data))
        .await
        // The outer `?` is the blocking task's own failure (it panicked);
        // the inner one is `prepare`'s result.
        .map_err(|e| StartupError::Unexpected {
            detail: e.to_string(),
            logs: data.logs(),
        })??;

    Ok(Started {
        listener,
        port,
        state: build_state(&data, &config, port),
        idle_for: Duration::from_secs(config.idle_unload_minutes * 60),
    })
}

/// The blocking part of [`start`]: the models folder, then espeak.
fn prepare(data: &DataDir) -> Result<(), StartupError> {
    prepare_models_dir(&data.models());

    let espeak_root = data.espeak_root();
    if let Err(e) = espeak::prune_old_versions(&espeak_root) {
        // Old copies only waste disk space; they don't affect speech.
        tracing::warn!(
            dir = %espeak_root.display(),
            error = %e,
            "couldn't delete old espeak data folders; they'll be tried again next start"
        );
    }
    let private = espeak::private_temp_dir(&espeak_root);
    let result = {
        // TMP and TEMP point at the private folder only for the startup
        // check: piper-phonemize needs them just for its one-time init, and
        // Velopack's updater or any other child process must never inherit
        // the redirect. The guard puts them back at the end of this block.
        //
        // SAFETY: always safe on Windows (see `redirect_temp`). `prepare`
        // runs before `serve` answers any request, so this check is the
        // process's first phonemize call, and the guard is held across it.
        let _temp = unsafe { espeak::redirect_temp(&private) };
        espeak::verify_startup(&private, espeak::ipa)
    };
    if let Err(EspeakError::Damaged { got }) = &result {
        // The user-facing message doesn't include the IPA; the log does.
        tracing::warn!(
            got,
            expected = espeak::CHECK_IPA,
            "espeak startup check failed"
        );
    }
    Ok(result?)
}

/// Creates `models\`, checks it can be written, removes leftovers from
/// interrupted downloads and logs how many models are installed. Problems
/// here are warnings: the server still runs and installed voices still work.
fn prepare_models_dir(models: &Path) {
    let writable = std::fs::create_dir_all(models).and_then(|()| {
        let probe = models.join(".write-test");
        std::fs::write(&probe, b"ok")?;
        std::fs::remove_file(&probe)
    });
    if let Err(e) = writable {
        tracing::warn!(
            "SLC Local TTS can't write to {} ({e}). Voices that aren't installed yet can't be \
             downloaded: check that the folder isn't read-only, then restart SLC Local TTS.",
            models.display()
        );
    }
    if let Err(e) = install::clean_leftovers(models) {
        tracing::warn!(
            dir = %models.display(),
            error = %e,
            "couldn't delete leftover download files; they'll be tried again next start"
        );
    }
    // Several voices share one archive, so count archives, not voices.
    let installed: HashSet<&str> = catalog::voices()
        .iter()
        .map(|voice| voice.archive)
        .filter(|archive| install::is_installed(archive, models))
        .map(|archive| archive.stem)
        .collect();
    tracing::info!(count = installed.len(), "installed voice models found");
}

/// The shared state for the real store and synthesizer. Building it loads
/// nothing.
fn build_state(data: &DataDir, config: &Config, port: u16) -> Arc<RealState> {
    Arc::new(AppState {
        store: DiskStore::new(data.models(), HttpInstaller::new()),
        synth: Arc::new(SherpaSynthesizer::new(config)),
        synth_lock: Arc::new(tokio::sync::Mutex::new(())),
        load_cooldown: LoadCooldown::default(),
        settings: ApiSettings {
            port,
            speech_budget: SPEECH_BUDGET,
            preview_budget: PREVIEW_BUDGET,
            base_speed: config.base_speed.clone(),
        },
    })
}

/// Every minute, unloads models unused for `idle_for`.
///
/// The unload runs under the global synthesis lock and on a blocking thread.
/// The model cache keeps its own std mutex locked for a whole model load
/// (seconds), so calling `unload_idle` while a request loads a model would
/// block. Taking `synth_lock` first means we only get there when no synthesis
/// (and so no load) is running, and the blocking thread keeps even a short
/// wait off the async workers that answer SLC.
async fn idle_unloader<Y: Synthesizer>(
    synth: Arc<Y>,
    synth_lock: Arc<tokio::sync::Mutex<()>>,
    idle_for: Duration,
) {
    let mut ticks = tokio::time::interval(UNLOAD_TICK);
    // After a long wait for the lock, carry on a minute later instead of
    // firing the missed ticks back to back.
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // An interval's first tick completes at once; skip it so the first
    // check is a minute after startup.
    ticks.tick().await;
    loop {
        ticks.tick().await;
        // `lock_owned` (called on an `Arc` clone) returns a guard that owns
        // its reference to the mutex instead of borrowing it. So the guard
        // can move into the `'static` blocking closure below, and the lock
        // stays held until that closure ends.
        let guard = Arc::clone(&synth_lock).lock_owned().await;
        let synth = Arc::clone(&synth);
        let unloaded = tokio::task::spawn_blocking(move || {
            let _guard = guard;
            synth.unload_idle(idle_for)
        })
        .await;
        match unloaded {
            Ok(0) => {}
            Ok(count) => tracing::info!(count, "unloaded idle voice models"),
            Err(e) => tracing::error!(error = %e, "the idle model unloader failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::Audio;
    use crate::catalog::Voice;
    use crate::config::LogLevel;
    use crate::engine::{CancelFlag, Prepared, SynthError, SynthJob};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn second_bind_reports_port_in_use_with_fix() {
        let first = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = first.local_addr().unwrap().port();
        let config_path = PathBuf::from(r"C:\data\SLC-Local-TTS\config.toml");

        let err = bind(port, &config_path).await.unwrap_err();

        assert!(matches!(err, StartupError::PortInUse { .. }), "{err:?}");
        let text = err.to_string();
        assert!(text.contains(&port.to_string()), "{text}");
        assert!(text.contains("already in use"), "{text}");
        assert!(
            text.contains(r"C:\data\SLC-Local-TTS\config.toml"),
            "{text}"
        );
        assert!(text.contains("Custom API URL"), "{text}");
    }

    #[test]
    fn banner_text() {
        assert_eq!(
            banner("0.1.0", 8765),
            "SLC Local TTS 0.1.0\n\
             Paste this into SLC's ElevenLabs \"Custom API URL\": http://127.0.0.1:8765\n\
             Keep this window open while flying."
        );
    }

    #[test]
    fn unwritable_data_dir_message() {
        let tmp = tempfile::tempdir().unwrap();
        // A file where the data folder should be: nothing can be created in it.
        let root = tmp.path().join("SLC-Local-TTS");
        std::fs::write(&root, "a file, not a folder").unwrap();
        let data = DataDir::at(root.clone());

        assert!(crate::logging::init(&data.logs(), LogLevel::Info, false).is_err());

        let text = StartupError::DataDirNotWritable { path: root.clone() }.to_string();
        assert!(text.contains(&root.display().to_string()), "{text}");
    }

    #[test]
    fn config_error_names_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "num_threads = 0\n").unwrap();
        let error = crate::config::load(&path).unwrap_err();

        let text = StartupError::Config {
            error,
            path: path.clone(),
        }
        .to_string();

        assert!(text.contains("num_threads"), "{text}");
        assert!(
            text.contains("deleting it brings back the defaults"),
            "{text}"
        );
        assert!(text.contains(&path.display().to_string()), "{text}");
        // The advice is given once, by the `ConfigError` itself.
        assert_eq!(text.matches("Fix").count(), 1, "{text}");
    }

    #[test]
    fn unexpected_error_names_logs_folder() {
        let logs = PathBuf::from(r"C:\data\SLC-Local-TTS\logs");
        let text = StartupError::Unexpected {
            detail: "task panicked".to_string(),
            logs: logs.clone(),
        }
        .to_string();
        assert_eq!(
            text,
            r"SLC Local TTS hit an unexpected problem while starting (task panicked). Start it again. If it keeps happening, report it and include the newest log file from C:\data\SLC-Local-TTS\logs."
        );
    }

    #[tokio::test]
    async fn serve_returns_ok_when_shutdown_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        let data = DataDir::at(tmp.path().to_path_buf());
        let config = Config::default();
        let listener = bind(0, &data.config_file()).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let started = Started {
            listener,
            port,
            state: build_state(&data, &config, port),
            idle_for: Duration::from_secs(600),
        };
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(started.serve(async {
            let _ = stopped.await;
        }));

        // The server answers while it runs...
        let health = reqwest::get(format!("http://127.0.0.1:{port}/health"))
            .await
            .unwrap();
        assert_eq!(health.status(), 200);
        drop(health);

        // ...and `serve` returns Ok once the shutdown future resolves.
        stop.send(()).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .expect("serve should stop after shutdown")
            .unwrap();
        assert!(result.is_ok(), "{result:?}");
    }

    /// Counts `unload_idle` calls; everything else is unused here.
    #[derive(Default)]
    struct CountingSynth(AtomicUsize);

    impl Synthesizer for CountingSynth {
        fn prepare(&self, _: &'static Voice, _: &str) -> Result<Prepared, SynthError> {
            Err(SynthError::NothingToSay)
        }
        fn synthesize(&self, _: SynthJob, _: CancelFlag) -> Result<Audio, SynthError> {
            Err(SynthError::NothingToSay)
        }
        fn unload_idle(&self, _: Duration) -> usize {
            self.0.fetch_add(1, Ordering::SeqCst);
            0
        }
    }

    #[tokio::test(start_paused = true)]
    async fn idle_unloader_waits_for_synth_lock() {
        let synth = Arc::new(CountingSynth::default());
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        let held = Arc::clone(&lock).lock_owned().await;
        let unloader = tokio::spawn(idle_unloader(
            Arc::clone(&synth),
            Arc::clone(&lock),
            Duration::from_secs(600),
        ));

        // A synthesis holds the lock past the first tick: no unload yet.
        tokio::time::sleep(Duration::from_secs(61)).await;
        assert_eq!(synth.0.load(Ordering::SeqCst), 0);

        // Once the lock is free, the waiting tick unloads.
        drop(held);
        for _ in 0..100 {
            if synth.0.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(synth.0.load(Ordering::SeqCst), 1);
        unloader.abort();
    }
}
