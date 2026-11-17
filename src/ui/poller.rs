//! The real [`Backend`]: polls the server once a second on a thread of its
//! own, and runs the pages' actions on another, so the window's drawing
//! thread never waits for the network.
//!
//! The window repaints only when a poll finds a change or an action
//! finishes; a window left alone costs one small request a second.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use eframe::egui;

use super::api::{ClientError, Preview, ServerApi};
use super::backend::{Action, Backend, Connection, Notice, Snapshot};
use super::sound::Player;
use crate::autostart::{self, RunEntry};
use crate::config_edit::{self, FileSettings};
use crate::desktop::handoff::read_port_file;
use crate::desktop::{clipboard, shell};
use crate::paths::DataDir;
use crate::status::records::CAPACITY;

/// How often the server is polled.
pub const POLL_EVERY: Duration = Duration::from_secs(1);
/// Failed polls in a row before the server counts as stopped. One failure
/// alone can be a busy moment.
pub const LOST_AFTER_FAILURES: u32 = 2;
/// How long dropping the backend waits for each of its threads.
const STOP_WAIT: Duration = Duration::from_secs(2);
/// What the window says when the server sent silence for a preview. The
/// server's reason is in the request's record, which Activity shows.
pub const PREVIEW_FAILED: &str = "Couldn't play the preview. Activity says why.";
/// The licence notices' file, next to the exe in an installed copy.
pub const NOTICES_FILE: &str = "THIRD_PARTY_NOTICES.txt";
/// What "View licences" says in a copy with no notices next to it.
pub const NO_LICENCE_FILES: &str = "This copy wasn't installed with Setup.exe, so it has no \
                                    licence files. Every release on GitHub includes them.";
/// What the window says when Windows wouldn't play a preview it was given.
pub const SOUND_FAILED: &str = "Couldn't play the preview: Windows couldn't play the sound. \
                                Check your speakers, then press Play again.";

/// Keeps the window's picture of the server up to date, one [`tick`] at a
/// time. It has no threads of its own, so the tests drive it directly.
///
/// [`tick`]: Poller::tick
pub struct Poller {
    api: Arc<dyn ServerApi>,
    snapshot: Snapshot,
    /// Failed status polls in a row.
    failures: u32,
    /// The newest record's `seq` the window holds (0 before the first).
    last_seq: u64,
    watch_voices: bool,
    /// The data folder whose port file names the server's port, if the
    /// poller follows it.
    port_file: Option<DataDir>,
}

impl Poller {
    pub fn new(api: Arc<dyn ServerApi>) -> Poller {
        Poller {
            api,
            snapshot: Snapshot::default(),
            failures: 0,
            last_seq: 0,
            watch_voices: false,
            port_file: None,
        }
    }

    /// The poller, following the port that `data`'s port file names while
    /// the server is lost.
    pub fn following(mut self, data: DataDir) -> Poller {
        self.port_file = Some(data);
        self
    }

    /// Polls the server once: its status, then any new request records,
    /// then the voices if they're wanted. True when the snapshot changed.
    pub fn tick(&mut self) -> bool {
        let status = match self.api.status() {
            Ok(status) => status,
            Err(e) => {
                self.failures = self.failures.saturating_add(1);
                if self.failures < LOST_AFTER_FAILURES {
                    return false;
                }
                if self.snapshot.connection != Connection::Lost {
                    tracing::info!("the server stopped answering ({e})");
                }
                self.follow_port_file();
                // The voice list goes with the server: its Play, Download
                // and Delete buttons could only fail now. The next answer
                // brings it back.
                let voices_gone = self.snapshot.voices.take().is_some();
                return replace(&mut self.snapshot.connection, Connection::Lost) | voices_gone;
            }
        };
        self.failures = 0;
        let mut changed = replace(&mut self.snapshot.connection, Connection::Connected);

        // Each run of the server counts its records from 1. A newest `seq`
        // below the one the window holds means the server started again,
        // so all its records are new.
        if status.latest_seq < self.last_seq {
            self.snapshot.records.clear();
            self.last_seq = 0;
            changed = true;
        }
        if status.latest_seq > self.last_seq {
            changed |= self.fetch_records();
        }

        let voices_wanted = self.watch_voices || !status.downloads.is_empty();
        changed |= replace(&mut self.snapshot.status, Some(status));
        if voices_wanted {
            match self.api.voices() {
                Ok(voices) => changed |= replace(&mut self.snapshot.voices, Some(voices)),
                Err(e) => tracing::debug!("the voice list couldn't be read ({e})"),
            }
        }
        changed
    }

    /// Switches to the port the port file names, if it names another one.
    /// Called only while the server is lost, once a tick: a server started
    /// again after the user changed the port listens on the new one, and
    /// the window would otherwise keep asking the old one.
    fn follow_port_file(&self) {
        let Some(port) = self.port_file.as_ref().and_then(read_port_file) else {
            return;
        };
        if port != self.api.port() {
            tracing::info!("the server now runs on port {port}; the window follows it");
            self.api.set_port(port);
        }
    }

    /// Adds the records newer than the last one held, keeping the newest
    /// [`CAPACITY`]. True if any arrived.
    fn fetch_records(&mut self) -> bool {
        let answer = match self.api.requests_after(self.last_seq) {
            Ok(answer) => answer,
            Err(e) => {
                // The next tick asks again from the same place.
                tracing::debug!("the request records couldn't be read ({e})");
                return false;
            }
        };
        let newest = answer.records.last().map_or(0, |record| record.seq);
        self.last_seq = self.last_seq.max(answer.latest_seq).max(newest);
        if answer.records.is_empty() {
            return false;
        }
        let records = &mut self.snapshot.records;
        records.extend(answer.records);
        if records.len() > CAPACITY {
            records.drain(..records.len() - CAPACITY);
        }
        true
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// Whether the voice list is polled even with no download running.
    pub fn set_watch_voices(&mut self, on: bool) {
        self.watch_voices = on;
    }
}

/// Puts `value` in `slot`. True if that changed it.
fn replace<T: PartialEq>(slot: &mut T, value: T) -> bool {
    if *slot == value {
        return false;
    }
    *slot = value;
    true
}

/// What an action changed, for the backend to merge into the snapshot.
/// `None` fields leave the snapshot as it was.
#[derive(Debug, Default)]
pub struct WorkerResult {
    pub notice: Option<Notice>,
    pub settings: Option<Result<FileSettings, String>>,
    pub autostart: Option<bool>,
    /// The volume the server just took on, so the snapshot's status shows
    /// it at once instead of after the next poll. `None` when it wasn't
    /// changed or the server couldn't be told.
    pub volume_applied: Option<u16>,
    /// A settings save finished (whether or not it worked), so the
    /// snapshot's count of them goes up.
    pub settings_saved: bool,
    /// The window should close itself.
    pub close_window: bool,
}

/// Runs the pages' actions. It has no egui dependency, so the tests call
/// [`Worker::perform`] directly.
pub struct Worker {
    api: Arc<dyn ServerApi>,
    data: DataDir,
    /// Where "Start when Windows starts" is stored: the user's real `Run`
    /// value in the window, a scratch key in the tests.
    autostart: RunEntry,
    /// Plays the previews. It lives as long as the worker, so a preview
    /// keeps playing after the action that started it has returned.
    player: Player,
}

impl Worker {
    pub fn new(api: Arc<dyn ServerApi>, data: DataDir, autostart: RunEntry) -> Worker {
        Worker {
            api,
            data,
            autostart,
            player: Player::new(),
        }
    }

