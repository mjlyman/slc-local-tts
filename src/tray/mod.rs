//! The icon in the taskbar corner and its menu.
//!
//! The tray runs on a thread of its own inside the server's process,
//! because Windows delivers a tray icon's clicks as messages to the thread
//! that made it, and that thread must keep reading its messages. The thread
//! sleeps until something happens: a click, a menu choice, a status change
//! the server posts ([`TrayHandle::wake`]), or the moment a failed
//! announcement leaves the last hour. It never polls.
//!
//! If the thread panics, only it ends (the release build unwinds panics
//! rather than aborting): the icon goes, the panic is logged, and the
//! server keeps running.

pub mod icon;
pub mod model;
pub mod session;
pub mod streak;
pub mod tooltip;

use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::watch;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
use windows_sys::Win32::System::Threading::{GetCurrentThreadId, INFINITE};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_NOREMOVE,
    PM_REMOVE, PeekMessageW, PostThreadMessageW, QS_ALLINPUT, TranslateMessage, WM_APP, WM_QUIT,
};

use crate::api::slc::dto::StatusDto;
use crate::autostart::{self, RunEntry};
use crate::desktop::{clipboard, dialog, handoff, shell};
use crate::status::from_unix_ms;
use crate::stop::{ServedFlag, StopReason, StopSignal};
use crate::ui;
use crate::ui::model::update::restart_prompt;
use crate::update::UpdateState;
use model::{TrayView, quit_prompt, ready_update, tray_guid, tray_view};
use session::SessionWindow;
use streak::FailureStreak;

/// The message [`TrayHandle::wake`] posts: "the status changed". The
/// session window posts it too, when the computer wakes from sleep.
pub(crate) const WM_WAKE: u32 = WM_APP + 1;

/// How long [`TrayHandle::shutdown`] waits for the thread to end.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(1);

/// The most [`spawn`] waits for the tray thread to report that its message
/// queue exists. That takes a moment; the icon is built only afterwards,
/// so a slow Windows shell can't hold up the server, which starts
/// answering SLC only once `spawn` has returned. This is a backstop for a
/// thread that never gets that far.
const START_WAIT: Duration = Duration::from_secs(5);

/// The menu items' IDs, which come back in menu events.
const STATE_ID: &str = "state";
const OPEN_ID: &str = "open";
const COPY_ID: &str = "copy";
const AUTOSTART_ID: &str = "autostart";
const UPDATE_ID: &str = "update";
const QUIT_ID: &str = "quit";

/// What the tray thread needs from the rest of the program.
pub struct TrayContext {
    /// The port the server listens on.
    pub port: u16,
    /// The server's status right now.
    pub snapshot: Box<dyn Fn() -> StatusDto + Send>,
    /// When the next failed announcement leaves the last hour, if one is in
    /// it.
    pub next_expiry: Box<dyn Fn() -> Option<SystemTime> + Send>,
    pub stop: StopSignal,
    pub served: ServedFlag,
    /// This program's exe, for the icon's GUID.
    pub exe: PathBuf,
}

