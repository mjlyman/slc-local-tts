//! Opening things the way Explorer would (folders, files, web pages),
//! starting the server, and finding the settings window by its class and
//! title.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::process::Command;
use std::ptr;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, IsIconic, PostMessageW, SW_RESTORE, SW_SHOWNORMAL, SetForegroundWindow,
    ShowWindow, WM_CLOSE,
};

use super::dialog::to_wide;

/// The window class of the settings window. eframe doesn't name its
/// windows' class, so this is winit's default (`"Window Class"`, set in
/// winit 0.30's Windows platform code, which eframe 0.37 uses). Other
/// windows carry the app's title too (the CLI's console, message boxes),
/// so the search matches the class as well. Check it again after any
/// eframe upgrade.
pub const WINDOW_CLASS: &str = "Window Class";

/// What [`bring_to_front`] managed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Raise {
    /// No settings window with that title is open.
    NotFound,
    Raised,
    /// The window is open, but Windows kept another window in front of it.
    KeptBehind,
}

/// Opens `path` with its usual program: a folder in Explorer, a file with
/// the app Windows has for its type.
pub fn open(path: &Path) -> io::Result<()> {
    shell_open(&wide_os(path.as_os_str()))
}

/// Opens the text file `path` with its usual program, or with Notepad when
/// Windows has none for its type.
pub fn open_text(path: &Path) -> io::Result<()> {
    if open(path).is_ok() {
        return Ok(());
    }
    // `Command` puts quotes around an argument with spaces itself.
    Command::new("notepad.exe").arg(path).spawn()?;
    Ok(())
}

/// Opens `url` in the user's web browser.
pub fn open_url(url: &str) -> io::Result<()> {
    shell_open(&to_wide(url))
}

/// Starts SLC Local TTS (this same exe, with no arguments), and doesn't
/// wait for it.
pub fn start_server() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    // Dropping the returned `Child` neither waits for it nor ends it.
    Command::new(exe).spawn()?;
    Ok(())
}

/// Brings the settings window titled `title` to the front, restoring it
/// first if it's minimized.
///
/// Windows lets a program take the foreground only in some cases (for
/// example, when it was just started by the user), so it can refuse.
pub fn bring_to_front(title: &str) -> Raise {
    let Some(window) = find_window(title) else {
        return Raise::NotFound;
    };
    // SAFETY: plain Win32 calls on a window handle; a handle that has
    // closed in the meantime only makes them fail.
    let raised = unsafe {
        if IsIconic(window) != 0 {
            ShowWindow(window, SW_RESTORE);
        }
        SetForegroundWindow(window) != 0
    };
    if raised {
        Raise::Raised
    } else {
        Raise::KeptBehind
    }
}

/// Asks the settings window titled `title` to close, as its close button
/// would. Doesn't wait. False if there's no such window.
pub fn close_window(title: &str) -> bool {
    let Some(window) = find_window(title) else {
        return false;
    };
    // SAFETY: `PostMessageW` only queues the message; a stale handle makes
    // it fail.
    unsafe { PostMessageW(window, WM_CLOSE, 0, 0) != 0 }
}

/// How often [`close_window_and_wait`] looks for the window again.
const CLOSE_POLL: Duration = Duration::from_millis(50);

/// Asks the settings window titled `title` to close, then waits up to
/// `limit` for it to go. True once no such window is open, at once if
/// there was none.
///
/// Installing an update needs this. The window is a process of its own,
/// started from the same `current\` folder the update replaces, so it
/// should be gone before the updater starts on those files.
pub fn close_window_and_wait(title: &str, limit: Duration) -> bool {
    if !close_window(title) {
        // No window (or one that just closed): nothing to wait for.
        return find_window(title).is_none();
    }
    let deadline = Instant::now() + limit;
    loop {
        if find_window(title).is_none() {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        std::thread::sleep(CLOSE_POLL.min(deadline - now));
    }
}

/// The top-level window of class [`WINDOW_CLASS`] titled `title`.
fn find_window(title: &str) -> Option<HWND> {
    let class = to_wide(WINDOW_CLASS);
    let title = to_wide(title);
    // SAFETY: both strings are NUL-terminated UTF-16 that outlive the call.
    let window = unsafe { FindWindowW(class.as_ptr(), title.as_ptr()) };
    (!window.is_null()).then_some(window)
}

/// `ShellExecuteW`'s "open" on `target` (NUL-terminated UTF-16). The
/// calling thread should have COM set up (see `with_com`), as Windows asks
/// of every `ShellExecuteW` caller.
fn shell_open(target: &[u16]) -> io::Result<()> {
    let verb = to_wide("open");
    // SAFETY: `verb` and `target` are NUL-terminated UTF-16 that live
    // until the call returns; null means no owner window, no parameters and
    // the default working folder.
    let result = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // The result isn't a real handle: above 32 means it worked, and 32 or
    // below is an error code.
    let code = result as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(shell_error(code))
    }
}

