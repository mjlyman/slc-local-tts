//! The Windows console window: who owns it, QuickEdit, title, close event.
//!
//! Every function here is best effort: if a console call fails (for example
//! when output is redirected), the program carries on without it.

use std::io::{self, BufRead, IsTerminal, Write};

use windows_sys::Win32::Foundation::{FALSE, INVALID_HANDLE_VALUE, TRUE};
use windows_sys::Win32::System::Console::{
    ENABLE_EXTENDED_FLAGS, ENABLE_QUICK_EDIT_MODE, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    GetConsoleMode, GetConsoleProcessList, GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    SetConsoleCtrlHandler, SetConsoleMode, SetConsoleTitleW,
};
use windows_sys::core::BOOL;

/// True when this process is the only one attached to its console, which
/// means Windows opened the window for us (a Start Menu or Explorer launch)
/// and it closes when we exit. False inside a terminal, where the shell is
/// attached too.
pub fn owns_console() -> bool {
    let mut processes = [0u32; 2];
    // SAFETY: the pointer and length describe `processes`, which outlives
    // the call. The return value is the number of attached processes (0 on
    // failure, for example with no console at all).
    let count = unsafe { GetConsoleProcessList(processes.as_mut_ptr(), processes.len() as u32) };
    count == 1
}

/// Turns off QuickEdit mode. With it on, one stray click in the window
/// selects text and pauses all console output, and with it the server's
/// logging thread, until a key is pressed.
pub fn disable_quick_edit() {
    // SAFETY: plain Win32 calls on this process's own standard input handle;
    // `mode` is a valid place for GetConsoleMode to write to.
    unsafe {
        let input = GetStdHandle(STD_INPUT_HANDLE);
        if input.is_null() || input == INVALID_HANDLE_VALUE {
            return;
        }
        let mut mode = 0;
        if GetConsoleMode(input, &mut mode) == FALSE {
            return;
        }
        // ENABLE_EXTENDED_FLAGS must be set for the QuickEdit bit to count.
        SetConsoleMode(
            input,
            (mode & !ENABLE_QUICK_EDIT_MODE) | ENABLE_EXTENDED_FLAGS,
        );
    }
}

/// Sets the console window's title.
pub fn set_title(title: &str) {
    // Windows wants UTF-16 with a terminating zero.
    let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is zero-terminated and lives until the call returns.
    unsafe {
        SetConsoleTitleW(wide.as_ptr());
    }
}

/// Flushes the log file before a console event ends the process: the window
/// closing, Windows logging off or shutting down, or a Ctrl+C that tokio
/// doesn't take.
pub fn install_close_handler() {
    // SAFETY: `on_console_event` has the signature Windows expects and is a
    // plain function, so it stays valid for the life of the process.
    unsafe {
        SetConsoleCtrlHandler(Some(on_console_event), TRUE);
    }
}

/// Called by Windows on a thread of its own when a console event arrives.
///
/// Windows tries handlers newest first. Tokio registers its own handler
/// later than this one, so it sees every event first: while a `ctrl_c()`
/// listener is alive it takes Ctrl+C (returns TRUE) and we never hear of it.
/// Whatever tokio declines reaches us: Ctrl+Break (nothing listens for it), a
/// close, logoff or shutdown, and Ctrl+C before the listener starts or after
/// it has fired once (the "press again to quit now" case). Returning FALSE
/// hands the event on to Windows' default handler, which ends the process,
/// and Rust's normal cleanup never runs. So every event that gets here is
/// logged and the log is flushed first.
unsafe extern "system" fn on_console_event(_event: u32) -> BOOL {
    tracing::info!("stopping now; flushing the log");
    crate::logging::flush();
    FALSE
}

/// Shows "Press Enter to exit." and waits, so a window Windows opened for us
/// doesn't vanish before the user reads why we stopped.
pub fn wait_for_enter() {
    print!("Press Enter to exit.");
    let _ = io::stdout().flush();
    let mut line = String::new();
    let _ = io::stdin().lock().read_line(&mut line);
}

/// Turns on colour codes for the console if it can show them, and says
/// whether the log's console lines should use them.
///
/// A classic console window (conhost) only understands ANSI codes once
/// virtual terminal processing is on; without it, every log line shows raw
/// `ESC[2m` noise. A file or pipe (`> log.txt`) gets no codes at all.
pub fn enable_ansi() -> bool {
    let stdout = io::stdout();
    if !stdout.is_terminal() {
        return false;
    }
    // SAFETY: plain Win32 calls on this process's own standard output
    // handle; `mode` is a valid place for GetConsoleMode to write to.
    unsafe {
        let output = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0;
        let console_mode = (!output.is_null()
            && output != INVALID_HANDLE_VALUE
            && GetConsoleMode(output, &mut mode) != FALSE)
            .then_some(mode);
        ansi_decision(true, console_mode, |new_mode| {
            SetConsoleMode(output, new_mode) != FALSE
        })
    }
}

/// The decision behind [`enable_ansi`], without touching a console.
///
/// - `is_terminal`: stdout is a terminal, not a file or pipe.
/// - `console_mode`: stdout's console mode, or `None` if it has none. That
///   is a terminal such as Git Bash's mintty, which reads the codes itself.
/// - `enable_vt`: sets the given console mode; true if that worked.
pub(crate) fn ansi_decision(
    is_terminal: bool,
    console_mode: Option<u32>,
    enable_vt: impl FnOnce(u32) -> bool,
) -> bool {
    if !is_terminal {
        return false;
    }
    match console_mode {
        None => true,
        Some(mode) if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0 => true,
        Some(mode) => enable_vt(mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const VT: u32 = ENABLE_VIRTUAL_TERMINAL_PROCESSING;

    #[test]
    fn ansi_off_when_not_a_terminal() {
        assert!(!ansi_decision(false, Some(VT), |_| panic!("no set")));
        assert!(!ansi_decision(false, None, |_| panic!("no set")));
    }

    #[test]
    fn ansi_on_when_vt_already_on() {
        assert!(ansi_decision(true, Some(VT | 1), |_| panic!("no set")));
    }

    #[test]
    fn ansi_follows_enabling_vt() {
        let asked = Cell::new(None);
        assert!(ansi_decision(true, Some(1), |mode| {
            asked.set(Some(mode));
            true
        }));
        assert_eq!(asked.get(), Some(1 | VT));
        assert!(!ansi_decision(true, Some(1), |_| false));
    }

    #[test]
    fn ansi_on_for_terminal_without_console_mode() {
        assert!(ansi_decision(true, None, |_| panic!("no set")));
    }
}