/// The way to reach the tray thread. Clones reach the same thread.
#[derive(Clone)]
pub struct TrayHandle {
    thread_id: u32,
    /// The thread, shared by every clone so any of them can tell whether it
    /// still runs. `None` once [`shutdown`](TrayHandle::shutdown) has run.
    thread: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl TrayHandle {
    /// Tells the tray the status changed. Does nothing if the thread has
    /// ended.
    pub fn wake(&self) {
        // Checked first because Windows reuses thread IDs: once the thread
        // is gone, its ID may belong to some other thread.
        if self.running() {
            self.post(WM_WAKE);
        }
    }

    /// Removes the icon and ends the thread, waiting up to a second. Does
    /// nothing if the thread has already ended.
    pub fn shutdown(self) {
        let Some(thread) = self.lock_thread().take() else {
            return;
        };
        if !thread.is_finished() {
            // The thread returns from its loop on WM_QUIT, and dropping its
            // `TrayIcon` then removes the icon. Without this, a ghost icon
            // would stay in the taskbar corner until the mouse passed over it.
            self.post(WM_QUIT);
        }
        let deadline = Instant::now() + SHUTDOWN_WAIT;
        while !thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if thread.is_finished() {
            // A panic in the thread was already logged; nothing more to do.
            let _ = thread.join();
        } else {
            // Dropping the handle leaves the thread to end with the process.
            tracing::warn!("the tray icon didn't close within a second");
        }
    }

    fn running(&self) -> bool {
        self.lock_thread()
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
    }

    fn post(&self, message: u32) {
        // SAFETY: a plain Win32 call with no pointers. It fails harmlessly
        // if the thread has no message queue any more.
        unsafe {
            PostThreadMessageW(self.thread_id, message, 0, 0);
        }
    }

    fn lock_thread(&self) -> MutexGuard<'_, Option<JoinHandle<()>>> {
        // A poisoned lock means a panic while it was held; the handle in it
        // is still fine to use.
        self.thread
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Starts the tray thread. Returns once the thread's message queue exists,
/// so [`TrayHandle::wake`] and [`TrayHandle::shutdown`] can reach it. The
/// thread then builds the icon on its own; if that fails, it logs why and
/// ends, and the server runs on without an icon.
pub fn spawn(ctx: TrayContext) -> io::Result<TrayHandle> {
    // The thread sends its ID once, over a channel of capacity 0 (a
    // "rendezvous"): its `send` completes only when this function actually
    // receives it. If this function has given up and dropped the receiver,
    // the `send` fails instead, and the thread ends before building
    // anything, so there is never a tray that no handle can stop.
    let (ready_tx, ready_rx) = mpsc::sync_channel::<u32>(0);
    let thread = std::thread::Builder::new()
        .name("tray".to_string())
        .spawn(move || run(ctx, ready_tx))?;
    match ready_rx.recv_timeout(START_WAIT) {
        Ok(thread_id) => Ok(TrayHandle {
            thread_id,
            thread: Arc::new(Mutex::new(Some(thread))),
        }),
        // Dropping `ready_rx` (when this returns) makes the thread's `send`
        // fail, so it ends; dropping `thread` doesn't wait for that.
        Err(mpsc::RecvTimeoutError::Timeout) => Err(io::Error::other(format!(
            "the tray icon took more than {} seconds to start",
            START_WAIT.as_secs()
        ))),
        // The thread ended without reporting: it panicked while starting.
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = thread.join();
            Err(io::Error::other("the tray thread stopped while starting"))
        }
    }
}

/// Wakes the tray whenever the server's status or the update check
/// changes. Returns when neither can change any more.
pub async fn forward_changes(
    changes: watch::Receiver<u64>,
    updates: watch::Receiver<UpdateState>,
    tray: TrayHandle,
) {
    forward_changes_to(changes, updates, move || tray.wake()).await;
}

/// [`forward_changes`], calling `wake` instead of waking the tray, so a
/// test can count the wakes.
async fn forward_changes_to(
    mut changes: watch::Receiver<u64>,
    mut updates: watch::Receiver<UpdateState>,
    wake: impl Fn(),
) {
    // A receiver's `changed()` fails at once, and for ever, once its sender
    // is gone; the update check drops its sender when it finishes. So each
    // is watched only while it's open, or this would spin.
    let mut changes_open = true;
    let mut updates_open = true;
    while changes_open || updates_open {
        tokio::select! {
            changed = changes.changed(), if changes_open => match changed {
                Ok(()) => wake(),
                Err(_) => changes_open = false,
            },
            changed = updates.changed(), if updates_open => match changed {
                Ok(()) => wake(),
                Err(_) => updates_open = false,
            },
        }
    }
}

/// The tray thread.
fn run(ctx: TrayContext, ready: mpsc::SyncSender<u32>) {
    // Windows gives a thread a message queue the first time it asks for a
    // message, so ask once; posts from `wake` (even a WM_QUIT from
    // `shutdown` while the icon is still being built) then wait in it.
    // SAFETY: `msg` is a valid place for Windows to write one message.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
    }
    // SAFETY: GetCurrentThreadId has no preconditions.
    if ready.send(unsafe { GetCurrentThreadId() }).is_err() {
        // `spawn` gave up waiting, so nothing could ever stop this tray.
        return;
    }
    let tray = match Tray::build(&ctx) {
        Ok(tray) => tray,
        Err(e) => {
            tracing::error!(
                "the tray icon couldn't start ({e}); the server keeps running. Open SLC Local TTS from the Start Menu to see its window."
            );
            return;
        }
    };
    tray.run_loop(&ctx);
    // Dropping `tray` here removes the icon and the hidden window.
}

