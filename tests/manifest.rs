//! Both exes carry exactly one application manifest, with every entry the
//! program relies on, and the tray program carries its icon. The unit tests
//! prove Windows honours the manifest (their code page is UTF-8); this
//! proves the real exes have it, since the icon and version step in
//! build.rs also writes resources into them.

use std::path::Path;
use std::ptr;

use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{
    EnumResourceNamesW, FindResourceW, LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE,
    LoadLibraryExW, LoadResource, LockResource, SizeofResource,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{RT_GROUP_ICON, RT_MANIFEST};
use windows_sys::core::{BOOL, PCWSTR};

/// An exe opened for reading its resources only: none of its code runs.
/// Dropping it closes it.
struct Resources(HMODULE);

impl Resources {
    fn open(exe: &Path) -> Resources {
        let wide: Vec<u16> = exe
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // SAFETY: `wide` is NUL-terminated and outlives the call; these
        // flags map the file as data, so nothing in it is run.
        let module = unsafe {
            LoadLibraryExW(
                wide.as_ptr(),
                ptr::null_mut(),
                LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE,
            )
        };
        assert!(!module.is_null(), "couldn't open {}", exe.display());
        Resources(module)
    }

    /// The names of every resource of type `kind`. Each is an ID or a
    /// string; IDs are kept as `Ok`, strings as `Err` (they don't occur in
    /// our exes, but must still count).
    fn names(&self, kind: PCWSTR) -> Vec<Result<usize, String>> {
        unsafe extern "system" fn collect(
            _module: HMODULE,
            _kind: PCWSTR,
            name: PCWSTR,
            found: isize,
        ) -> BOOL {
            // SAFETY: `found` is the `&mut Vec` passed below, alive for the
            // whole enumeration. A name below 0x10000 is an ID, not a
            // pointer (Windows' `IS_INTRESOURCE`); otherwise it points to a
            // NUL-terminated string valid during this call.
            unsafe {
                let found = &mut *(found as *mut Vec<Result<usize, String>>);
                let raw = name as usize;
                if raw < 0x10000 {
                    found.push(Ok(raw));
                } else {
                    let len = (0..).take_while(|&i| *name.add(i) != 0).count();
                    let text = String::from_utf16_lossy(std::slice::from_raw_parts(name, len));
                    found.push(Err(text));
                }
            }
            1
        }
        let mut found: Vec<Result<usize, String>> = Vec::new();
        // SAFETY: the callback only touches `found`, which outlives the
        // call. It returns FALSE when the type isn't there at all, which
        // here just means "none".
        unsafe {
            EnumResourceNamesW(
                self.0,
                kind,
                Some(collect),
                &mut found as *mut Vec<Result<usize, String>> as isize,
            );
        }
        found
    }

    /// Resource `id` of type `kind`, as text.
    fn text(&self, kind: PCWSTR, id: usize) -> String {
        // SAFETY: the handles belong to `self.0`, which stays open while
        // `self` lives, and `SizeofResource` gives the exact length of the
        // bytes `LockResource` points to.
        unsafe {
            let info = FindResourceW(self.0, ptr::without_provenance(id), kind);
            assert!(!info.is_null());
            let data = LoadResource(self.0, info);
            assert!(!data.is_null());
            let size = SizeofResource(self.0, info) as usize;
            let bytes = std::slice::from_raw_parts(LockResource(data).cast::<u8>(), size);
            String::from_utf8_lossy(bytes).into_owned()
        }
    }
}

impl Drop for Resources {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from LoadLibraryExW and is closed once.
        unsafe {
            FreeLibrary(self.0);
        }
    }
}

fn check_manifest(exe: &Path) -> Resources {
    let resources = Resources::open(exe);
    let names = resources.names(RT_MANIFEST);
    assert_eq!(names.len(), 1, "{}: manifests {names:?}", exe.display());
    let id = *names[0].as_ref().expect("the manifest has a numeric ID");
    let manifest = resources.text(RT_MANIFEST, id);
    for entry in [
        "Microsoft.Windows.Common-Controls",
        "PerMonitorV2",
        "asInvoker",
        "UTF-8",
    ] {
        assert!(
            manifest.contains(entry),
            "{}: {entry} missing from:\n{manifest}",
            exe.display()
        );
    }
    assert_eq!(manifest.matches("asInvoker").count(), 1, "{manifest}");
    resources
}

#[test]
fn bins_carry_one_manifest() {
    let gui = check_manifest(Path::new(env!("CARGO_BIN_EXE_slc-local-tts")));
    assert!(
        !gui.names(RT_GROUP_ICON).is_empty(),
        "slc-local-tts.exe has no icon"
    );
    check_manifest(Path::new(env!("CARGO_BIN_EXE_slc-local-tts-cli")));
}