    /// Reads what the Settings page starts from: `config.toml`'s settings
    /// and whether Windows starts SLC Local TTS.
    pub fn load(&self) -> WorkerResult {
        WorkerResult {
            settings: Some(self.read_settings()),
            autostart: self.read_autostart(),
            ..WorkerResult::default()
        }
    }

    /// Does `action` and says what changed. Blocks until it's done.
    pub fn perform(&mut self, action: Action) -> WorkerResult {
        match action {
            Action::CopyText(text) => failure_notice(clipboard::set_text(&text), |e| {
                format!("Couldn't copy to the clipboard ({e}). Try again.")
            }),
            Action::StartServer => failure_notice(shell::start_server(), |e| {
                format!("SLC Local TTS couldn't start ({e}). Start it from the Start menu instead.")
            }),
            Action::OpenLogFolder => {
                let logs = self.data.logs();
                failure_notice(shell::open(&logs), |e| {
                    format!(
                        "Couldn't open the log folder ({e}). You'll find it at {}.",
                        logs.display()
                    )
                })
            }
            Action::OpenUrl(url) => failure_notice(shell::open_url(&url), |e| {
                format!("Couldn't open {url} ({e}). Open it in your web browser instead.")
            }),
            // `act` sets the shared flag itself, the moment the page asks.
            Action::WatchVoices(_) => WorkerResult::default(),
            Action::Download(stem) => failure_notice(self.api.download(&stem), |e| {
                server_text(e, "Couldn't start the download")
            }),
            Action::Delete(stem) => failure_notice(self.api.delete(&stem), |e| {
                server_text(e, "Couldn't delete the voice")
            }),
            Action::Play(voice_id) => self.play(&voice_id),
            Action::SaveSettings { base, new } => self.save_settings(&base, &new),
            Action::ReloadSettings => self.load(),
            Action::SetAutostart(on) => self.set_autostart(on),
            Action::OpenConfig => self.open_config(),
            Action::OpenLicences => open_licences(),
            Action::ApplyUpdate => self.apply_update(),
        }
    }

    /// "Restart and update now": the server stops, installs the update and
    /// starts again, so this window closes itself. A server that can't (no
    /// update is ready after all) says why.
    fn apply_update(&self) -> WorkerResult {
        match self.api.apply_update() {
            Ok(()) => {
                tracing::info!("the server is restarting to install an update; closing the window");
                WorkerResult {
                    close_window: true,
                    ..WorkerResult::default()
                }
            }
            Err(e) => {
                let text = server_text(e, "Couldn't restart to update");
                tracing::warn!("{text}");
                error_notice(text)
            }
        }
    }

    /// Saves into `config.toml` the fields where `new` differs from `base`,
    /// the values the page drew from, keeping the file's comments. The
    /// other fields keep what the file holds now, so a hand edit since the
    /// page's last read, or an earlier save still on its way to the page,
    /// isn't undone. Then it reads the file back, so the page shows what the
    /// file holds even when the save failed.
    ///
    /// A new volume is also sent to the server once it's saved, so it
    /// applies at once. A server that can't be told only gets it at its
    /// next start, which the page's restart note says, so that isn't an
    /// error.
    fn save_settings(&self, base: &FileSettings, new: &FileSettings) -> WorkerResult {
        let path = self.data.config_file();
        let saved = match config_edit::read(&path) {
            Ok(current) => config_edit::write(&path, &changed_fields(base, new, current)),
            // A file that can't be read can't be saved either; `write` says
            // why in words for the user.
            Err(_) => config_edit::write(&path, new),
        };
        let mut volume_applied = None;
        if saved.is_ok() && new.volume_percent != base.volume_percent {
            match self.api.set_volume(new.volume_percent) {
                Ok(()) => volume_applied = Some(new.volume_percent),
                Err(e) => tracing::warn!(
                    "the volume is saved, but the server couldn't be told, so it applies at its next start ({e})"
                ),
            }
        }
        let mut result = failure_notice(saved, |e| e.to_string());
        result.volume_applied = volume_applied;
        result.settings = Some(self.read_settings());
        result.settings_saved = true;
        result
    }

    /// `config.toml`'s settings, or why they couldn't be read.
    fn read_settings(&self) -> Result<FileSettings, String> {
        config_edit::read(&self.data.config_file()).map_err(|e| {
            tracing::warn!("the settings couldn't be read ({e})");
            e.to_string()
        })
    }

    /// Turns starting with Windows on or off, then reads it back.
    fn set_autostart(&self, on: bool) -> WorkerResult {
        let changed = autostart::current_target()
            .and_then(|exe| autostart::set_enabled(&self.autostart, &exe, on));
        let mut result = failure_notice(changed, |e| {
            let what = if on { "turn on" } else { "turn off" };
            format!("Couldn't {what} starting with Windows ({e}). Try again.")
        });
        result.autostart = self.read_autostart();
        result
    }

    /// Whether Windows starts SLC Local TTS, or `None` if that couldn't be
    /// read, which leaves the switch as it was.
    fn read_autostart(&self) -> Option<bool> {
        autostart::is_enabled(&self.autostart)
            .inspect_err(|e| {
                tracing::warn!("couldn't read whether SLC Local TTS starts with Windows ({e})")
            })
            .ok()
    }

    /// Opens `config.toml` in the user's text editor, first writing the
    /// commented template if there's no file yet.
    fn open_config(&self) -> WorkerResult {
        let path = self.data.config_file();
        let opened = config_edit::ensure_exists(&path).and_then(|()| shell::open_text(&path));
        failure_notice(opened, |e| {
            format!(
                "Couldn't open config.toml ({e}). You'll find it at {}.",
                path.display()
            )
        })
    }

    /// Fetches `voice_id`'s preview and starts playing it.
    fn play(&mut self, voice_id: &str) -> WorkerResult {
        match self.api.preview_wav(voice_id) {
            // The server sent silence instead of the preview, and logged
            // and recorded why.
            Ok(Preview {
                error: Some(code), ..
            }) => {
                tracing::warn!("the preview of {voice_id} failed ({code})");
                error_notice(PREVIEW_FAILED.to_string())
            }
            Ok(Preview { wav, error: None }) => {
                if self.player.play(wav) {
                    WorkerResult::default()
                } else {
                    error_notice(SOUND_FAILED.to_string())
                }
            }
            Err(e) => failure_notice(Err(e), |e| server_text(e, "Couldn't play the preview")),
        }
    }
}

/// `current` with each field the user changed (where `new` differs from
/// `base`) taken from `new`.
fn changed_fields(base: &FileSettings, new: &FileSettings, current: FileSettings) -> FileSettings {
    FileSettings {
        check_for_updates: if new.check_for_updates != base.check_for_updates {
            new.check_for_updates
        } else {
            current.check_for_updates
        },
        num_threads: if new.num_threads != base.num_threads {
            new.num_threads
        } else {
            current.num_threads
        },
        port: if new.port != base.port {
            new.port
        } else {
            current.port
        },
        volume_percent: if new.volume_percent != base.volume_percent {
            new.volume_percent
        } else {
            current.volume_percent
        },
    }
}

/// Opens the licence notices next to the exe. A copy that wasn't installed
/// with Setup.exe (a development build) has none, which isn't a failure.
fn open_licences() -> WorkerResult {
    let Some(path) = notices_path() else {
        return WorkerResult {
            notice: Some(Notice {
                text: NO_LICENCE_FILES.to_string(),
                is_error: false,
            }),
            ..WorkerResult::default()
        };
    };
    failure_notice(shell::open_text(&path), |e| {
        format!(
            "Couldn't open the licences ({e}). You'll find them at {}.",
            path.display()
        )
    })
}

/// The notices file next to this exe (in `current\` once installed), if
/// it's there.
fn notices_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe.parent()?.join(NOTICES_FILE);
    path.is_file().then_some(path)
}