/// The icon, the menu items it changes, and the hidden session window.
struct Tray {
    icon: TrayIcon,
    /// The menu, to add and remove the update item. A `Menu` is a shared
    /// handle (like an `Rc`), so this and the icon's copy are one menu.
    menu: Menu,
    state_item: MenuItem,
    autostart_item: CheckMenuItem,
    /// "Restart to update to …", in the menu only while `view` has it.
    update_item: MenuItem,
    view: TrayView,
    /// The icon's GUID, for an installed copy.
    guid: Option<u128>,
    /// The icon and tooltip changes' runs of failures, so a change Windows
    /// keeps refusing is logged once rather than on every status change.
    icon_failures: FailureStreak,
    tooltip_failures: FailureStreak,
    // Handles the end of the session for as long as the tray lives, and
    // owns what the tray puts on the clipboard.
    session: SessionWindow,
}

impl Tray {
    fn build(ctx: &TrayContext) -> io::Result<Tray> {
        let view = tray_view(&(ctx.snapshot)());
        let state_item = MenuItem::with_id(STATE_ID, &view.state_line, false, None);
        let autostart_item = CheckMenuItem::with_id(
            AUTOSTART_ID,
            "Start with Windows",
            true,
            autostart_on().unwrap_or(false),
            None,
        );
        // Its text is set whenever it's shown.
        let update_item = MenuItem::with_id(UPDATE_ID, "", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &state_item,
            &MenuItem::with_id(OPEN_ID, "Open SLC Local TTS", true, None),
            &MenuItem::with_id(COPY_ID, "Copy SLC address", true, None),
            &PredefinedMenuItem::separator(),
            &autostart_item,
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(QUIT_ID, "Quit SLC Local TTS…", true, None),
        ])
        .map_err(io::Error::other)?;

