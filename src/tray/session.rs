//! Signing out and shutting down. A program without a console gets no
//! console events, so Windows tells it the session is ending only through
//! messages to its top-level windows. The tray thread owns this hidden one
//! for that: it lets Windows end the session, then stops the server cleanly
//! and writes out the log before the process is ended. It also tells the
//! tray when the computer wakes from sleep.

use std::cell::RefCell;
use std::io;
use std::ptr;
use std::time::Duration;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, PBT_APMRESUMEAUTOMATIC, PostThreadMessageW,
    RegisterClassW, WM_ENDSESSION, WM_POWERBROADCAST, WM_QUERYENDSESSION, WNDCLASSW, WS_OVERLAPPED,
};

use super::WM_WAKE;
use crate::desktop::dialog::to_wide;
use crate::logging;
use crate::stop::{ServedFlag, StopReason, StopSignal};

/// The hidden window's class and title.
const CLASS_NAME: &str = "SLCLocalTTS.Session";
const TITLE: &str = "SLC Local TTS session";

/// How long the end of a session waits for the server to stop before it
/// writes out the log anyway. Windows ends the process soon after.
pub const SESSION_END_WAIT: Duration = Duration::from_secs(5);

/// Asks the server to stop because the session is ending, waits up to
/// `wait` for it to finish, then writes out the log.
pub fn end_session(stop: &StopSignal, served: &ServedFlag, wait: Duration) {
    stop.request(StopReason::SessionEnding);
    if !served.wait_timeout(wait) {
        tracing::warn!(
            "Windows is ending the session, and the server didn't stop within {} seconds",
            wait.as_secs()
        );
    }
    logging::flush();
}

/// What the hidden window's message handler needs. It lives in a
/// thread-local slot because Windows calls the handler with only the
/// window and the message; the window belongs to the tray thread, so its
/// handler always runs on that thread and finds this thread's slot.
struct Session {
    stop: StopSignal,
    served: ServedFlag,
}

thread_local! {
    // `RefCell` lets the handler read the slot, and `SessionWindow` fill
    // and empty it, with the borrow checked at run time instead of at
    // compile time.
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// The hidden window. Dropping it destroys the window.
pub struct SessionWindow {
    hwnd: HWND,
}

impl SessionWindow {
    /// Makes the hidden window on the calling thread, which must keep
    /// reading its messages.
    pub fn create(stop: StopSignal, served: ServedFlag) -> io::Result<SessionWindow> {
        SESSION.with(|slot| *slot.borrow_mut() = Some(Session { stop, served }));
        let class = to_wide(CLASS_NAME);
        let title = to_wide(TITLE);
        // SAFETY: the class and title strings are NUL-terminated and live
        // until the calls return (Windows copies them). `handle_message`
        // has the signature Windows expects of a window procedure.
        let hwnd = unsafe {
            let instance = GetModuleHandleW(ptr::null());
            let class_info = WNDCLASSW {
                lpfnWndProc: Some(handle_message),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                // SAFETY: every other field of this C struct may be zero.
                ..std::mem::zeroed()
            };
            // Registering fails if the class already exists (a second
            // tray thread in tests, say); creating the window still works.
            RegisterClassW(&class_info);
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                instance,
                ptr::null(),
            )
        };
        if hwnd.is_null() {
            let error = io::Error::last_os_error();
            SESSION.with(|slot| slot.borrow_mut().take());
            return Err(error);
        }
        // Never shown: a top-level window gets the end-of-session messages
        // whether or not it's visible.
        Ok(SessionWindow { hwnd })
    }

    /// The window's handle, for calls that want a window of this thread
    /// (such as owning the clipboard).
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

impl Drop for SessionWindow {
    fn drop(&mut self) {
        // SAFETY: the window was made on this thread and is destroyed once.
        unsafe {
            DestroyWindow(self.hwnd);
        }
        SESSION.with(|slot| slot.borrow_mut().take());
    }
}

/// The hidden window's message handler (its "window procedure").
unsafe extern "system" fn handle_message(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        // "May the session end?" Always yes: nothing here needs saving.
        WM_QUERYENDSESSION => 1,
        // The session really is ending (wParam is FALSE when it was
        // cancelled after all). Windows may end the process as soon as
        // this returns, so the server is stopped and the log written first.
        WM_ENDSESSION => {
            if wparam != 0 {
                // Clones, so no borrow of the slot is held during the wait.
                let session = SESSION.with(|slot| {
                    slot.borrow()
                        .as_ref()
                        .map(|s| (s.stop.clone(), s.served.clone()))
                });
                if let Some((stop, served)) = session {
                    end_session(&stop, &served, SESSION_END_WAIT);
                }
            }
            0
        }
        // The computer woke from sleep. The tray's timed wait doesn't count
        // time asleep, so a failure that left the last hour meanwhile would
        // keep its amber dot; wake the tray's loop so it looks again.
        WM_POWERBROADCAST => {
            if wparam == PBT_APMRESUMEAUTOMATIC as WPARAM {
                // SAFETY: plain Win32 calls with no pointers. This handler
                // runs on the tray thread, so the message goes to its loop.
                unsafe {
                    PostThreadMessageW(GetCurrentThreadId(), WM_WAKE, 0, 0);
                }
            }
            1
        }
        // SAFETY: the arguments are the ones Windows passed in.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn end_session_requests_stop_and_waits() {
        let stop = StopSignal::new();
        let served = ServedFlag::default();
        let setter = served.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            setter.set();
        });

        let started = Instant::now();
        end_session(&stop, &served, SESSION_END_WAIT);
        let took = started.elapsed();
        thread.join().unwrap();

        assert_eq!(stop.requested(), Some(StopReason::SessionEnding));
        assert!(took >= Duration::from_millis(90), "{took:?}");
        assert!(took < Duration::from_secs(3), "{took:?}");
    }

    #[test]
    fn end_session_gives_up_after_the_wait() {
        let stop = StopSignal::new();
        let started = Instant::now();
        end_session(&stop, &ServedFlag::default(), Duration::from_millis(50));
        assert!(started.elapsed() >= Duration::from_millis(40));
        assert_eq!(stop.requested(), Some(StopReason::SessionEnding));
    }
}