/// What the user reads when a call to the server failed: the server's own
/// message when it refused, or `failed` and why otherwise.
fn server_text(e: ClientError, failed: &str) -> String {
    match e {
        ClientError::Refused { message, .. } => message,
        other => format!("{failed}: {other}. Try again."),
    }
}

/// A notice reporting a failure.
fn error_notice(text: String) -> WorkerResult {
    WorkerResult {
        notice: Some(Notice {
            text,
            is_error: true,
        }),
        ..WorkerResult::default()
    }
}

/// Nothing to say when `result` is `Ok`; an error notice from `text`
/// otherwise.
fn failure_notice<E: std::fmt::Display>(
    result: Result<(), E>,
    text: impl FnOnce(E) -> String,
) -> WorkerResult {
    match result {
        Ok(()) => WorkerResult::default(),
        Err(e) => {
            let text = text(e);
            tracing::warn!("{text}");
            error_notice(text)
        }
    }
}

/// What the backend's threads share with it.
struct Shared {
    /// The snapshot the pages see. Both threads replace it under this lock,
    /// so neither overwrites the other's change.
    published: Mutex<Arc<Snapshot>>,
    stop: AtomicBool,
    watch_voices: AtomicBool,
    ctx: egui::Context,
}

impl Shared {
    /// The lock on the published snapshot. A thread that panicked while
    /// holding it can't have left a half-written `Arc`, so a poisoned lock
    /// is still fine to use.
    fn published(&self) -> MutexGuard<'_, Arc<Snapshot>> {
        self.published
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Publishes a copy of the snapshot changed by `change`, and asks the
    /// window to draw it.
    fn publish(&self, change: impl FnOnce(&mut Snapshot)) {
        {
            let mut published = self.published();
            let mut next = Snapshot::clone(&published);
            change(&mut next);
            *published = Arc::new(next);
        }
        self.ctx.request_repaint();
    }
}

/// The real [`Backend`], with a poll thread and an action thread. Dropping
/// it stops both.
pub struct PollingBackend {
    shared: Arc<Shared>,
    /// `Option` so `drop` can take and drop the sender, which ends the
    /// action thread's loop.
    actions: Option<Sender<Action>>,
    /// Wakes the poll thread early: after an action, and to stop it.
    wake: Sender<()>,
    threads: Vec<JoinHandle<()>>,
}

impl PollingBackend {
    /// Starts polling the server through `api` and redrawing `ctx` when
    /// something changes. "Start when Windows starts" is the user's real
    /// `Run` value.
    pub fn start(
        api: Arc<dyn ServerApi>,
        ctx: egui::Context,
        data: DataDir,
    ) -> Arc<PollingBackend> {
        PollingBackend::start_with(api, ctx, data, RunEntry::user())
    }

    /// [`start`], with "Start when Windows starts" stored in `autostart`
    /// instead, so the tests never touch the real `Run` value.
    ///
    /// [`start`]: PollingBackend::start
    fn start_with(
        api: Arc<dyn ServerApi>,
        ctx: egui::Context,
        data: DataDir,
        autostart: RunEntry,
    ) -> Arc<PollingBackend> {
        let shared = Arc::new(Shared {
            published: Mutex::new(Arc::new(Snapshot::default())),
            stop: AtomicBool::new(false),
            watch_voices: AtomicBool::new(false),
            ctx,
        });
        let (actions, action_queue) = mpsc::channel();
        let (wake, wakes) = mpsc::channel();

        // Each thread gets its own `Arc` handles. Neither holds the
        // `PollingBackend` itself, so dropping the window's last handle to
        // it runs `drop` below, which stops them.
        let poller = Poller::new(Arc::clone(&api)).following(data.clone());
        let poll_shared = Arc::clone(&shared);
        let poll_thread = thread::Builder::new()
            .name("window-poll".to_string())
            .spawn(move || poll_loop(poller, &poll_shared, &wakes));
        let worker = Worker::new(api, data, autostart);
        let action_shared = Arc::clone(&shared);
        let action_wake = wake.clone();
        let action_thread = thread::Builder::new()
            .name("window-actions".to_string())
            .spawn(move || {
                // Opening folders and pages goes through `ShellExecuteW`,
                // which wants COM set up on its thread.
                shell::with_com(|| {
                    action_loop(worker, &action_shared, &action_queue, &action_wake)
                });
            });

        let mut threads = Vec::new();
        for spawned in [poll_thread, action_thread] {
            match spawned {
                Ok(handle) => threads.push(handle),
                Err(e) => tracing::error!("the settings window couldn't start a thread ({e})"),
            }
        }
        Arc::new(PollingBackend {
            shared,
            actions: Some(actions),
            wake,
            threads,
        })
    }
}

impl Backend for PollingBackend {
    fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(&self.shared.published())
    }

    fn act(&self, action: Action) {
        // Set here, at once, rather than by the action thread after the
        // actions queued before it, which may take a while (a preview, a
        // delete), so a tab switch is never kept waiting.
        if let Action::WatchVoices(on) = action {
            self.shared.watch_voices.store(on, Ordering::SeqCst);
            let _ = self.wake.send(());
        }
        if let Some(actions) = &self.actions {
            // Fails only once the action thread has stopped.
            let _ = actions.send(action);
        }
    }
}

