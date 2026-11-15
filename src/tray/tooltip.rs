//! Changing the tooltip of an icon registered with a GUID.
//!
//! An installed copy registers its tray icon with a fixed GUID, and Windows
//! then knows the icon only by that GUID. tray-icon's own `set_tooltip`
//! names the icon by its window and number instead, so Windows can't find
//! it and every change fails. This sends the change itself, naming the icon
//! by its GUID.
//!
//! One gap remains: if Explorer restarts, tray-icon adds the icon back with
//! the tooltip it was built with, until the tooltip next changes.

use std::io;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Shell::{
    NIF_GUID, NIF_TIP, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::core::GUID;

/// The request that sets the tooltip of the icon `guid`, owned by the
/// window `hwnd`, to `text`. Text past the tooltip's 127 characters is cut,
/// so the last one is always the terminating NUL.
pub fn guid_tooltip_data(hwnd: HWND, guid: u128, text: &str) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uFlags: NIF_TIP | NIF_GUID,
        guidItem: GUID::from_u128(guid),
        // Every other field zero, as Windows expects for the ones the
        // flags don't name.
        ..NOTIFYICONDATAW::default()
    };
    let room = data.szTip.len() - 1;
    for (slot, unit) in data.szTip.iter_mut().zip(text.encode_utf16().take(room)) {
        *slot = unit;
    }
    data
}

/// Sets the tooltip of the icon `guid`, owned by the window `hwnd`.
pub fn set_guid_tooltip(hwnd: HWND, guid: u128, text: &str) -> io::Result<()> {
    let data = guid_tooltip_data(hwnd, guid, text);
    // SAFETY: `data` is a fully initialised NOTIFYICONDATAW with `cbSize`
    // set, and lives until the call returns; Windows only reads it.
    if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUID_VALUE: u128 = 0x0123_4567_89ab_cdef_0011_2233_4455_6677;

    fn guid_bytes(guid: &GUID) -> (u32, u16, u16, [u8; 8]) {
        (guid.data1, guid.data2, guid.data3, guid.data4)
    }

    #[test]
    fn names_the_icon_by_its_guid() {
        let data = guid_tooltip_data(std::ptr::null_mut(), GUID_VALUE, "SLC Local TTS: ready");
        assert_eq!(data.cbSize as usize, size_of::<NOTIFYICONDATAW>());
        assert_eq!(data.uFlags & NIF_TIP, NIF_TIP);
        assert_eq!(data.uFlags & NIF_GUID, NIF_GUID);
        assert_eq!(
            guid_bytes(&data.guidItem),
            guid_bytes(&GUID::from_u128(GUID_VALUE))
        );
        let expected: Vec<u16> = "SLC Local TTS: ready".encode_utf16().collect();
        assert_eq!(&data.szTip[..expected.len()], &expected[..]);
        assert_eq!(data.szTip[expected.len()], 0, "ends with a NUL");
    }

    #[test]
    fn a_full_length_tip_keeps_its_nul() {
        let text = "x".repeat(127);
        let data = guid_tooltip_data(std::ptr::null_mut(), GUID_VALUE, &text);
        assert!(
            data.szTip[..127]
                .iter()
                .all(|&unit| unit == u16::from(b'x'))
        );
        assert_eq!(data.szTip[127], 0);

        // Anything longer is cut rather than overwriting the NUL.
        let data = guid_tooltip_data(std::ptr::null_mut(), GUID_VALUE, &"y".repeat(200));
        assert_eq!(data.szTip[127], 0);
    }

    #[test]
    fn an_empty_tip_is_all_zero() {
        let data = guid_tooltip_data(std::ptr::null_mut(), GUID_VALUE, "");
        assert!(data.szTip.iter().all(|&unit| unit == 0));
    }
}
