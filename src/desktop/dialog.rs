//! Windows message boxes, for a program with no console to print to.

use std::ptr;

use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_YESNO,
    MESSAGEBOX_STYLE, MessageBoxW,
};

/// Every box's title.
const TITLE: &str = "SLC Local TTS";

/// Shows `text` in an error box with an OK button, and waits until it's
/// closed.
pub fn error(text: &str) {
    show(text, MB_OK | MB_ICONERROR | MB_SETFOREGROUND);
}

/// Asks a yes/no question with a warning icon, and waits for the answer.
/// No is the default button, so a stray Enter never confirms. True only
/// for Yes.
pub fn confirm(text: &str) -> bool {
    show(
        text,
        MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2 | MB_SETFOREGROUND,
    ) == IDYES
}

/// `MessageBoxW` with no owner window. Returns the button pressed.
fn show(text: &str, style: MESSAGEBOX_STYLE) -> i32 {
    let text = to_wide(text);
    let title = to_wide(TITLE);
    // SAFETY: both strings are NUL-terminated UTF-16 that live until the
    // call returns; a null owner window is allowed.
    unsafe { MessageBoxW(ptr::null_mut(), text.as_ptr(), title.as_ptr(), style) }
}

/// `s` as UTF-16 with a terminating NUL, the form Windows' `W` functions
/// take.
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_wide_is_nul_terminated_utf16() {
        assert_eq!(to_wide("Ü a"), vec![0xDC, 0x20, 0x61, 0]);
        assert_eq!(to_wide(""), vec![0]);
    }
}