impl Drop for PollingBackend {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        let _ = self.wake.send(());
        drop(self.actions.take());
        // A thread in the middle of a request finishes it first. A long one
        // (a delete, a preview) isn't waited for: the window is closing, and
        // the process ends without it.
        let deadline = Instant::now() + STOP_WAIT;
        for handle in self.threads.drain(..) {
            while !handle.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}

/// The poll thread: a tick now, then one every [`POLL_EVERY`] or as soon as
/// something wakes it, until the backend stops.
fn poll_loop(mut poller: Poller, shared: &Shared, wakes: &Receiver<()>) {
    while !shared.stop.load(Ordering::SeqCst) {
        poller.set_watch_voices(shared.watch_voices.load(Ordering::SeqCst));
        if poller.tick() {
            let polled = poller.snapshot();
            shared.publish(|next| {
                next.connection = polled.connection;
                next.status = polled.status.clone();
                next.voices = polled.voices.clone();
                next.records = polled.records.clone();
            });
        }
        match wakes.recv_timeout(POLL_EVERY) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// The action thread: runs each action in turn, publishes what it changed,
/// then wakes the poll thread so the result of the action shows at once.
fn action_loop(mut worker: Worker, shared: &Shared, queue: &Receiver<Action>, wake: &Sender<()>) {
    // First what the Settings page starts from, so it's there by the time
    // the user opens it.
    let loaded = worker.load();
    shared.publish(|next| merge(next, loaded));
    // `recv` fails once the backend has dropped its sender.
    while let Ok(action) = queue.recv() {
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }
        // A page switch isn't something the user asked to be done, so it
        // leaves the notice alone: opening Activity to read why a preview
        // failed mustn't hide the notice that sent them there.
        let is_page_switch = matches!(action, Action::WatchVoices(_) | Action::ReloadSettings);
        let result = worker.perform(action);
        if result.close_window {
            shared.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if !is_page_switch {
            shared.publish(|next| merge(next, result));
        } else if result.settings.is_some() || result.autostart.is_some() {
            shared.publish(|next| merge_keeping_notice(next, result));
        }
        let _ = wake.send(());
    }
}

/// Puts what a finished action changed into the snapshot `next`. Its
/// notice always replaces the one before, so an action with nothing to say
/// clears an earlier failure.
fn merge(next: &mut Snapshot, mut result: WorkerResult) {
    next.notice = result.notice.take();
    merge_keeping_notice(next, result);
}

/// [`merge`] for a page switch, which leaves the notice as it was.
fn merge_keeping_notice(next: &mut Snapshot, result: WorkerResult) {
    if let Some(settings) = result.settings {
        next.settings = Some(settings);
    }
    if let Some(autostart) = result.autostart {
        next.autostart = autostart;
    }
    // The status is the poll thread's, but it only re-reads the server
    // later; set the new volume now, so the page doesn't briefly compare
    // the saved file with the old volume and show its restart note.
    if let (Some(percent), Some(status)) = (result.volume_applied, next.status.as_mut()) {
        status.config.volume_percent = percent;
    }
    if result.settings_saved {
        next.settings_saves += 1;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::api::slc::dto::{ActivityDto, DownloadDto, ModelDto, RequestsDto, StatusDto};
    use crate::autostart::scratch::ScratchKey;
    use crate::config::Config;
    use crate::models::ModelState;
    use crate::status::records::{RequestRecord, Route};
    use crate::ui::api::{ClientError, Preview};
    use crate::update::UpdateState;

    fn status(latest_seq: u64) -> StatusDto {
        StatusDto {
            app: "SLC Local TTS".to_string(),
            version: "0.0.0-test".to_string(),
            port: 8765,
            state: ActivityDto::Idle,
            last_request_ms: None,
            ever_connected: true,
            recent_failures: 0,
            failed_downloads: 0,
            downloads: Vec::new(),
            latest_seq,
            update: UpdateState::UpToDate,
            can_restart_to_update: false,
            updated_to: None,
            config: Config::default(),
        }
    }

    fn record(seq: u64) -> RequestRecord {
        RequestRecord {
            seq,
            at_ms: 1_000 + seq,
            route: Route::Speech,
            voice_id: "M1".to_string(),
            text: "Hello".to_string(),
            outcome: "spoken".to_string(),
            phase: None,
            reason: None,
            message: None,
            audio_seconds: Some(1.0),
            ms: 10,
        }
    }

    fn unreachable() -> ClientError {
        ClientError::Unreachable("connection refused".to_string())
    }

    /// A server that answers from a script. Each status answer is taken
    /// from `statuses` in turn, and the last one repeats.
    #[derive(Default)]
    struct FakeApi {
        statuses: Mutex<VecDeque<Result<StatusDto, ClientError>>>,
        /// The records the server holds; `requests_after` filters them.
        records: Mutex<Vec<RequestRecord>>,
        /// The `seq` each `requests_after` call asked after.
        asked_after: Mutex<Vec<u64>>,
        voices_calls: Mutex<u32>,
        /// Every download, delete, preview and volume asked for, in order, such as
        /// "delete vits-piper-en_US-joe-medium".
        calls: Mutex<Vec<String>>,
        /// What download, delete and set_volume answer with, when they fail.
        refusal: Mutex<Option<ClientError>>,
        /// The `X-SLC-Local-TTS-Error` code a preview carries, if any.
        preview_error: Mutex<Option<String>>,
        /// When set, each delete waits for one message on it, so a test
        /// can hold the action thread inside a slow action.
        delete_gate: Mutex<Option<Receiver<()>>>,
        /// The port calls go to (0 until set).
        port: Mutex<u16>,
        /// The port each status call went to.
        status_ports: Mutex<Vec<u16>>,
        /// A `config.toml` that `set_volume` reads when it's called, so a
        /// test can see what the file held at that moment.
        config_file: Mutex<Option<PathBuf>>,
        /// The file's volume each `set_volume` call saw (`None` if it
        /// couldn't be read).
        volume_in_file: Mutex<Vec<Option<u16>>>,
    }

    impl FakeApi {
        fn script(statuses: Vec<Result<StatusDto, ClientError>>) -> Arc<FakeApi> {
            Arc::new(FakeApi {
                statuses: Mutex::new(statuses.into()),
                ..FakeApi::default()
            })
        }

        fn set_records(&self, seqs: impl IntoIterator<Item = u64>) {
            *self.records.lock().unwrap() = seqs.into_iter().map(record).collect();
        }

        fn asked_after(&self) -> Vec<u64> {
            self.asked_after.lock().unwrap().clone()
        }

        fn voices_calls(&self) -> u32 {
            *self.voices_calls.lock().unwrap()
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn call(&self, what: &str, arg: &str) {
            self.calls.lock().unwrap().push(format!("{what} {arg}"));
        }

        fn refused(&self) -> Result<(), ClientError> {
            match self.refusal.lock().unwrap().clone() {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }
    }

    impl ServerApi for FakeApi {
        fn status(&self) -> Result<StatusDto, ClientError> {
            self.status_ports.lock().unwrap().push(self.port());
            let mut statuses = self.statuses.lock().unwrap();
            if statuses.len() > 1 {
                statuses.pop_front().unwrap()
            } else {
                statuses.front().cloned().expect("a scripted status")
            }
        }

        fn voices(&self) -> Result<Vec<ModelDto>, ClientError> {
            *self.voices_calls.lock().unwrap() += 1;
            Ok(vec![ModelDto {
                stem: "kokoro-multi-lang-v1_0".to_string(),
                engine: "kokoro".to_string(),
                size: 350_000_000,
                state: ModelState::NotInstalled,
                voices: Vec::new(),
            }])
        }

        fn requests_after(&self, seq: u64) -> Result<RequestsDto, ClientError> {
            self.asked_after.lock().unwrap().push(seq);
            let records = self.records.lock().unwrap();
            let latest_seq = records.last().map_or(0, |r| r.seq);
            Ok(RequestsDto {
                records: records.iter().filter(|r| r.seq > seq).cloned().collect(),
                latest_seq,
            })
        }

        fn download(&self, stem: &str) -> Result<(), ClientError> {
            self.call("download", stem);
            self.refused()
        }

        fn delete(&self, stem: &str) -> Result<(), ClientError> {
            self.call("delete", stem);
            if let Some(gate) = self.delete_gate.lock().unwrap().as_ref() {
                let _ = gate.recv();
            }
            self.refused()
        }

        fn apply_update(&self) -> Result<(), ClientError> {
            self.call("apply", "update");
            self.refused()
        }

        fn set_volume(&self, percent: u16) -> Result<(), ClientError> {
            self.call("set_volume", &percent.to_string());
            if let Some(path) = self.config_file.lock().unwrap().as_ref() {
                let seen = config_edit::read(path).ok().map(|s| s.volume_percent);
                self.volume_in_file.lock().unwrap().push(seen);
            }
            self.refused()
        }

        fn preview_wav(&self, voice_id: &str) -> Result<Preview, ClientError> {
            self.call("preview", voice_id);
            Ok(Preview {
                wav: Vec::new(),
                error: self.preview_error.lock().unwrap().clone(),
            })
        }

        fn port(&self) -> u16 {
            *self.port.lock().unwrap()
        }

        fn set_port(&self, port: u16) {
            *self.port.lock().unwrap() = port;
        }
    }

    fn seqs(poller: &Poller) -> Vec<u64> {
        poller.snapshot().records.iter().map(|r| r.seq).collect()
    }

    #[test]
    fn lost_after_two_failures_and_recovers() {
        let api = FakeApi::script(vec![
            Ok(status(0)),
            Err(unreachable()),
            Err(unreachable()),
            Ok(status(0)),
        ]);
        let mut poller = Poller::new(api);
        assert_eq!(poller.snapshot().connection, Connection::Connecting);

        assert!(poller.tick());
        assert_eq!(poller.snapshot().connection, Connection::Connected);
        poller.tick();
        assert_eq!(poller.snapshot().connection, Connection::Connected);
        assert!(poller.tick());
        assert_eq!(poller.snapshot().connection, Connection::Lost);
        // The last answer is kept, so the pages can say what was going on.
        assert!(poller.snapshot().status.is_some());
        assert!(poller.tick());
        assert_eq!(poller.snapshot().connection, Connection::Connected);
    }

    /// A data folder in `tmp` whose port file names `port`.
    fn port_file_naming(tmp: &tempfile::TempDir, port: u16) -> DataDir {
        let data = DataDir::at(tmp.path().to_path_buf());
        std::fs::write(data.port_file(), port.to_string()).unwrap();
        data
    }

    #[test]
    fn lost_server_follows_a_new_port() {
        let api = FakeApi::script(vec![
            Ok(status(0)),
            Err(unreachable()),
            Err(unreachable()),
            Ok(status(0)),
        ]);
        api.set_port(8765);
        let tmp = tempfile::tempdir().unwrap();
        let data = port_file_naming(&tmp, 9100);
        let mut poller = Poller::new(Arc::clone(&api) as Arc<dyn ServerApi>).following(data);

        poller.tick();
        poller.tick();
        poller.tick();
        assert_eq!(poller.snapshot().connection, Connection::Lost);
        assert!(poller.tick());
        assert_eq!(poller.snapshot().connection, Connection::Connected);
        assert_eq!(
            *api.status_ports.lock().unwrap(),
            vec![8765, 8765, 8765, 9100]
        );
    }

    #[test]
    fn connected_server_ignores_the_port_file() {
        let api = FakeApi::script(vec![Ok(status(0)), Err(unreachable()), Ok(status(0))]);
        api.set_port(8765);
        let tmp = tempfile::tempdir().unwrap();
        let data = port_file_naming(&tmp, 9100);
        let mut poller = Poller::new(Arc::clone(&api) as Arc<dyn ServerApi>).following(data);

        // Connected, then one failed poll, which isn't lost yet.
        for _ in 0..4 {
            poller.tick();
        }
        assert_eq!(poller.snapshot().connection, Connection::Connected);
        assert_eq!(*api.status_ports.lock().unwrap(), vec![8765; 4]);
    }

    #[test]
    fn records_fetched_incrementally_and_capped() {
        let api = FakeApi::script(vec![Ok(status(3)), Ok(status(205))]);
        api.set_records(1..=3);
        let mut poller = Poller::new(Arc::clone(&api) as Arc<dyn ServerApi>);
        assert!(poller.tick());
        assert_eq!(seqs(&poller), vec![1, 2, 3]);

        api.set_records(1..=205);
        assert!(poller.tick());
        assert_eq!(api.asked_after(), vec![0, 3]);
        let kept = seqs(&poller);
        assert_eq!(kept.len(), CAPACITY);
        assert_eq!(kept.first(), Some(&6));
        assert_eq!(kept.last(), Some(&205));
    }

    #[test]
    fn server_restart_resets_records() {
        let api = FakeApi::script(vec![Ok(status(50)), Ok(status(2))]);
        api.set_records(1..=50);
        let mut poller = Poller::new(Arc::clone(&api) as Arc<dyn ServerApi>);
        poller.tick();
        assert_eq!(seqs(&poller).len(), 50);

        // A new server counts from 1 again.
        api.set_records(1..=2);
        assert!(poller.tick());
        assert_eq!(seqs(&poller), vec![1, 2]);
        assert_eq!(api.asked_after(), vec![0, 0]);
    }

    #[test]
    fn unchanged_tick_reports_no_change() {
        let api = FakeApi::script(vec![Ok(status(2))]);
        api.set_records(1..=2);
        let mut poller = Poller::new(Arc::clone(&api) as Arc<dyn ServerApi>);
        assert!(poller.tick());
        assert!(!poller.tick());
        // Nothing new, so the records weren't asked for again.
        assert_eq!(api.asked_after(), vec![0]);
    }

    #[test]
    fn voices_polled_only_when_watched_or_downloading() {
        let mut downloading = status(0);
        downloading.downloads = vec![DownloadDto {
            stem: "kokoro-multi-lang-v1_0".to_string(),
            done: 1,
            total: 2,
        }];
        let api = FakeApi::script(vec![
            Ok(status(0)),
            Ok(status(0)),
            Ok(downloading),
            Ok(status(0)),
        ]);
        let mut poller = Poller::new(Arc::clone(&api) as Arc<dyn ServerApi>);

        poller.tick();
        assert_eq!(api.voices_calls(), 0);
        assert_eq!(poller.snapshot().voices, None);

        poller.set_watch_voices(true);
        poller.tick();
        assert_eq!(api.voices_calls(), 1);
        assert!(poller.snapshot().voices.is_some());

        poller.set_watch_voices(false);
        poller.tick();
        assert_eq!(api.voices_calls(), 2, "a download is running");

        poller.tick();
        assert_eq!(api.voices_calls(), 2);
    }

    #[test]
    fn backend_delivers_watch_voices_and_stops_promptly() {
        let tmp = tempfile::tempdir().unwrap();
        let api = FakeApi::script(vec![Ok(status(0))]);
        let scratch = ScratchKey::new("backend");
        let backend = PollingBackend::start_with(
            Arc::clone(&api) as Arc<dyn ServerApi>,
            egui::Context::default(),
            DataDir::at(tmp.path().to_path_buf()),
            scratch.entry.clone(),
        );
        // The first tick publishes the connection.
        let deadline = Instant::now() + Duration::from_secs(3);
        while backend.snapshot().connection != Connection::Connected {
            assert!(Instant::now() < deadline, "no first tick");
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(api.voices_calls(), 0);

        backend.act(Action::WatchVoices(true));
        while backend.snapshot().voices.is_none() {
            assert!(Instant::now() < deadline, "the voices were never polled");
            thread::sleep(Duration::from_millis(10));
        }

        let started = Instant::now();
        drop(backend);
        let took = started.elapsed();
        assert!(took < STOP_WAIT, "{took:?}");
    }

    #[test]
    fn worker_reports_a_folder_it_cannot_open() {
        let tmp = tempfile::tempdir().unwrap();
        // The logs folder was never made, so Explorer can't open it.
        let data = DataDir::at(tmp.path().join("missing"));
        let api = FakeApi::script(vec![Ok(status(0))]);
        let result = Worker::new(api, data, untouched_entry()).perform(Action::OpenLogFolder);
        let notice = result.notice.expect("a notice");
        assert!(notice.is_error);
        assert!(
            notice.text.starts_with("Couldn't open the log folder"),
            "{}",
            notice.text
        );
    }

    /// A start-with-Windows entry for workers whose tests never read or
    /// change it. Tests that do use a [`ScratchKey`].
    fn untouched_entry() -> RunEntry {
        RunEntry {
            subkey: r"Software\SLC-Local-TTS-tests\never-touched".to_string(),
            value: autostart::VALUE_NAME.to_string(),
        }
    }

    fn worker(api: &Arc<FakeApi>, tmp: &tempfile::TempDir) -> Worker {
        Worker::new(
            Arc::clone(api) as Arc<dyn ServerApi>,
            DataDir::at(tmp.path().to_path_buf()),
            untouched_entry(),
        )
    }

    #[test]
    fn save_settings_writes_config() {
        let tmp = tempfile::tempdir().unwrap();
        let api = FakeApi::script(vec![Ok(status(0))]);
        let new = FileSettings {
            check_for_updates: false,
            num_threads: 3,
            port: 9100,
            volume_percent: 100,
        };
        let base = FileSettings::from_config(&Config::default());
        let result = worker(&api, &tmp).perform(Action::SaveSettings {
            base,
            new: new.clone(),
        });
        assert_eq!(result.notice, None);
        assert_eq!(result.settings, Some(Ok(new.clone())));
        assert!(result.settings_saved);
        let path = tmp.path().join("config.toml");
        assert_eq!(config_edit::read(&path).unwrap(), new);
        // Written from the commented template, so its comments are there.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# SLC Local TTS settings."), "{text}");
    }

    #[test]
    fn failed_save_is_a_notice_and_rereads() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "port = \"not a number\"\n").unwrap();
        let api = FakeApi::script(vec![Ok(status(0))]);
        let base = FileSettings::from_config(&Config::default());
        let new = FileSettings {
            num_threads: 3,
            ..base.clone()
        };
        let result = worker(&api, &tmp).perform(Action::SaveSettings { base, new });
        // A failed save still counts as come back.
        assert!(result.settings_saved);
        let notice = result.notice.expect("a notice");
        assert!(notice.is_error);
        assert!(
            notice
                .text
                .starts_with("config.toml has a mistake, so Settings can't change it:"),
            "{}",
            notice.text
        );
        // The file is still broken, and the page is told so.
        assert!(
            matches!(result.settings, Some(Err(_))),
            "{:?}",
            result.settings
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "port = \"not a number\"\n"
        );
    }

    /// The defaults, as a page that read a file holding none of the
    /// settings would have them.
    fn defaults() -> FileSettings {
        FileSettings::from_config(&Config::default())
    }

    fn saved(tmp: &tempfile::TempDir) -> FileSettings {
        config_edit::read(&tmp.path().join("config.toml")).unwrap()
    }

    #[test]
    fn hand_edit_survives_a_save_of_another_field() {
        let tmp = tempfile::tempdir().unwrap();
        let api = FakeApi::script(vec![Ok(status(0))]);
        let mut worker = worker(&api, &tmp);
        let base = defaults();
        // Edited by hand after the page read the file.
        std::fs::write(
            tmp.path().join("config.toml"),
            "num_threads = 6
",
        )
        .unwrap();

        let result = worker.perform(Action::SaveSettings {
            new: FileSettings {
                check_for_updates: false,
                ..base.clone()
            },
            base,
        });
        let expected = FileSettings {
            check_for_updates: false,
            num_threads: 6,
            ..defaults()
        };
        assert_eq!(result.notice, None);
        assert_eq!(saved(&tmp), expected);
        assert_eq!(result.settings, Some(Ok(expected)));
    }

    #[test]
    fn two_saves_from_one_base_both_survive() {
        let tmp = tempfile::tempdir().unwrap();
        let api = FakeApi::script(vec![Ok(status(0))]);
        let mut worker = worker(&api, &tmp);
        let base = defaults();
        // Both queued before the first one's result reached the page.
        worker.perform(Action::SaveSettings {
            base: base.clone(),
            new: FileSettings {
                num_threads: 3,
                ..base.clone()
            },
        });
        worker.perform(Action::SaveSettings {
            base: base.clone(),
            new: FileSettings {
                port: 9100,
                ..base.clone()
            },
        });
        assert_eq!(
            saved(&tmp),
            FileSettings {
                num_threads: 3,
                port: 9100,
                ..defaults()
            }
        );
    }

    #[test]
    fn value_set_back_to_base_is_written() {
        let tmp = tempfile::tempdir().unwrap();
        let api = FakeApi::script(vec![Ok(status(0))]);
        let mut worker = worker(&api, &tmp);
        let first = defaults();
        let three = FileSettings {
            num_threads: 3,
            ..first.clone()
        };
        worker.perform(Action::SaveSettings {
            base: first.clone(),
            new: three.clone(),
        });
        // The page now shows 3 and the user goes back to 2, which is what
        // the page first read.
        worker.perform(Action::SaveSettings {
            base: three,
            new: first.clone(),
        });
        assert_eq!(saved(&tmp), first);
        // The template's line became a live one, so it isn't just the
        // default showing through.
        let text = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
        assert!(
            text.contains(
                "
num_threads = 2"
            ),
            "{text}"
        );
    }

    /// The `set_volume` calls `api` saw, such as "set_volume 130".
    fn volume_calls(api: &FakeApi) -> Vec<String> {
        api.calls()
            .into_iter()
            .filter(|c| c.starts_with("set_volume"))
            .collect()
    }

    #[test]
    fn saving_a_new_volume_tells_the_server() {
        let tmp = tempfile::tempdir().unwrap();
        let api = FakeApi::script(vec![Ok(status(0))]);
        *api.config_file.lock().unwrap() = Some(tmp.path().join("config.toml"));
        let base = defaults();
        let new = FileSettings {
            volume_percent: 130,
            ..base.clone()
        };

        let result = worker(&api, &tmp).perform(Action::SaveSettings { base, new });

        assert_eq!(result.notice, None);
        assert_eq!(saved(&tmp).volume_percent, 130);
        assert_eq!(volume_calls(&api), ["set_volume 130"]);
        // The file already held the new volume when the server was told.
        assert_eq!(*api.volume_in_file.lock().unwrap(), [Some(130)]);
    }

    #[test]
    fn saving_other_settings_leaves_volume_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let api = FakeApi::script(vec![Ok(status(0))]);
        let base = defaults();
        let new = FileSettings {
            num_threads: 3,
            ..base.clone()
        };

        worker(&api, &tmp).perform(Action::SaveSettings { base, new });

        assert_eq!(saved(&tmp).num_threads, 3);
        assert_eq!(volume_calls(&api), Vec::<String>::new());
    }

    #[test]
    fn failed_save_does_not_change_the_server() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "port = \"not a number\"\n").unwrap();
        let api = FakeApi::script(vec![Ok(status(0))]);
        let base = defaults();
        let new = FileSettings {
            volume_percent: 130,
            ..base.clone()
        };

        let result = worker(&api, &tmp).perform(Action::SaveSettings { base, new });

        assert!(result.notice.expect("a notice").is_error);
        assert_eq!(volume_calls(&api), Vec::<String>::new());
    }

    #[test]
    fn volume_saved_while_server_is_down() {
        let tmp = tempfile::tempdir().unwrap();
        let api = FakeApi::script(vec![Ok(status(0))]);
        *api.refusal.lock().unwrap() = Some(unreachable());
        let base = defaults();
        let new = FileSettings {
            volume_percent: 70,
            ..base.clone()
        };

        let result = worker(&api, &tmp).perform(Action::SaveSettings {
            base,
            new: new.clone(),
        });

        // The file has it, so it applies at the next start; the page's
        // restart note says so, not an error.
        assert_eq!(saved(&tmp).volume_percent, 70);
        assert_eq!(volume_calls(&api), ["set_volume 70"]);
        assert_eq!(result.notice, None);
        assert_eq!(result.settings, Some(Ok(new)));
    }

    /// Saves a volume of 130 (the server started at 100), merges the result
    /// into a snapshot holding the server's status, and returns it.
    fn snapshot_after_volume_save(api: &Arc<FakeApi>) -> Snapshot {
        let tmp = tempfile::tempdir().unwrap();
        let base = defaults();
        let new = FileSettings {
            volume_percent: 130,
            ..base.clone()
        };
        let result = worker(api, &tmp).perform(Action::SaveSettings { base, new });
        let mut snapshot = Snapshot {
            status: Some(status(0)),
            ..Snapshot::default()
        };
        merge(&mut snapshot, result);
        snapshot
    }

    /// Whether the Settings page would show its restart note for `snapshot`.
    fn restart_note_shows(snapshot: &Snapshot) -> bool {
        let file = snapshot.settings.clone().unwrap().unwrap();
        file != FileSettings::from_config(&snapshot.status.as_ref().unwrap().config)
    }

    #[test]
    fn applied_volume_reaches_the_status_with_the_file() {
        let api = FakeApi::script(vec![Ok(status(0))]);

        let snapshot = snapshot_after_volume_save(&api);

        // The same snapshot that brings the re-read file has the server's
        // new volume, so the restart note doesn't flash up for a frame.
        assert_eq!(snapshot.status.as_ref().unwrap().config.volume_percent, 130);
        assert!(!restart_note_shows(&snapshot));
    }

    #[test]
    fn volume_the_server_missed_stays_pending() {
        let api = FakeApi::script(vec![Ok(status(0))]);
        *api.refusal.lock().unwrap() = Some(unreachable());

        let snapshot = snapshot_after_volume_save(&api);

        // The server still plays at 100, so the restart note says the new
        // volume waits for the next start.
        assert_eq!(snapshot.status.as_ref().unwrap().config.volume_percent, 100);
        assert!(restart_note_shows(&snapshot));
    }

    #[test]
    fn reload_settings_reads_again_and_keeps_notice() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = ScratchKey::new("worker-reload");
        let api = FakeApi::script(vec![Ok(status(0))]);
        let mut worker = Worker::new(
            Arc::clone(&api) as Arc<dyn ServerApi>,
            DataDir::at(tmp.path().to_path_buf()),
            scratch.entry.clone(),
        );
        std::fs::write(
            tmp.path().join("config.toml"),
            "num_threads = 5
",
        )
        .unwrap();
        let result = worker.perform(Action::ReloadSettings);
        let reread = FileSettings {
            num_threads: 5,
            ..defaults()
        };
        assert_eq!(result.settings, Some(Ok(reread.clone())));
        assert_eq!(result.autostart, Some(false));

        let notice = Notice {
            text: "Couldn't start the download.".to_string(),
            is_error: true,
        };
        let mut snapshot = Snapshot {
            settings: Some(Ok(defaults())),
            notice: Some(notice.clone()),
            ..Snapshot::default()
        };
        merge_keeping_notice(&mut snapshot, result);
        assert_eq!(snapshot.settings, Some(Ok(reread)));
        assert_eq!(snapshot.notice, Some(notice));
    }

    #[test]
    fn open_licences_in_dev_build_is_a_notice() {
        let tmp = tempfile::tempdir().unwrap();
        let api = FakeApi::script(vec![Ok(status(0))]);
        // A test binary in `target\` has no notices next to it.
        let result = worker(&api, &tmp).perform(Action::OpenLicences);
        assert_eq!(
            result.notice,
            Some(Notice {
                text: "This copy wasn't installed with Setup.exe, so it has no licence files. \
                       Every release on GitHub includes them."
                    .to_string(),
                is_error: false,
            })
        );
    }

    #[test]
    fn set_autostart_changes_the_given_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = ScratchKey::new("worker-autostart");
        let api = FakeApi::script(vec![Ok(status(0))]);
        let mut worker = Worker::new(
            Arc::clone(&api) as Arc<dyn ServerApi>,
            DataDir::at(tmp.path().to_path_buf()),
            scratch.entry.clone(),
        );
        assert_eq!(worker.load().autostart, Some(false));

        let on = worker.perform(Action::SetAutostart(true));
        assert_eq!(on.notice, None);
        assert_eq!(on.autostart, Some(true));
        assert!(autostart::is_enabled(&scratch.entry).unwrap());

        let off = worker.perform(Action::SetAutostart(false));
        assert_eq!(off.autostart, Some(false));
        assert!(!autostart::is_enabled(&scratch.entry).unwrap());
    }