        let session = SessionWindow::create(ctx.stop.clone(), ctx.served.clone())?;
        let mut builder = TrayIconBuilder::new()
            .with_icon(icon::tray_icon(view.badge))
            .with_tooltip(&view.tooltip)
            .with_menu(Box::new(menu.clone()))
            // A left click opens the window; only a right click shows the
            // menu.
            .with_menu_on_left_click(false);
        let guid = tray_guid(&ctx.exe);
        if let Some(guid) = guid {
            builder = builder.with_guid(guid);
        }
        let icon = builder.build().map_err(io::Error::other)?;
        // Built as if no update were ready, then shown as `view` says, so
        // the update item has one place that adds it.
        let shown = TrayView {
            update_item: None,
            ..view.clone()
        };
        let mut tray = Tray {
            icon,
            menu,
            state_item,
            autostart_item,
            update_item,
            view: shown,
            guid,
            icon_failures: FailureStreak::default(),
            tooltip_failures: FailureStreak::default(),
            session,
        };
        tray.apply(view);
        Ok(tray)
    }

    fn run_loop(mut self, ctx: &TrayContext) {
        loop {
            let expiry = (ctx.next_expiry)();
            let timeout = wait_ms(expiry, SystemTime::now());
            // Sleeps until a message arrives (for this thread or one of its
            // windows) or the timeout passes. MWMO_INPUTAVAILABLE makes it
            // return at once if messages are already waiting.
            // SAFETY: no handles are passed (count 0, null pointer).
            let woke = unsafe {
                MsgWaitForMultipleObjectsEx(
                    0,
                    std::ptr::null(),
                    timeout,
                    QS_ALLINPUT,
                    MWMO_INPUTAVAILABLE,
                )
            };
            let timed_out = woke == WAIT_TIMEOUT;
            let mut dirty = false;

            // SAFETY: `msg` is a valid place for Windows to write a message,
            // and each one is handed back to Windows as it came.
            unsafe {
                let mut msg: MSG = std::mem::zeroed();
                while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    match msg.message {
                        WM_QUIT => return,
                        WM_WAKE => dirty = true,
                        _ => {
                            // Sends it to its window's handler: the tray
                            // icon's, which turns clicks into the events
                            // read below, or the session window's.
                            TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                }
            }

            // A click or menu choice may run a message box, and a right
            // click runs the menu (inside `DispatchMessageW` above). While
            // one is open, Windows drops the wake-ups posted to this thread,
            // and clicks made meanwhile wait in the channels with no message
            // left to wake the loop. So after any such event, look at the
            // status again, and read both channels until a pass finds
            // nothing.
            while self.handle_events(ctx) {
                dirty = true;
            }

            // The clock is read only now, after any menu or box has closed:
            // the deadline may have passed while one was open.
            if dirty || needs_refresh(timed_out, expiry, SystemTime::now()) {
                self.apply(tray_view(&(ctx.snapshot)()));
            }
        }
    }

    /// Handles every tray and menu event waiting now. True if any of them
    /// could have run a message box or the menu.
    fn handle_events(&self, ctx: &TrayContext) -> bool {
        let mut modal = false;
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            modal |= may_have_run_modal(&event);
            match event {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } => open_window(ctx.port),
                // Re-read, so the check matches a change made in the
                // settings window.
                TrayIconEvent::Enter { .. } => {
                    if let Ok(on) = autostart_on() {
                        self.autostart_item.set_checked(on);
                    }
                }
                _ => {}
            }
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            self.menu_action(ctx, &event);
            modal = true;
        }
        modal
    }

    fn menu_action(&self, ctx: &TrayContext, event: &MenuEvent) {
        if event.id == OPEN_ID {
            open_window(ctx.port);
        } else if event.id == COPY_ID {
            let address = format!("http://127.0.0.1:{}", ctx.port);
            if let Err(e) = clipboard::set_text_owned(&address, self.session.hwnd()) {
                tracing::warn!("couldn't copy the SLC address ({e})");
                dialog::error(&format!(
                    "Couldn't copy the SLC address ({e}). Type {address} into SLC's Custom API URL instead."
                ));
            }
        } else if event.id == AUTOSTART_ID {
            // The menu already flipped the check; make the setting match.
            let on = self.autostart_item.is_checked();
            let changed = autostart::current_target()
                .and_then(|exe| autostart::set_enabled(&RunEntry::user(), &exe, on));
            if let Err(e) = changed {
                tracing::warn!("couldn't change the start-with-Windows setting ({e})");
                dialog::error(&format!(
                    "Couldn't change the start-with-Windows setting: {e}."
                ));
                // Show what the setting really is.
                self.autostart_item
                    .set_checked(autostart_on().unwrap_or(!on));
            }
        } else if event.id == UPDATE_ID {
            restart_to_update(ctx);
        } else if event.id == QUIT_ID {
            let last_request = (ctx.snapshot)().last_request_ms.map(from_unix_ms);
            if dialog::confirm(&quit_prompt(last_request, SystemTime::now())) {
                tracing::info!("Quit chosen from the tray icon's menu");
                ctx.stop.request(StopReason::Quit);
            }
        }
    }

    /// Shows `view`, changing only what differs from what's shown.
    fn apply(&mut self, view: TrayView) {
        if view.badge != self.view.badge {
            let result = self.icon.set_icon(Some(icon::tray_icon(view.badge)));
            self.icon_failures
                .record("couldn't change the tray icon", result);
        }
        if view.tooltip != self.view.tooltip {
            let result = self.set_tooltip(&view.tooltip);
            self.tooltip_failures
                .record("couldn't change the tray icon's tooltip", result);
        }
        if view.state_line != self.view.state_line {
            self.state_item.set_text(&view.state_line);
        }
        if view.update_item != self.view.update_item {
            self.show_update_item(view.update_item.as_deref());
        }
        self.view = view;
    }

    /// Changes the icon's tooltip to `text`.
    fn set_tooltip(&self, text: &str) -> io::Result<()> {
        match self.guid {
            // tray-icon's own `set_tooltip` names the icon by its window and
            // number, which Windows ignores for an icon registered with a
            // GUID, so that change goes through `tooltip` instead.
            Some(guid) => tooltip::set_guid_tooltip(self.icon.window_handle(), guid, text),
            None => self.icon.set_tooltip(Some(text)).map_err(io::Error::other),
        }
    }

    /// Shows the update item with `text`, or takes it out of the menu for
    /// `None`. It goes just above the last separator, next to Quit, which
    /// also stops the server.
    fn show_update_item(&self, text: Option<&str>) {
        let Some(text) = text else {
            if let Err(e) = self.menu.remove(&self.update_item) {
                tracing::warn!("couldn't take the update item out of the tray menu ({e})");
            }
            return;
        };
        self.update_item.set_text(text);
        if self.view.update_item.is_some() {
            // Already in the menu; only its text changed.
            return;
        }
        // The last two items are the separator and Quit.
        let position = self.menu.items().len().saturating_sub(2);
        if let Err(e) = self.menu.insert(&self.update_item, position) {
            tracing::warn!("couldn't add the update item to the tray menu ({e})");
        }
    }
}

