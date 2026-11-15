//! Glue to the Windows desktop: message boxes, the clipboard, opening
//! folders, files and web pages, and the hand-off from a second launch to
//! the copy that's already running. Both programs write the hand-off's port
//! file while they serve.

pub mod clipboard;
pub mod dialog;
pub mod handoff;
pub mod shell;

#[cfg(test)]
mod tests {
    use std::ptr;

    use windows_sys::Win32::System::LibraryLoader::{
        FindResourceW, GetModuleHandleW, LoadResource, LockResource, SizeofResource,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::RT_MANIFEST;

    /// The manifest embedded in this test binary: resource 1 of type
    /// `RT_MANIFEST`, the one Windows reads when it starts a program.
    fn own_manifest() -> String {
        // `MAKEINTRESOURCE(1)`: a resource ID passed where a name pointer
        // goes, as Windows' C headers do it.
        let id = ptr::without_provenance::<u16>(1);
        // SAFETY: a null name gives this process's own exe, which stays
        // loaded. The resource handles belong to that module and are never
        // freed, and `SizeofResource` gives the exact length of the bytes
        // `LockResource` points to.
        unsafe {
            let module = GetModuleHandleW(ptr::null());
            assert!(!module.is_null());
            let info = FindResourceW(module, id, RT_MANIFEST);
            assert!(!info.is_null(), "no manifest resource 1");
            let data = LoadResource(module, info);
            assert!(!data.is_null());
            let size = SizeofResource(module, info) as usize;
            let bytes = std::slice::from_raw_parts(LockResource(data).cast::<u8>(), size);
            String::from_utf8_lossy(bytes).into_owned()
        }
    }

    #[test]
    fn embedded_manifest_has_ui_entries() {
        let manifest = own_manifest();
        for entry in [
            "Microsoft.Windows.Common-Controls",
            "PerMonitorV2",
            "asInvoker",
            "UTF-8",
        ] {
            assert!(
                manifest.contains(entry),
                "{entry} missing from:\n{manifest}"
            );
        }
        assert_eq!(
            manifest.matches("asInvoker").count(),
            1,
            "one execution level only:\n{manifest}"
        );
    }
}