/// `ShellExecuteW`'s error codes, in words. Some match Windows' own error
/// numbers; the rest are its own.
fn shell_error(code: isize) -> io::Error {
    // SE_ERR_ASSOCINCOMPLETE (27) and SE_ERR_NOASSOC (31).
    const NO_PROGRAM: [isize; 2] = [27, 31];
    // SE_ERR_SHARE, SE_ERR_DDETIMEOUT, SE_ERR_DDEFAIL, SE_ERR_DDEBUSY and
    // SE_ERR_DLLNOTFOUND (26, 28, 29, 30, 32).
    const OWN_CODES: [isize; 5] = [26, 28, 29, 30, 32];
    if NO_PROGRAM.contains(&code) {
        io::Error::other("Windows has no program set to open it")
    } else if OWN_CODES.contains(&code) || code <= 0 {
        io::Error::other(format!("Windows couldn't open it (error {code})"))
    } else {
        // 2 (file not found), 3 (path not found), 5 (access denied), 8 (out
        // of memory) and 11 (bad format) are Windows' own error numbers.
        io::Error::from_raw_os_error(code as i32)
    }
}

/// Runs `body` with COM set up on this thread, single-threaded, as
/// `ShellExecuteW` needs: some file types are opened through COM handlers,
/// which fail or misbehave on a thread without it. Undone again after
/// `body` if it was set up here.
pub fn with_com<R>(body: impl FnOnce() -> R) -> R {
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };
    // SAFETY: a null reserved pointer is required; the flags are valid.
    let result = unsafe {
        CoInitializeEx(
            ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    // S_OK and S_FALSE (already set up) both need a matching uninitialize;
    // a failure (another threading model already chosen) doesn't.
    let initialized = result >= 0;
    if !initialized {
        tracing::debug!("COM couldn't be set up on this thread (0x{result:08X})");
    }
    let output = body();
    if initialized {
        // SAFETY: matches the successful `CoInitializeEx` above, on the
        // same thread.
        unsafe { CoUninitialize() };
    }
    output
}

/// `s` as NUL-terminated UTF-16, kept exactly, even if it isn't valid
/// Unicode (a Windows path may not be).
fn wide_os(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_errors_read_well() {
        assert_eq!(
            shell_error(31).to_string(),
            "Windows has no program set to open it"
        );
        assert_eq!(shell_error(2).raw_os_error(), Some(2));
        assert_eq!(
            shell_error(0).to_string(),
            "Windows couldn't open it (error 0)"
        );
    }

    #[test]
    fn wide_path_keeps_unicode() {
        let path = Path::new(r"C:\Users\José Ü\@x");
        let wide = wide_os(path.as_os_str());
        assert_eq!(wide.last(), Some(&0));
        assert_eq!(
            String::from_utf16(&wide[..wide.len() - 1]).unwrap(),
            r"C:\Users\José Ü\@x"
        );
    }

    /// A hidden top-level window of class `class` titled `title`, as a
    /// console or message box with the app's title would be.
    struct OtherWindow(HWND);

    impl OtherWindow {
        fn new(class: &str, title: &str) -> OtherWindow {
            use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, RegisterClassW, WNDCLASSW, WS_OVERLAPPED,
            };
            let class = to_wide(class);
            let title = to_wide(title);
            // SAFETY: both strings are NUL-terminated and outlive the calls,
            // and `DefWindowProcW` is a valid window procedure.
            let hwnd = unsafe {
                let instance = GetModuleHandleW(ptr::null());
                let info = WNDCLASSW {
                    lpfnWndProc: Some(DefWindowProcW),
                    hInstance: instance,
                    lpszClassName: class.as_ptr(),
                    // SAFETY: every other field may be zero.
                    ..std::mem::zeroed()
                };
                RegisterClassW(&info);
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
            assert!(!hwnd.is_null());
            OtherWindow(hwnd)
        }
    }

    impl Drop for OtherWindow {
        fn drop(&mut self) {
            use windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow;
            // SAFETY: the window was made on this thread and is destroyed once.
            unsafe { DestroyWindow(self.0) };
        }
    }

    #[test]
    fn only_the_settings_window_class_is_found() {
        let title = format!("SLC Local TTS shell test {}", std::process::id());
        let _console = OtherWindow::new("SLCLocalTTS.ShellTestConsole", &title);
        assert_eq!(find_window(&title), None);
        assert_eq!(bring_to_front(&title), Raise::NotFound);
        assert!(!close_window(&title));

        let window = OtherWindow::new(WINDOW_CLASS, &title);
        assert_eq!(find_window(&title), Some(window.0));
    }

    #[test]
    fn close_and_wait_returns_at_once_without_a_window() {
        let started = std::time::Instant::now();
        assert!(close_window_and_wait(
            "SLC Local TTS test window that doesn't exist",
            Duration::from_secs(2)
        ));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn close_and_wait_gives_up_on_a_window_that_stays() {
        let title = format!("SLC Local TTS close test {}", std::process::id());
        // This thread never reads its messages, so the window ignores the
        // close it's sent.
        let _window = OtherWindow::new(WINDOW_CLASS, &title);
        let started = std::time::Instant::now();
        assert!(!close_window_and_wait(&title, Duration::from_millis(200)));
        assert!(started.elapsed() >= Duration::from_millis(200));
    }

    #[test]
    fn missing_windows_are_not_found() {
        let title = "SLC Local TTS test window that doesn't exist";
        assert_eq!(bring_to_front(title), Raise::NotFound);
        assert!(!close_window(title));
    }
}