/// Whether handling `event` may have run a modal loop (a message box, or
/// the icon's own menu), during which Windows drops the wake-ups posted to
/// this thread. A left click may show an error box; a right click's event
/// arrives just before tray-icon shows the menu, and is read only after
/// the menu has closed again.
fn may_have_run_modal(event: &TrayIconEvent) -> bool {
    matches!(
        event,
        TrayIconEvent::Click {
            button: MouseButton::Left | MouseButton::Right,
            button_state: MouseButtonState::Up,
            ..
        }
    )
}

/// "Restart to update": asks first, then closes any open settings window
/// and stops the server, which then installs the update and starts again.
fn restart_to_update(ctx: &TrayContext) {
    let status = (ctx.snapshot)();
    // The update may have gone while the menu was open.
    let Some(version) = ready_update(&status) else {
        return;
    };
    if dialog::confirm(&restart_prompt(version)) {
        tracing::info!("Restart to update chosen from the tray icon's menu");
        // The window runs from the folder the update replaces.
        shell::close_window(ui::TITLE);
        ctx.stop.request(StopReason::RestartToUpdate);
    }
}

/// Whether start with Windows is on.
fn autostart_on() -> io::Result<bool> {
    autostart::is_enabled(&RunEntry::user())
}

/// Opens the settings window (or, if one is open, the window brings itself
/// to the front).
fn open_window(port: u16) {
    if let Err(e) = handoff::spawn_window(port, false) {
        tracing::warn!("couldn't open the settings window ({e})");
        dialog::error(&format!(
            "The SLC Local TTS window couldn't open ({e}). Try again in a moment."
        ));
    }
}

/// Whether the tray must look at the status again after a wait: when the
/// wait timed out, or whenever the failure deadline it waited for has
/// passed. Another message can wake the thread just after the deadline and
/// before the timeout fires; without the second check the amber dot would
/// then stay until the status next changed.
fn needs_refresh(timed_out: bool, expiry: Option<SystemTime>, now: SystemTime) -> bool {
    timed_out || expiry.is_some_and(|deadline| now >= deadline)
}