    #[test]
    fn load_reads_the_settings_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("config.toml"), "port = 9100\n").unwrap();
        let scratch = ScratchKey::new("worker-load");
        let api = FakeApi::script(vec![Ok(status(0))]);
        let worker = Worker::new(
            Arc::clone(&api) as Arc<dyn ServerApi>,
            DataDir::at(tmp.path().to_path_buf()),
            scratch.entry.clone(),
        );
        let loaded = worker.load();
        let settings = loaded.settings.expect("settings").expect("readable");
        assert_eq!(settings.port, 9100);
        assert_eq!(loaded.autostart, Some(false));
        assert_eq!(loaded.notice, None);
    }

    #[test]
    fn backend_publishes_settings_at_start() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("config.toml"), "num_threads = 3\n").unwrap();
        let api = FakeApi::script(vec![Ok(status(0))]);
        let scratch = ScratchKey::new("backend-load");
        let backend = PollingBackend::start_with(
            Arc::clone(&api) as Arc<dyn ServerApi>,
            egui::Context::default(),
            DataDir::at(tmp.path().to_path_buf()),
            scratch.entry.clone(),
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while backend.snapshot().settings.is_none() {
            assert!(Instant::now() < deadline, "the settings were never read");
            thread::sleep(Duration::from_millis(5));
        }
        let snapshot = backend.snapshot();
        let settings = snapshot.settings.clone().unwrap().unwrap();
        assert_eq!(settings.num_threads, 3);
        assert!(!snapshot.autostart);
        drop(backend);
    }

    #[test]
    fn apply_update_closes_the_window_or_says_why_not() {
        let tmp = tempfile::tempdir().unwrap();
        let api = FakeApi::script(vec![Ok(status(0))]);
        let result = worker(&api, &tmp).perform(Action::ApplyUpdate);
        assert!(result.close_window);
        assert_eq!(result.notice, None);
        assert_eq!(api.calls(), vec!["apply update"]);

        *api.refusal.lock().unwrap() = Some(ClientError::Refused {
            status: 409,
            message: "No update is ready to install.".to_string(),
        });
        let result = worker(&api, &tmp).perform(Action::ApplyUpdate);
        assert!(!result.close_window);
        assert_eq!(
            result.notice,
            Some(Notice {
                text: "No update is ready to install.".to_string(),
                is_error: true,
            })
        );
    }

    #[test]
    fn failed_preview_becomes_notice() {
        let tmp = tempfile::tempdir().unwrap();
        let api = FakeApi::script(vec![Ok(status(0))]);
        *api.preview_error.lock().unwrap() = Some("model_download_failed".to_string());
        let result = worker(&api, &tmp).perform(Action::Play("M3".to_string()));
        assert_eq!(
            result.notice,
            Some(Notice {
                text: "Couldn't play the preview. Activity says why.".to_string(),
                is_error: true,
            })
        );
        assert_eq!(api.calls(), vec!["preview M3"]);
    }

    #[test]
    fn unplayable_preview_becomes_notice() {
        let tmp = tempfile::tempdir().unwrap();
        let api = FakeApi::script(vec![Ok(status(0))]);
        // The fake's preview is empty, which Windows can't play.
        let result = worker(&api, &tmp).perform(Action::Play("M3".to_string()));
        assert_eq!(
            result.notice,
            Some(Notice {
                text: "Couldn't play the preview: Windows couldn't play the sound. Check your speakers, then press Play again."
                    .to_string(),
                is_error: true,
            })
        );
        assert_eq!(api.calls(), vec!["preview M3"]);
    }

    #[test]
    fn merge_replaces_or_clears_notice() {
        let mut snapshot = Snapshot::default();
        let failed = Notice {
            text: "Couldn't start the download.".to_string(),
            is_error: true,
        };
        merge(
            &mut snapshot,
            WorkerResult {
                notice: Some(failed.clone()),
                ..WorkerResult::default()
            },
        );
        assert_eq!(snapshot.notice, Some(failed));
        // The next action said nothing, so the old notice goes.
        merge(&mut snapshot, WorkerResult::default());
        assert_eq!(snapshot.notice, None);
    }

    #[test]
    fn merge_counts_settings_saves() {
        let mut snapshot = Snapshot::default();
        let saved = || WorkerResult {
            settings_saved: true,
            ..WorkerResult::default()
        };
        merge(&mut snapshot, saved());
        merge(&mut snapshot, WorkerResult::default());
        merge_keeping_notice(&mut snapshot, saved());
        assert_eq!(snapshot.settings_saves, 2);
    }

    #[test]
    fn merge_without_settings_keeps_them() {
        let settings = FileSettings::from_config(&Config::default());
        let mut snapshot = Snapshot {
            settings: Some(Ok(settings.clone())),
            autostart: true,
            ..Snapshot::default()
        };
        // An action that changed neither leaves both as they were.
        merge(&mut snapshot, WorkerResult::default());
        assert_eq!(snapshot.settings, Some(Ok(settings)));
        assert!(snapshot.autostart);
    }

    #[test]
    fn notice_survives_watch_voices() {
        let tmp = tempfile::tempdir().unwrap();
        let api = FakeApi::script(vec![Ok(status(0))]);
        *api.refusal.lock().unwrap() = Some(unreachable());
        let (release, gate) = mpsc::channel();
        *api.delete_gate.lock().unwrap() = Some(gate);
        let scratch = ScratchKey::new("backend");
        let backend = PollingBackend::start_with(
            Arc::clone(&api) as Arc<dyn ServerApi>,
            egui::Context::default(),
            DataDir::at(tmp.path().to_path_buf()),
            scratch.entry.clone(),
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        let wait_for = |what: &str, done: &dyn Fn() -> bool| {
            while !done() {
                assert!(Instant::now() < deadline, "{what}");
                thread::sleep(Duration::from_millis(5));
            }
        };

        // A failed delete leaves a notice.
        backend.act(Action::Delete("a".to_string()));
        release.send(()).unwrap();
        wait_for("the failed delete left no notice", &|| {
            backend.snapshot().notice.is_some()
        });
        let notice = backend.snapshot().notice.clone();

        // A tab switch, then a delete held at the gate. Once that delete
        // has started, the action thread has finished the tab switch.
        backend.act(Action::WatchVoices(true));
        backend.act(Action::Delete("b".to_string()));
        wait_for("the second delete never started", &|| {
            api.calls() == vec!["delete a", "delete b"]
        });
        assert_eq!(
            backend.snapshot().notice,
            notice,
            "a tab switch cleared the notice"
        );

        release.send(()).unwrap();
        drop(backend);
    }

    #[test]
    fn lost_server_clears_voices() {
        let api = FakeApi::script(vec![
            Ok(status(0)),
            Err(unreachable()),
            Err(unreachable()),
            Ok(status(0)),
        ]);
        let mut poller = Poller::new(api);
        poller.set_watch_voices(true);
        poller.tick();
        assert!(poller.snapshot().voices.is_some());
        poller.tick();
        assert!(poller.snapshot().voices.is_some(), "one failure isn't lost");
        assert!(poller.tick());
        assert_eq!(poller.snapshot().connection, Connection::Lost);
        // Its buttons could only fail now.
        assert_eq!(poller.snapshot().voices, None);
        poller.tick();
        assert!(poller.snapshot().voices.is_some());
    }

    #[test]
    fn download_and_delete_reach_the_server() {
        let tmp = tempfile::tempdir().unwrap();
        let api = FakeApi::script(vec![Ok(status(0))]);
        let mut worker = worker(&api, &tmp);
        let download = worker.perform(Action::Download("kokoro-multi-lang-v1_0".to_string()));
        let delete = worker.perform(Action::Delete("vits-piper-en_US-joe-medium".to_string()));
        assert_eq!(download.notice, None);
        assert_eq!(delete.notice, None);
        assert_eq!(
            api.calls(),
            vec![
                "download kokoro-multi-lang-v1_0",
                "delete vits-piper-en_US-joe-medium"
            ]
        );
    }

    #[test]
    fn refusal_becomes_notice_with_its_message() {
        let tmp = tempfile::tempdir().unwrap();
        let api = FakeApi::script(vec![Ok(status(0))]);
        let message = "This voice is downloading. Wait for it to finish, then delete it.";
        *api.refusal.lock().unwrap() = Some(ClientError::Refused {
            status: 409,
            message: message.to_string(),
        });
        let mut worker = worker(&api, &tmp);
        for action in [
            Action::Delete("kokoro-multi-lang-v1_0".to_string()),
            Action::Download("kokoro-multi-lang-v1_0".to_string()),
        ] {
            let notice = worker.perform(action).notice.expect("a notice");
            assert_eq!(notice.text, message);
            assert!(notice.is_error);
        }

        // No answer at all says what failed, and to try again.
        *api.refusal.lock().unwrap() = Some(unreachable());
        let notice = worker
            .perform(Action::Download("kokoro-multi-lang-v1_0".to_string()))
            .notice
            .expect("a notice");
        assert_eq!(
            notice.text,
            "Couldn't start the download: SLC Local TTS didn't answer (connection refused). \
             Try again."
        );
    }

    #[test]
    fn slow_action_keeps_newer_watch_voices() {
        let tmp = tempfile::tempdir().unwrap();
        let api = FakeApi::script(vec![Ok(status(0))]);
        let (release, gate) = mpsc::channel();
        *api.delete_gate.lock().unwrap() = Some(gate);
        let scratch = ScratchKey::new("backend");
        let backend = PollingBackend::start_with(
            Arc::clone(&api) as Arc<dyn ServerApi>,
            egui::Context::default(),
            DataDir::at(tmp.path().to_path_buf()),
            scratch.entry.clone(),
        );
        let watching = || backend.shared.watch_voices.load(Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(3);
        let wait_for = |what: &str, done: &dyn Fn() -> bool| {
            while !done() {
                assert!(Instant::now() < deadline, "{what}");
                thread::sleep(Duration::from_millis(5));
            }
        };

        // The page opens.
        backend.act(Action::WatchVoices(true));
        // Two slow deletes, then the page closes while the first one runs.
        backend.act(Action::Delete("a".to_string()));
        backend.act(Action::Delete("b".to_string()));
        wait_for("the first delete never started", &|| {
            api.calls() == vec!["delete a"]
        });
        backend.act(Action::WatchVoices(false));
        assert!(!watching());

        // When the first delete ends, the queued `WatchVoices(false)` still
        // waits behind the second delete. Nothing the action thread does in
        // between may bring back the old `true`.
        release.send(()).unwrap();
        wait_for("the second delete never started", &|| {
            api.calls() == vec!["delete a", "delete b"]
        });
        assert!(!watching(), "a slow action brought back an old flag");

        release.send(()).unwrap();
        drop(backend);
    }
}
