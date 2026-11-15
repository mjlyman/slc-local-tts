//! Putting text on the Windows clipboard.

use std::io;
use std::ptr;
use std::time::Duration;

use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

use super::dialog::to_wide;

/// The clipboard format for UTF-16 text. windows-sys files it under OLE,
/// which this program doesn't otherwise need, so it's written out here.
const CF_UNICODETEXT: u32 = 13;

/// How often, and how far apart, to try opening the clipboard while
/// another program has it open.
const OPEN_TRIES: u32 = 10;
const OPEN_WAIT: Duration = Duration::from_millis(20);

/// Puts `text` on the clipboard, replacing what was there. For a thread
/// with no window of its own; the clipboard then has no owner, which is
/// fine for plain text.
pub fn set_text(text: &str) -> io::Result<()> {
    set_text_owned(text, ptr::null_mut())
}

/// [`set_text`], with `owner` (a window of the calling thread, or null)
/// as the clipboard's owner.
pub fn set_text_owned(text: &str, owner: HWND) -> io::Result<()> {
    // The text is copied into clipboard memory first, so a failure here
    // leaves the user's clipboard as it was.
    let memory = ClipboardMemory::with_text(&wide(text))?;
    let _open = OpenClipboardGuard::open(owner)?;
    // SAFETY: the clipboard is open (`_open`), and `memory` holds a valid
    // movable block that isn't locked.
    unsafe {
        if EmptyClipboard() == 0 {
            return Err(io::Error::last_os_error());
        }
        if SetClipboardData(CF_UNICODETEXT, memory.0 as HANDLE).is_null() {
            return Err(io::Error::last_os_error());
        }
    }
    // The clipboard owns the memory now, so it must not be freed here.
    std::mem::forget(memory);
    Ok(())
}

/// A block of movable global memory, the kind the clipboard takes. Dropping
/// it frees the block, unless it has been handed to the clipboard (then
/// `set_text_owned` forgets it instead).
struct ClipboardMemory(HGLOBAL);

impl ClipboardMemory {
    /// A block holding exactly `units`.
    fn with_text(units: &[u16]) -> io::Result<ClipboardMemory> {
        let bytes = std::mem::size_of_val(units);
        // SAFETY: GlobalAlloc takes no pointers; a null result is an error.
        let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) };
        if memory.is_null() {
            return Err(io::Error::last_os_error());
        }
        // From here on, `block` frees the memory on every early return.
        let block = ClipboardMemory(memory);
        // SAFETY: the block is `bytes` long, and exactly `units.len()` u16s
        // are copied into it while it's locked.
        unsafe {
            let target = GlobalLock(block.0).cast::<u16>();
            if target.is_null() {
                return Err(io::Error::last_os_error());
            }
            ptr::copy_nonoverlapping(units.as_ptr(), target, units.len());
            GlobalUnlock(block.0);
        }
        Ok(block)
    }
}

impl Drop for ClipboardMemory {
    fn drop(&mut self) {
        // SAFETY: the block came from GlobalAlloc and is freed once.
        unsafe {
            GlobalFree(self.0);
        }
    }
}

/// The clipboard's form of `text`: UTF-16 with a closing NUL, which
/// `CF_UNICODETEXT` requires.
fn wide(text: &str) -> Vec<u16> {
    to_wide(text)
}

/// The open clipboard. Dropping it closes the clipboard again, on every
/// path out of `set_text_owned`, as a C++ RAII guard would.
struct OpenClipboardGuard;

impl OpenClipboardGuard {
    fn open(owner: HWND) -> io::Result<OpenClipboardGuard> {
        let mut tries = 0;
        loop {
            // SAFETY: `owner` is a window of this thread or null, and both
            // are allowed.
            if unsafe { OpenClipboard(owner) } != 0 {
                return Ok(OpenClipboardGuard);
            }
            tries += 1;
            if tries == OPEN_TRIES {
                return Err(io::Error::last_os_error());
            }
            std::thread::sleep(OPEN_WAIT);
        }
    }
}

impl Drop for OpenClipboardGuard {
    fn drop(&mut self) {
        // SAFETY: this guard exists only while the clipboard is open.
        unsafe {
            CloseClipboard();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_text_for_clipboard_is_nul_terminated() {
        let units = wide("http://127.0.0.1:8765");
        assert_eq!(units.last(), Some(&0));
        assert_eq!(units.len(), "http://127.0.0.1:8765".len() + 1);
        assert_eq!(
            String::from_utf16(&units[..units.len() - 1]).unwrap(),
            "http://127.0.0.1:8765"
        );
        assert_eq!(wide("Jürgen 🛫"), {
            let mut v: Vec<u16> = "Jürgen 🛫".encode_utf16().collect();
            v.push(0);
            v
        });
    }
}
