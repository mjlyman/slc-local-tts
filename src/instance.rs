//! One server at a time.
//!
//! `serve` takes a named Windows mutex, [`MUTEX_NAME`], before it binds the
//! port or cleans anything up, and a second `serve` that finds it taken
//! stops with [`AlreadyRunning`]. `voices` and `download` never take it, so
//! they work while a server runs.
//!
//! Nobody ever waits on (locks) the mutex: its existence is the signal. The
//! first copy creates it and keeps a handle open; a later copy's
//! `CreateMutexW` opens the same object and Windows reports
//! `ERROR_ALREADY_EXISTS`. Windows deletes the mutex when its last handle
//! closes, which also happens when a process crashes, so a crash can never
//! leave a stale "already running" behind.

use std::iter;
use std::ptr;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, FALSE, GetLastError, HANDLE,
};
use windows_sys::Win32::System::Threading::CreateMutexW;

/// The server's instance mutex. `Local\` scopes it to this Windows session,
/// so each signed-in user can run their own server.
pub const MUTEX_NAME: &str = r"Local\SLC-Local-TTS";

/// Proof that this process holds the instance mutex. Keep it alive for as
/// long as the server runs; dropping it closes the handle, and once every
/// handle is closed the mutex is gone.
#[derive(Debug)]
pub struct InstanceGuard {
    /// `None` when the check couldn't be made (see [`acquire_named`]), so
    /// there is nothing to close.
    handle: Option<HANDLE>,
}

// SAFETY: `HANDLE` is a raw pointer, so Rust assumes it can't cross threads.
// But a kernel handle is a process-wide number, not a pointer into memory
// owned by one thread, and `CloseHandle` may run on any thread. So moving the
// guard to another thread (for example inside an async task) is sound.
unsafe impl Send for InstanceGuard {}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // SAFETY: `handle` came from a successful `CreateMutexW` and is
            // closed only here, once, because `take()` leaves `None` behind.
            unsafe { CloseHandle(handle) };
        }
    }
}

/// Another copy of SLC Local TTS already holds the instance mutex.
#[derive(Debug, thiserror::Error)]
#[error(
    "SLC Local TTS is already running, so this copy won't start a second one. \
     Use the copy that's already running, or close it before starting SLC Local TTS again."
)]
pub struct AlreadyRunning;

/// Takes the server's instance mutex, [`MUTEX_NAME`].
pub fn acquire() -> Result<InstanceGuard, AlreadyRunning> {
    acquire_named(MUTEX_NAME)
}

/// Takes the named instance mutex: `Ok` with a guard to keep alive, or
/// `Err(AlreadyRunning)` if another process holds it. The tests use their
/// own names so they never clash with a real server.
pub fn acquire_named(name: &str) -> Result<InstanceGuard, AlreadyRunning> {
    // Windows wants the name as UTF-16 with a terminating zero.
    let wide: Vec<u16> = name.encode_utf16().chain(iter::once(0)).collect();

    // SAFETY: `wide` is a zero-terminated UTF-16 string that outlives the
    // call; a null security-attributes pointer means "the defaults".
    // `GetLastError` is read straight after, before any other Windows call
    // can overwrite it.
    let (handle, error) = unsafe {
        let handle = CreateMutexW(ptr::null(), FALSE, wide.as_ptr());
        (handle, GetLastError())
    };

    if !handle.is_null() {
        let guard = InstanceGuard {
            handle: Some(handle),
        };
        if error == ERROR_ALREADY_EXISTS {
            // We only opened another copy's mutex. Dropping the guard closes
            // our handle; theirs keeps the mutex alive.
            drop(guard);
            return Err(AlreadyRunning);
        }
        return Ok(guard);
    }

    if error == ERROR_ACCESS_DENIED {
        // The mutex exists but we may not open it: a copy started as
        // administrator holds it.
        return Err(AlreadyRunning);
    }

    tracing::warn!(
        "couldn't check whether SLC Local TTS is already running (Windows error {error}); starting anyway. If another copy is running, the port check stops this one."
    );
    Ok(InstanceGuard { handle: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mutex name no other test or real server uses.
    fn unique_name() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!(r"Local\SLC-Local-TTS-test-{}-{nanos}", std::process::id())
    }

    #[test]
    fn second_acquire_is_already_running() {
        let name = unique_name();
        let _first = acquire_named(&name).expect("first acquire");
        assert!(acquire_named(&name).is_err());
    }

    #[test]
    fn dropping_the_guard_releases_it() {
        let name = unique_name();
        let first = acquire_named(&name).expect("first acquire");
        drop(first);
        assert!(acquire_named(&name).is_ok());
    }

    #[test]
    fn already_running_text() {
        assert_eq!(
            AlreadyRunning.to_string(),
            "SLC Local TTS is already running, so this copy won't start a second one. \
             Use the copy that's already running, or close it before starting SLC Local TTS again."
        );
    }
}