/// How long to sleep, in milliseconds, before `expiry` passes:
/// `INFINITE` without one. Rounded up, and one more, so the wake comes just
/// after the failure has left the hour rather than just before.
fn wait_ms(expiry: Option<SystemTime>, now: SystemTime) -> u32 {
    let Some(expiry) = expiry else {
        return INFINITE;
    };
    let left = expiry.duration_since(now).unwrap_or(Duration::ZERO);
    let ms = left.as_micros().div_ceil(1000) + 1;
    // Never INFINITE itself (u32::MAX) by accident.
    u32::try_from(ms).unwrap_or(INFINITE - 1).min(INFINITE - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handle to a thread that has already ended.
    fn ended_handle() -> TrayHandle {
        let (tx, rx) = mpsc::channel();
        // SAFETY: GetCurrentThreadId has no preconditions.
        let thread = std::thread::spawn(move || tx.send(unsafe { GetCurrentThreadId() }).unwrap());
        let thread_id = rx.recv().unwrap();
        while !thread.is_finished() {
            std::thread::sleep(Duration::from_millis(5));
        }
        TrayHandle {
            thread_id,
            thread: Arc::new(Mutex::new(Some(thread))),
        }
    }

    #[test]
    fn wake_after_thread_ended_is_harmless() {
        let tray = ended_handle();
        tray.wake();
        tray.clone().shutdown();
        // After a shutdown, other clones still do nothing harmful.
        tray.wake();
        tray.shutdown();
    }

    #[tokio::test]
    async fn forward_changes_wakes_and_ends_when_both_close() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Lets the forwarding task run until it waits again.
        async fn settle() {
            for _ in 0..10 {
                tokio::task::yield_now().await;
            }
        }

        let (changes_tx, changes) = watch::channel(0_u64);
        let (updates_tx, updates) = watch::channel(UpdateState::UpToDate);
        // Counts the wakes. In an `Arc`, because the task and the test
        // both hold it.
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let forward = tokio::spawn(forward_changes_to(changes, updates, move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        let count = || wakes.load(Ordering::SeqCst);

        settle().await;
        assert_eq!(count(), 0, "no change, no wake");
        changes_tx.send(1).unwrap();
        settle().await;
        assert_eq!(count(), 1, "a status change wakes");
        updates_tx.send(UpdateState::Off).unwrap();
        settle().await;
        assert_eq!(count(), 2, "an update change wakes");

        // The update check drops its sender when it's done; the tray must
        // keep watching the status, and not take the closing for a change.
        drop(updates_tx);
        settle().await;
        assert_eq!(count(), 2, "a closed channel isn't a change");
        changes_tx.send(2).unwrap();
        settle().await;
        assert_eq!(count(), 3);
        assert!(!forward.is_finished());

        drop(changes_tx);
        tokio::time::timeout(Duration::from_secs(5), forward)
            .await
            .expect("forward_changes ends when both senders are gone")
            .unwrap();
    }

    #[test]
    fn refresh_when_the_deadline_passed_whatever_woke_the_tray() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let passed = Some(now - Duration::from_millis(1));
        let ahead = Some(now + Duration::from_secs(60));
        // Woken by some other message after the deadline.
        assert!(needs_refresh(false, passed, now));
        assert!(needs_refresh(false, Some(now), now), "exactly at it");
        // Woken by another message before it: nothing to do.
        assert!(!needs_refresh(false, ahead, now));
        assert!(!needs_refresh(false, None, now));
        // The wait ran out.
        assert!(needs_refresh(true, ahead, now));
        assert!(needs_refresh(true, None, now));
    }

    #[test]
    fn clicks_count_as_modal_but_hovering_does_not() {
        use tray_icon::dpi::PhysicalPosition;
        use tray_icon::{Rect, TrayIconId};

        let click = |button, button_state| TrayIconEvent::Click {
            id: TrayIconId::new("test"),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect::default(),
            button,
            button_state,
        };
        // Left up opens the window, which may show an error box.
        assert!(may_have_run_modal(&click(
            MouseButton::Left,
            MouseButtonState::Up
        )));
        // Right up comes just before the menu, which runs its own loop.
        assert!(may_have_run_modal(&click(
            MouseButton::Right,
            MouseButtonState::Up
        )));
        // Presses, the middle button and hovering run nothing modal.
        assert!(!may_have_run_modal(&click(
            MouseButton::Left,
            MouseButtonState::Down
        )));
        assert!(!may_have_run_modal(&click(
            MouseButton::Right,
            MouseButtonState::Down
        )));
        assert!(!may_have_run_modal(&click(
            MouseButton::Middle,
            MouseButtonState::Up
        )));
        let enter = TrayIconEvent::Enter {
            id: TrayIconId::new("test"),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect::default(),
        };
        assert!(!may_have_run_modal(&enter));
    }

    #[test]
    fn wait_is_infinite_without_a_failure_and_just_after_one_expires() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        assert_eq!(wait_ms(None, now), INFINITE);
        assert_eq!(wait_ms(Some(now + Duration::from_millis(1500)), now), 1501);
        assert_eq!(
            wait_ms(Some(now + Duration::from_micros(1)), now),
            2,
            "rounded up"
        );
        // Already passed: wake almost at once, never "never".
        assert_eq!(wait_ms(Some(now - Duration::from_secs(5)), now), 1);
    }
}
