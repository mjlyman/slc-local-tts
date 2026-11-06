//! Downloading, verifying and unpacking one model archive.
//!
//! An archive is installed into `models\<stem>\` in these steps: check disk
//! space, stream the download into `models\.tmp\<stem>.part` while hashing it,
//! check size and sha256, unpack into `models\.tmp\<stem>.extract`, move the
//! folder into place, then write the `.verified` manifest. The manifest's first
//! line is `<archive sha256>  <url>`; every following line is
//! `<file sha256>  <relative/path>`, sorted by path.
//!
//! Every step runs while holding `models\.tmp\<stem>.lock`, an advisory file
//! lock. The `download` command and a running server may both install the
//! same archive, and they share these scratch paths, so the lock lets only
//! one copy of SLC Local TTS at a time touch them. Windows releases the lock
//! when the file is closed or the process exits, even after a crash.

use std::fmt::Write as _;
use std::fs;
use std::future::Future;
use std::io::{self, BufReader};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::catalog::Archive;

/// Name of the manifest file written into every installed model folder.
const MARKER: &str = ".verified";
/// Scratch folder (under `models\`) for downloads and extraction.
const TMP_DIR: &str = ".tmp";
/// Suffix for model folders that failed verification.
const BROKEN_SUFFIX: &str = ".broken";
/// Suffixes of the scratch entries for one archive in `models\.tmp\`.
const PART_SUFFIX: &str = ".part";
const EXTRACT_SUFFIX: &str = ".extract";
const LOCK_SUFFIX: &str = ".lock";
/// How often an install waiting for another copy's lock tries again.
const LOCK_POLL: Duration = Duration::from_millis(500);
/// Folders inside an archive that we never install: espeak-ng data comes from
/// our own private copy, and `dict/` is only used by engines we don't run.
const SKIPPED_DIRS: [&str; 2] = ["espeak-ng-data", "dict"];

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("network error: {0}")]
    Network(String),
    #[error("server answered HTTP {0}")]
    HttpStatus(u16),
    #[error("size mismatch: expected {expected} bytes, got {got}")]
    SizeMismatch { expected: u64, got: u64 },
    #[error("sha256 mismatch: expected {expected}, got {got}")]
    HashMismatch { expected: String, got: String },
    #[error("not enough disk space: need {needed} bytes, {free} free")]
    DiskSpace { needed: u64, free: u64 },
    #[error("extraction failed: {0}")]
    Extract(String),
    #[error("file error: {0}")]
    Io(String),
}

impl InstallError {
    /// A plain-words explanation for people who aren't developers.
    pub fn user_reason(&self) -> String {
        match self {
            InstallError::Network(_) => "no internet connection or GitHub is unreachable; \
                 check your connection and try again"
                .to_string(),
            InstallError::HttpStatus(code) => format!(
                "GitHub refused the download (HTTP {code}); try again later, \
                 and update SLC Local TTS if it keeps happening"
            ),
            InstallError::SizeMismatch { .. } | InstallError::HashMismatch { .. } => {
                "the download didn't match the expected file; please update SLC Local TTS"
                    .to_string()
            }
            InstallError::DiskSpace { needed, free } => format!(
                "not enough free disk space: the voice needs about {} MB free but only {} MB \
                 is available; free up some space and try again",
                needed.div_ceil(1_000_000),
                free / 1_000_000
            ),
            InstallError::Extract(_) => "the downloaded voice files couldn't be unpacked; \
                 try again, and update SLC Local TTS if it keeps happening"
                .to_string(),
            InstallError::Io(detail) => format!(
                "SLC Local TTS couldn't write the voice files to disk ({detail}); \
                 check that the disk isn't full or read-only and try again"
            ),
        }
    }

    /// True when retrying can't help: the file on the server isn't the one the
    /// catalog expects, so only an app update fixes it.
    pub fn is_permanent(&self) -> bool {
        matches!(
            self,
            InstallError::HashMismatch { .. } | InstallError::SizeMismatch { .. }
        )
    }
}

impl From<io::Error> for InstallError {
    fn from(e: io::Error) -> Self {
        InstallError::Io(e.to_string())
    }
}

/// Installs one archive into `models_dir` and returns the model folder.
/// A trait so the model store can be tested with a fake.
pub trait Installer: Send + Sync + 'static {
    fn install(
        &self,
        archive: &'static Archive,
        models_dir: &Path,
    ) -> impl Future<Output = Result<PathBuf, InstallError>> + Send;
}

/// The real installer: downloads from the archive's URL over HTTP(S).
pub struct HttpInstaller {
    client: reqwest::Client,
}

impl HttpInstaller {
    pub fn new() -> HttpInstaller {
        // No overall timeout (a big model on a slow line legitimately takes
        // minutes); instead give up if connecting or any single read stalls.
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(30))
            .build()
            .expect("the HTTP client settings are fixed and valid");
        HttpInstaller { client }
    }

    /// Streams the archive into `part`, returning the byte count and sha256.
    async fn download(
        &self,
        archive: &Archive,
        part: &Path,
    ) -> Result<(u64, String), InstallError> {
        let mut response = self
            .client
            .get(archive.url)
            .send()
            .await
            .map_err(network_error)?;
        let status = response.status();
        if !status.is_success() {
            return Err(InstallError::HttpStatus(status.as_u16()));
        }

        // tokio's File hands every write to a blocking thread; the buffer
        // batches the small network chunks into fewer, larger writes.
        let file = tokio::fs::File::create(part).await?;
        let mut out = tokio::io::BufWriter::with_capacity(256 * 1024, file);
        let mut hasher = Sha256::new();
        let mut got: u64 = 0;
        let mut logged_tenths: u64 = 0;

        tracing::info!(
            stem = archive.stem,
            url = archive.url,
            "downloading voice model"
        );
        while let Some(chunk) = response.chunk().await.map_err(network_error)? {
            got += chunk.len() as u64;
            if got > archive.size {
                // Stop early rather than fill the disk with a wrong file.
                return Err(InstallError::SizeMismatch {
                    expected: archive.size,
                    got,
                });
            }
            hasher.update(&chunk);
            out.write_all(&chunk).await?;

            let tenths = got * 10 / archive.size.max(1);
            if tenths > logged_tenths {
                logged_tenths = tenths;
                tracing::info!(stem = archive.stem, "download {}% done", tenths * 10);
            }
        }
        out.flush().await?;
        Ok((got, to_hex(&hasher.finalize())))
    }
}

impl Default for HttpInstaller {
    fn default() -> Self {
        HttpInstaller::new()
    }
}

impl Installer for HttpInstaller {
    async fn install(
        &self,
        archive: &'static Archive,
        models_dir: &Path,
    ) -> Result<PathBuf, InstallError> {
        let tmp = models_dir.join(TMP_DIR);
        fs::create_dir_all(&tmp)?;
        let lock = wait_for_install_lock(&tmp, archive.stem).await?;

        // While we waited, the other copy may have installed it.
        let target = models_dir.join(archive.stem);
        if is_installed(archive, models_dir) {
            tracing::info!(stem = archive.stem, dir = %target.display(), "voice model is installed");
            return Ok(target);
        }
        check_disk_space(archive.size, free_bytes(models_dir)?)?;

        let part = tmp.join(format!("{}{PART_SUFFIX}", archive.stem));
        let extract = tmp.join(format!("{}{EXTRACT_SUFFIX}", archive.stem));
        // Left by a crash or an older version. Nobody else can be using it:
        // we hold the lock. (`download` recreates `.part` from empty.)
        remove_if_present(&extract)?;
        // Deletes the scratch files when dropped, then releases the lock.
        // During the download this future owns it, so a failure or a cancel
        // (future dropped) cleans up and lets another copy go ahead.
        let cleanup = ScratchCleanup {
            part: part.clone(),
            extract: extract.clone(),
            _lock: lock,
        };

        let (got, sha256) = self.download(archive, &part).await?;
        if got != archive.size {
            return Err(InstallError::SizeMismatch {
                expected: archive.size,
                got,
            });
        }
        if sha256 != archive.sha256 {
            return Err(InstallError::HashMismatch {
                expected: archive.sha256.to_string(),
                got: sha256,
            });
        }
        tracing::info!(stem = archive.stem, "download verified; unpacking");

        // Unpacking, moving and hashing hundreds of MB is slow, blocking file
        // work. `spawn_blocking` runs it on tokio's blocking thread pool so the
        // async worker threads stay free to answer SLC. The closure must own
        // its data ('static), hence the cloned PathBufs.
        //
        // Dropping this future does NOT stop a blocking task that has already
        // started; it keeps running to the end. So from here on the task owns
        // the cleanup guard, and with it the lock file: `move` hands both to
        // the closure. The scratch files are deleted only after the task has
        // stopped using them, never underneath it, and another copy of SLC
        // Local TTS can't take the lock until `finish_install` has returned,
        // even if our caller gave up long before. (If the runtime shuts down
        // before the task starts, the closure is dropped and the guard runs.)
        //
        // `cancelled` tells the task its caller has gone away, so it must not
        // move anything into `models\<stem>` or write `.verified`.
        let task_target = target.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = CancelOnDrop(Arc::clone(&cancelled));
        tokio::task::spawn_blocking(move || {
            let _cleanup = cleanup;
            finish_install(&part, &extract, &task_target, archive, &cancelled)
        })
        .await
        .map_err(|e| InstallError::Extract(format!("the unpacking task failed: {e}")))??;

        tracing::info!(stem = archive.stem, dir = %target.display(), "voice model installed");
        Ok(target)
    }
}

/// The blocking half of an install: unpack the verified download, move it
/// into `target` and write the marker. Does nothing to `target` if
/// `cancelled` is set once unpacking is done.
fn finish_install(
    part: &Path,
    extract: &Path,
    target: &Path,
    archive: &Archive,
    cancelled: &AtomicBool,
) -> Result<(), InstallError> {
    unpack(part, extract, archive.stem)?;
    let unpacked = extract.join(archive.stem);
    if !unpacked.is_dir() {
        return Err(InstallError::Extract(format!(
            "the archive has no `{}` folder",
            archive.stem
        )));
    }
    // Checked after the long unpack. A cancel that lands after this check is
    // harmless: the folder is already fully unpacked from a verified archive,
    // so installing it is correct.
    if cancelled.load(Ordering::Relaxed) {
        return Err(InstallError::Io("the install was cancelled".to_string()));
    }
    // `install` re-checked `is_installed` after taking the lock, and no other
    // copy can install this archive while we hold it, so an existing folder
    // here is never a valid install. Windows can't rename onto an existing
    // folder, so remove it first.
    if target.exists() {
        fs::remove_dir_all(target)?;
    }
    rename_with_retry(&unpacked, target)?;
    write_marker(target, archive)?;
    Ok(())
}

/// Removes the download and extraction scratch files on drop, then releases
/// the install lock. The `.tmp` folder and the `.lock` file stay: another
/// install may be about to use them, and `clean_leftovers` sweeps them at
/// startup.
struct ScratchCleanup {
    part: PathBuf,
    extract: PathBuf,
    /// The locked `.lock` file. Never read; owning it is the point. Rust
    /// drops a struct's fields after its `drop` method has run, so the lock
    /// is released only once the scratch files are gone.
    _lock: fs::File,
}

impl Drop for ScratchCleanup {
    fn drop(&mut self) {
        // Errors are ignored: `clean_leftovers` sweeps anything left at startup.
        let _ = fs::remove_file(&self.part);
        let _ = fs::remove_dir_all(&self.extract);
    }
}

/// Opens `tmp\<stem>.lock` (creating it if needed) and waits until this
/// process holds its lock. The returned `File` is the lock: closing it, or
/// the process ending, releases it.
///
/// Waiting polls `try_lock` with an async sleep instead of calling the
/// blocking `lock()`, so no tokio worker thread is ever stuck; if the caller
/// gives up (the future is dropped), the wait simply stops.
async fn wait_for_install_lock(tmp: &Path, stem: &str) -> Result<fs::File, InstallError> {
    let path = tmp.join(format!("{stem}{LOCK_SUFFIX}"));
    let file = open_lock_file(&path).map_err(|e| lock_error(&path, &e))?;
    let mut said_waiting = false;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(fs::TryLockError::WouldBlock) => {
                if !said_waiting {
                    tracing::info!(
                        "another copy of SLC Local TTS is downloading {stem}; \
                         waiting for it to finish"
                    );
                    said_waiting = true;
                }
                tokio::time::sleep(LOCK_POLL).await;
            }
            Err(fs::TryLockError::Error(e)) => return Err(lock_error(&path, &e)),
        }
    }
}

/// Opens a lock file for locking, creating it if it's missing. It is never
/// truncated or written: only the lock on it matters.
fn open_lock_file(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

fn lock_error(path: &Path, e: &io::Error) -> InstallError {
    InstallError::Io(format!(
        "couldn't lock the download file {}: {e}",
        path.display()
    ))
}

/// Deletes a file or folder; one that is already gone is fine.
fn remove_if_present(path: &Path) -> io::Result<()> {
    let result = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) => Err(e),
    };
    match result {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Sets the shared flag when dropped, i.e. when the install future ends for
/// any reason, including being cancelled.
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        // Relaxed is enough: the flag is the only data shared through it.
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Turns a reqwest error into `Network`, or `HttpStatus` if it carries one.
fn network_error(e: reqwest::Error) -> InstallError {
    if let Some(status) = e.status() {
        return InstallError::HttpStatus(status.as_u16());
    }
    // reqwest's own message is terse ("error sending request"); the source
    // chain says why (refused, timed out, DNS). Catalog URLs carry no secrets.
    let mut message = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(cause) = source {
        let _ = write!(message, ": {cause}");
        source = cause.source();
    }
    InstallError::Network(message)
}

/// Unpacks the `.tar.bz2` at `part` into `extract`, skipping `SKIPPED_DIRS`.
fn unpack(part: &Path, extract: &Path, stem: &str) -> Result<(), InstallError> {
    let extract_error = |e: io::Error| InstallError::Extract(e.to_string());
    fs::create_dir_all(extract)?;
    let file = fs::File::open(part)?;
    let decoder = bzip2::read::BzDecoder::new(BufReader::new(file));
    let mut tar = tar::Archive::new(decoder);
    for entry in tar.entries().map_err(extract_error)? {
        let mut entry = entry.map_err(extract_error)?;
        let path = entry.path().map_err(extract_error)?.into_owned();
        // Compare real names only, so `./<stem>/dict/x` is skipped too.
        let mut parts = path.components().filter_map(|c| match c {
            Component::Normal(name) => Some(name),
            _ => None,
        });
        let first = parts.next();
        let second = parts.next();
        let skipped = first.is_some_and(|f| f == stem)
            && second.is_some_and(|s| SKIPPED_DIRS.iter().any(|d| s == *d));
        if skipped {
            continue;
        }
        // `unpack_in` drops `.`, root and drive-prefix components so every
        // entry lands inside `extract`; it returns Ok(false) only for a `..`
        // component, which could escape it.
        if !entry.unpack_in(extract).map_err(extract_error)? {
            return Err(InstallError::Extract(format!(
                "the archive has an unsafe path: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

/// True if `models\<stem>\.verified` exists and names this archive's sha256.
pub fn is_installed(archive: &Archive, models_dir: &Path) -> bool {
    let marker = models_dir.join(archive.stem).join(MARKER);
    let Ok(text) = fs::read_to_string(marker) else {
        return false;
    };
    text.lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        == Some(archive.sha256)
}

/// Re-hashes every file the manifest lists. `Ok(false)` if the manifest is
/// missing or unreadable, a listed file is missing, or any hash differs.
pub fn verify_files(dir: &Path) -> io::Result<bool> {
    let text = match fs::read_to_string(dir.join(MARKER)) {
        Ok(text) => text,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidData
            ) =>
        {
            return Ok(false);
        }
        Err(e) => return Err(e),
    };
    let mut lines = text.lines();
    // Line 1 is `<archive sha256>  <url>`.
    if lines.next().and_then(|l| l.split_once("  ")).is_none() {
        return Ok(false);
    }
    // A manifest listing no files can't vouch for anything.
    let mut lines = lines.peekable();
    if lines.peek().is_none() {
        return Ok(false);
    }
    for line in lines {
        let Some((expected, relative)) = line.split_once("  ") else {
            return Ok(false);
        };
        match hash_file(&join_relative(dir, relative)) {
            Ok(actual) if actual == expected => {}
            Ok(_) => return Ok(false),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// Renames a bad model folder to `<dir>.broken` so it's reinstalled and the
/// old files are swept by `clean_leftovers` later.
pub fn quarantine(dir: &Path) -> io::Result<()> {
    let mut broken = dir.as_os_str().to_owned();
    broken.push(BROKEN_SUFFIX);
    let broken = PathBuf::from(broken);
    // An older quarantine of the same folder would block the rename.
    match fs::remove_dir_all(&broken) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    rename_with_retry(dir, &broken)
}

/// Deletes the download leftovers in `models\.tmp\` and every
/// `models\*.broken`. Run at server startup, before this process starts any
/// install.
///
/// Another copy of SLC Local TTS (the `download` command) may be installing
/// right now, so an archive's scratch entries (`<stem>.part` and `.extract`)
/// are deleted only if its lock can be taken; otherwise they're kept. The
/// empty `<stem>.lock` files and the `.tmp` folder itself always stay.
pub fn clean_leftovers(models_dir: &Path) -> io::Result<()> {
    clean_scratch(&models_dir.join(TMP_DIR))?;
    let entries = match fs::read_dir(models_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().ends_with(BROKEN_SUFFIX) {
            continue;
        }
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// The `clean_leftovers` sweep of the `.tmp` folder.
fn clean_scratch(tmp: &Path) -> io::Result<()> {
    let entries = match fs::read_dir(tmp) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    // A BTreeSet so each stem is handled once, however many entries it has.
    let mut stems = std::collections::BTreeSet::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let stem = name.to_str().and_then(|name| {
            [PART_SUFFIX, EXTRACT_SUFFIX, LOCK_SUFFIX]
                .iter()
                .find_map(|suffix| name.strip_suffix(suffix))
        });
        match stem {
            Some(stem) => {
                stems.insert(stem.to_string());
            }
            // Not one of ours (or from an older version): just delete it.
            None => remove_if_present(&entry.path())?,
        }
    }
    for stem in stems {
        let lock_path = tmp.join(format!("{stem}{LOCK_SUFFIX}"));
        // Taken (and created if missing) even when no `.lock` exists yet, so
        // an install starting right now can't create its `.part` while we
        // delete the old one.
        let lock = open_lock_file(&lock_path)?;
        match lock.try_lock() {
            Ok(()) => {}
            // Another copy is installing this archive: leave it alone.
            Err(fs::TryLockError::WouldBlock) => continue,
            Err(fs::TryLockError::Error(e)) => return Err(e),
        }
        remove_if_present(&tmp.join(format!("{stem}{PART_SUFFIX}")))?;
        remove_if_present(&tmp.join(format!("{stem}{EXTRACT_SUFFIX}")))?;
        // The lock file itself is never deleted (it's empty). Another copy
        // may have it open while it waits to retry `try_lock`. Windows 11
        // unlinks a deleted file at once even while handles are open, so
        // that copy would go on to lock an orphaned file while a newcomer
        // locks a fresh one at the same path, and both would install.
        drop(lock);
    }
    Ok(())
}

/// Writes `dir\.verified`: the archive line, then one hash line per file.
pub fn write_marker(dir: &Path, archive: &Archive) -> io::Result<()> {
    let mut files = Vec::new();
    collect_files(dir, "", &mut files)?;
    files.sort();

    // The marker and its temporary file (a leftover from a crash) are not
    // model files, so they are never listed.
    let partial_name = format!("{MARKER}.part");
    let mut text = format!("{}  {}\n", archive.sha256, archive.url);
    for relative in &files {
        if *relative == MARKER || *relative == partial_name {
            continue;
        }
        let hash = hash_file(&join_relative(dir, relative))?;
        let _ = writeln!(text, "{hash}  {relative}");
    }
    // Write a temporary file, then rename: a crash mid-write never leaves a
    // half-written marker that looks valid.
    let partial = dir.join(partial_name);
    fs::write(&partial, text)?;
    rename_with_retry(&partial, &dir.join(MARKER))
}

/// Turns a manifest path like `sub/a.txt` back into a path under `dir`.
fn join_relative(dir: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(dir.to_path_buf(), |p, part| p.join(part))
}

/// Appends every file under `dir` to `out` as a `/`-separated path relative
/// to the top folder; `prefix` is the path of `dir` so far.
fn collect_files(dir: &Path, prefix: &str, out: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|name| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("file name isn't valid Unicode: {name:?}"),
            )
        })?;
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if entry.file_type()?.is_dir() {
            collect_files(&entry.path(), &relative, out)?;
        } else {
            out.push(relative);
        }
    }
    Ok(())
}

/// Lowercase hex sha256 of a file's contents.
fn hash_file(path: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    io::copy(&mut BufReader::new(fs::File::open(path)?), &mut hasher)?;
    Ok(to_hex(&hasher.finalize()))
}

fn to_hex(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

/// Requires 2.5x the archive size free: the archive plus its unpacked copy.
pub fn check_disk_space(size: u64, free: u64) -> Result<(), InstallError> {
    let needed = size * 5 / 2;
    if free >= needed {
        Ok(())
    } else {
        Err(InstallError::DiskSpace { needed, free })
    }
}

/// Free bytes available to this user on the volume holding `path`.
fn free_bytes(path: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    // Windows wants a NUL-terminated UTF-16 string.
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut free: u64 = 0;
    // SAFETY: `wide` is NUL-terminated and outlives the call; `free` is a
    // valid u64 to write into; the two null pointers are optional outputs.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(free)
}

/// Renames with a few retries: antivirus scanners briefly lock freshly
/// unpacked files, which makes a Windows rename fail with "access denied".
pub(crate) fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    const TRIES: u32 = 5;
    let mut attempt = 1;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if attempt >= TRIES => return Err(e),
            Err(_) => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const STEM: &str = "test-voice";

    /// Builds `<STEM>.tar.bz2` in memory. It holds two files we keep plus one
    /// file under each of the folders the installer must skip.
    fn fixture_archive() -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let files: [(&str, &[u8]); 4] = [
            ("model.onnx", b"model"),
            ("tokens.txt", b"a 1\nb 2\n"),
            ("espeak-ng-data/phontab", b"phonemes"),
            ("dict/lexicon.txt", b"word w er d"),
        ];
        for (name, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, format!("{STEM}/{name}"), data)
                .unwrap();
        }
        // A skipped file behind a `./` prefix. `append_data` would strip the
        // `./`, so the name bytes go into the header directly.
        let raw_name = format!("./{STEM}/espeak-ng-data/voices");
        let data: &[u8] = b"voices";
        let mut header = tar::Header::new_gnu();
        header.as_gnu_mut().unwrap().name[..raw_name.len()].copy_from_slice(raw_name.as_bytes());
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, data).unwrap();
        let tar_bytes = builder.into_inner().unwrap();
        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        encoder.write_all(&tar_bytes).unwrap();
        encoder.finish().unwrap()
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        to_hex(&Sha256::digest(bytes))
    }

    /// Serves `bytes` at `/<STEM>.tar.bz2` on a random local port and returns
    /// the base URL. The server task lives as long as the test runtime.
    async fn serve(bytes: Vec<u8>) -> String {
        let body = axum::body::Bytes::from(bytes);
        let app = axum::Router::new().route(
            &format!("/{STEM}.tar.bz2"),
            axum::routing::get(move || {
                let body = body.clone();
                async move { body }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    /// The installer wants `&'static Archive` (catalog entries are compiled
    /// in), so tests leak a small heap allocation to get one.
    fn leak_archive(url: String, sha256: String, size: u64) -> &'static Archive {
        Box::leak(Box::new(Archive {
            stem: STEM,
            url: Box::leak(url.into_boxed_str()),
            sha256: Box::leak(sha256.into_boxed_str()),
            size,
        }))
    }

    /// A served fixture plus a matching catalog entry.
    async fn good_archive() -> &'static Archive {
        let bytes = fixture_archive();
        let base = serve(bytes.clone()).await;
        leak_archive(
            format!("{base}/{STEM}.tar.bz2"),
            sha256_hex(&bytes),
            bytes.len() as u64,
        )
    }

    #[tokio::test]
    async fn install_extracts_and_marks() {
        let archive = good_archive().await;
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path().join("models");

        let dir = HttpInstaller::new()
            .install(archive, &models)
            .await
            .unwrap();

        assert_eq!(dir, models.join(STEM));
        assert_eq!(std::fs::read(dir.join("model.onnx")).unwrap(), b"model");
        assert!(dir.join("tokens.txt").is_file());
        assert!(!dir.join("espeak-ng-data").exists());
        assert!(!dir.join("dict").exists());
        assert!(!dir.join("dict").join("lexicon.txt").exists());
        assert!(is_installed(archive, &models));
        let marker = std::fs::read_to_string(dir.join(".verified")).unwrap();
        let first_line = marker.lines().next().unwrap();
        assert_eq!(first_line, format!("{}  {}", archive.sha256, archive.url));
        assert!(verify_files(&dir).unwrap());
        assert!(no_scratch_left(&models));
    }

    /// True if `models\.tmp` holds nothing but `.lock` files. An install
    /// keeps its lock file: another copy may have it open, waiting.
    fn no_scratch_left(models: &Path) -> bool {
        match std::fs::read_dir(models.join(".tmp")) {
            Ok(entries) => entries.into_iter().all(|entry| {
                let name = entry.unwrap().file_name();
                name.to_string_lossy().ends_with(".lock")
            }),
            Err(e) => e.kind() == io::ErrorKind::NotFound,
        }
    }

    /// Answers one request with the full Content-Length but only half the
    /// body, then holds the connection open without sending more.
    async fn serve_stalling(bytes: Vec<u8>) -> String {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await;
            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", bytes.len());
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(&bytes[..bytes.len() / 2]).await.unwrap();
            std::future::pending::<()>().await;
            drop(socket);
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn cancelled_download_leaves_nothing_installed() {
        let bytes = fixture_archive();
        let base = serve_stalling(bytes.clone()).await;
        let archive = leak_archive(
            format!("{base}/{STEM}.tar.bz2"),
            sha256_hex(&bytes),
            bytes.len() as u64,
        );
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path().join("models");
        let part = models.join(".tmp").join(format!("{STEM}.part"));

        // Box the future so `drop` really destroys it, as a cancel would.
        let installer = HttpInstaller::new();
        let mut install = Box::pin(installer.install(archive, &models));
        // Drive it until the download has started (the `.part` exists).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !part.exists() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "download never started"
            );
            tokio::select! {
                result = &mut install => panic!("install finished: {result:?}"),
                _ = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
        }
        drop(install);

        assert!(!part.exists());
        assert!(!models.join(STEM).join(".verified").exists());
        assert!(!is_installed(archive, &models));
        assert!(no_scratch_left(&models));
    }

    #[test]
    fn cancelled_extraction_never_touches_the_model_folder() {
        let bytes = fixture_archive();
        let archive = leak_archive("http://x/a.tar.bz2".into(), sha256_hex(&bytes), 1);
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path();
        let part = models.join("a.part");
        std::fs::write(&part, &bytes).unwrap();
        let extract = models.join("a.extract");
        // An existing (invalid) install must be left exactly as it was.
        let target = models.join(STEM);
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("old.onnx"), b"old").unwrap();

        let err =
            finish_install(&part, &extract, &target, archive, &AtomicBool::new(true)).unwrap_err();

        assert!(matches!(err, InstallError::Io(_)), "{err:?}");
        assert!(target.join("old.onnx").is_file());
        assert!(!target.join("model.onnx").exists());
        assert!(!target.join(".verified").exists());
        assert!(!is_installed(archive, models));
    }

    #[tokio::test]
    async fn oversized_download_stops_early() {
        let bytes = fixture_archive();
        let base = serve(bytes.clone()).await;
        let small = bytes.len() as u64 - 1;
        let archive = leak_archive(format!("{base}/{STEM}.tar.bz2"), sha256_hex(&bytes), small);
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path().join("models");

        let err = HttpInstaller::new()
            .install(archive, &models)
            .await
            .unwrap_err();

        assert!(
            matches!(err, InstallError::SizeMismatch { expected, got }
                if expected == small && got > small),
            "{err:?}"
        );
        assert!(!models.join(".tmp").join(format!("{STEM}.part")).exists());
        assert!(!models.join(STEM).exists());
    }

    #[tokio::test]
    async fn install_replaces_a_stale_folder() {
        let archive = good_archive().await;
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path().join("models");
        let stale = models.join(STEM);
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("old.onnx"), b"old").unwrap();
        std::fs::write(stale.join(".verified"), "deadbeef  http://old\n").unwrap();

        let dir = HttpInstaller::new()
            .install(archive, &models)
            .await
            .unwrap();

        assert!(!dir.join("old.onnx").exists());
        assert!(dir.join("model.onnx").is_file());
        assert!(is_installed(archive, &models));
    }

    #[tokio::test]
    async fn hash_mismatch_is_permanent_and_cleans_up() {
        let bytes = fixture_archive();
        let base = serve(bytes.clone()).await;
        let archive = leak_archive(
            format!("{base}/{STEM}.tar.bz2"),
            "0".repeat(64),
            bytes.len() as u64,
        );
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path().join("models");

        let err = HttpInstaller::new()
            .install(archive, &models)
            .await
            .unwrap_err();

        assert!(
            matches!(&err, InstallError::HashMismatch { expected, got }
                if *expected == "0".repeat(64) && *got == sha256_hex(&bytes)),
            "{err:?}"
        );
        assert!(err.is_permanent());
        assert!(!models.join(".tmp").join(format!("{STEM}.part")).exists());
        assert!(!models.join(STEM).exists());
    }

    #[tokio::test]
    async fn size_mismatch_detected() {
        let bytes = fixture_archive();
        let base = serve(bytes.clone()).await;
        let real = bytes.len() as u64;
        let archive = leak_archive(
            format!("{base}/{STEM}.tar.bz2"),
            sha256_hex(&bytes),
            real + 1,
        );
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path().join("models");

        let err = HttpInstaller::new()
            .install(archive, &models)
            .await
            .unwrap_err();

        assert!(
            matches!(err, InstallError::SizeMismatch { expected, got }
                if expected == real + 1 && got == real),
            "{err:?}"
        );
        assert!(err.is_permanent());
        assert!(!models.join(STEM).exists());
    }

    #[tokio::test]
    async fn http_404_is_not_permanent() {
        let base = serve(fixture_archive()).await;
        let archive = leak_archive(format!("{base}/missing.tar.bz2"), "0".repeat(64), 10);
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path().join("models");

        let err = HttpInstaller::new()
            .install(archive, &models)
            .await
            .unwrap_err();

        assert!(matches!(err, InstallError::HttpStatus(404)), "{err:?}");
        assert!(!err.is_permanent());
        assert!(!models.join(STEM).exists());
    }

    #[tokio::test]
    async fn connection_refused_is_network_error() {
        // Bind then drop a listener so the port is (almost certainly) closed.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let archive = leak_archive(
            format!("http://127.0.0.1:{port}/{STEM}.tar.bz2"),
            "0".repeat(64),
            10,
        );
        let tmp = tempfile::tempdir().unwrap();

        let err = HttpInstaller::new()
            .install(archive, &tmp.path().join("models"))
            .await
            .unwrap_err();

        assert!(matches!(err, InstallError::Network(_)), "{err:?}");
        assert!(!err.is_permanent());
    }

    #[test]
    fn stale_or_missing_marker_is_not_installed() {
        let archive = leak_archive("http://x/a.tar.bz2".into(), "a".repeat(64), 1);
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path();
        let dir = models.join(STEM);
        std::fs::create_dir_all(&dir).unwrap();

        assert!(!is_installed(archive, models));

        let other = format!("{}  http://x/a.tar.bz2\n", "b".repeat(64));
        std::fs::write(dir.join(".verified"), other).unwrap();
        assert!(!is_installed(archive, models));

        write_marker(&dir, archive).unwrap();
        assert!(is_installed(archive, models));
    }

    #[test]
    fn write_marker_lists_sorted_files_with_forward_slashes() {
        let archive = leak_archive("http://x/a.tar.bz2".into(), "a".repeat(64), 1);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(STEM);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("z.txt"), b"z").unwrap();
        std::fs::write(dir.join("sub").join("a.txt"), b"a").unwrap();

        write_marker(&dir, archive).unwrap();

        let marker = std::fs::read_to_string(dir.join(".verified")).unwrap();
        let lines: Vec<&str> = marker.lines().collect();
        assert_eq!(
            lines,
            [
                format!("{}  http://x/a.tar.bz2", "a".repeat(64)),
                format!("{}  sub/a.txt", sha256_hex(b"a")),
                format!("{}  z.txt", sha256_hex(b"z")),
            ]
        );
        assert!(verify_files(&dir).unwrap());
    }

    #[tokio::test]
    async fn verify_files_detects_tampering() {
        let archive = good_archive().await;
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path().join("models");
        let dir = HttpInstaller::new()
            .install(archive, &models)
            .await
            .unwrap();
        assert!(verify_files(&dir).unwrap());

        std::fs::write(dir.join("tokens.txt"), b"tampered").unwrap();
        assert!(!verify_files(&dir).unwrap());
    }

    #[test]
    fn write_marker_ignores_a_leftover_partial_marker() {
        let archive = leak_archive("http://x/a.tar.bz2".into(), "a".repeat(64), 1);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(STEM);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("m.onnx"), b"m").unwrap();
        std::fs::write(dir.join(".verified.part"), b"from a crash").unwrap();

        write_marker(&dir, archive).unwrap();

        let marker = std::fs::read_to_string(dir.join(".verified")).unwrap();
        assert!(!marker.contains(".verified.part"), "{marker}");
        assert_eq!(marker.lines().count(), 2);
        assert!(verify_files(&dir).unwrap());
    }

    #[test]
    fn verify_files_is_false_for_a_marker_listing_no_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(STEM);
        std::fs::create_dir_all(&dir).unwrap();
        let only_header = format!("{}  http://x/a.tar.bz2\n", "a".repeat(64));
        std::fs::write(dir.join(".verified"), only_header).unwrap();

        assert!(!verify_files(&dir).unwrap());
    }

    #[test]
    fn verify_files_is_false_for_missing_marker_or_file() {
        let archive = leak_archive("http://x/a.tar.bz2".into(), "a".repeat(64), 1);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(STEM);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!verify_files(&dir).unwrap());

        std::fs::write(dir.join("model.onnx"), b"model").unwrap();
        write_marker(&dir, archive).unwrap();
        std::fs::remove_file(dir.join("model.onnx")).unwrap();
        assert!(!verify_files(&dir).unwrap());

        std::fs::write(dir.join(".verified"), "garbage").unwrap();
        assert!(!verify_files(&dir).unwrap());
    }

    #[test]
    fn clean_leftovers_removes_tmp_and_broken() {
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path();
        std::fs::create_dir_all(models.join(".tmp").join("x.extract")).unwrap();
        std::fs::write(models.join(".tmp").join("x.part"), b"partial").unwrap();
        std::fs::create_dir_all(models.join("old-voice.broken")).unwrap();
        std::fs::write(models.join("old-voice.broken").join("m.onnx"), b"m").unwrap();
        std::fs::create_dir_all(models.join("good-voice")).unwrap();

        clean_leftovers(models).unwrap();

        assert!(no_scratch_left(models));
        assert!(!models.join("old-voice.broken").exists());
        assert!(models.join("good-voice").is_dir());
    }

    /// Serves `bytes` like [`serve`], and counts the requests it answers.
    async fn serve_counting(bytes: Vec<u8>) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::AtomicUsize;
        let body = axum::body::Bytes::from(bytes);
        let count = Arc::new(AtomicUsize::new(0));
        let handler_count = Arc::clone(&count);
        let app = axum::Router::new().route(
            &format!("/{STEM}.tar.bz2"),
            axum::routing::get(move || {
                handler_count.fetch_add(1, Ordering::SeqCst);
                let body = body.clone();
                async move { body }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), count)
    }

    /// Opens `models\.tmp\<STEM>.lock` with a handle of its own and locks it,
    /// standing in for another copy of SLC Local TTS.
    fn hold_lock(models: &Path) -> fs::File {
        let tmp = models.join(".tmp");
        fs::create_dir_all(&tmp).unwrap();
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(tmp.join(format!("{STEM}.lock")))
            .unwrap();
        file.try_lock().unwrap();
        file
    }

    #[test]
    fn separate_handles_conflict_on_the_lock() {
        // The cross-process lock relies on this: two handles to one file
        // conflict even inside one process (LockFileEx on Windows).
        let tmp = tempfile::tempdir().unwrap();
        let first = hold_lock(tmp.path());
        let second = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(tmp.path().join(".tmp").join(format!("{STEM}.lock")))
            .unwrap();
        assert!(matches!(
            second.try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
        drop(first);
        second.try_lock().unwrap();
    }

    #[tokio::test]
    async fn install_waits_while_another_copy_holds_the_lock() {
        let bytes = fixture_archive();
        let (base, requests) = serve_counting(bytes.clone()).await;
        let archive = leak_archive(
            format!("{base}/{STEM}.tar.bz2"),
            sha256_hex(&bytes),
            bytes.len() as u64,
        );
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path().join("models");
        let other_copy = hold_lock(&models);

        let installer = HttpInstaller::new();
        let mut install = Box::pin(installer.install(archive, &models));
        let waited = tokio::time::timeout(Duration::from_millis(300), &mut install).await;
        assert!(waited.is_err(), "install didn't wait: {waited:?}");
        assert_eq!(requests.load(Ordering::SeqCst), 0);

        // The other copy finishes the install, then lets go of the lock.
        let target = models.join(STEM);
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("model.onnx"), b"model").unwrap();
        write_marker(&target, archive).unwrap();
        drop(other_copy);

        let dir = tokio::time::timeout(Duration::from_secs(10), install)
            .await
            .expect("install still waiting after the lock was released")
            .unwrap();
        assert_eq!(dir, target);
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        assert!(is_installed(archive, &models));
    }

    #[tokio::test]
    async fn install_proceeds_after_the_lock_is_released_without_an_install() {
        let bytes = fixture_archive();
        let (base, requests) = serve_counting(bytes.clone()).await;
        let archive = leak_archive(
            format!("{base}/{STEM}.tar.bz2"),
            sha256_hex(&bytes),
            bytes.len() as u64,
        );
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path().join("models");
        let other_copy = hold_lock(&models);

        let installer = HttpInstaller::new();
        let mut install = Box::pin(installer.install(archive, &models));
        let waited = tokio::time::timeout(Duration::from_millis(300), &mut install).await;
        assert!(waited.is_err(), "install didn't wait: {waited:?}");
        assert_eq!(requests.load(Ordering::SeqCst), 0);

        // The other copy gave up (failed or was closed) without installing.
        drop(other_copy);

        let dir = tokio::time::timeout(Duration::from_secs(10), install)
            .await
            .expect("install still waiting after the lock was released")
            .unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert!(dir.join("model.onnx").is_file());
        assert!(is_installed(archive, &models));
        assert!(verify_files(&dir).unwrap());
    }

    #[test]
    fn clean_leftovers_keeps_a_locked_download() {
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path();
        let scratch = models.join(".tmp");
        fs::create_dir_all(&scratch).unwrap();
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(scratch.join("x.lock"))
            .unwrap();
        lock.try_lock().unwrap();
        fs::write(scratch.join("x.part"), b"downloading").unwrap();
        fs::create_dir_all(scratch.join("x.extract")).unwrap();
        fs::write(scratch.join("y.part"), b"left by a crash").unwrap();

        clean_leftovers(models).unwrap();

        assert!(scratch.join("x.lock").is_file());
        assert!(scratch.join("x.part").is_file());
        assert!(scratch.join("x.extract").is_dir());
        assert!(!scratch.join("y.part").exists());
        drop(lock);
    }

    /// A copy waiting for an install holds the lock file open between polls
    /// without holding the lock. If the sweep deleted the file, Windows 11
    /// would unlink it at once, the waiter would later lock an orphaned
    /// file, and a new copy would lock a fresh file at the same path: two
    /// installers, one set of scratch files.
    #[test]
    fn clean_leftovers_keeps_lock_files_a_waiter_has_open() {
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path();
        let scratch = models.join(".tmp");
        fs::create_dir_all(&scratch).unwrap();
        let waiter = open_lock_file(&scratch.join("z.lock")).unwrap();
        fs::write(scratch.join("z.part"), b"left by a crash").unwrap();

        clean_leftovers(models).unwrap();

        assert!(!scratch.join("z.part").exists());
        assert!(scratch.join("z.lock").is_file());
        waiter.try_lock().unwrap();
        let newcomer = open_lock_file(&scratch.join("z.lock")).unwrap();
        assert!(matches!(
            newcomer.try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
    }

    #[test]
    fn clean_leftovers_tolerates_missing_models_dir() {
        let tmp = tempfile::tempdir().unwrap();
        clean_leftovers(&tmp.path().join("models")).unwrap();
    }

    #[test]
    fn quarantine_renames_to_broken() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(STEM);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("m.onnx"), b"m").unwrap();
        // A leftover from an earlier quarantine must not block this one.
        std::fs::create_dir_all(tmp.path().join(format!("{STEM}.broken"))).unwrap();

        quarantine(&dir).unwrap();

        assert!(!dir.exists());
        assert!(
            tmp.path()
                .join(format!("{STEM}.broken"))
                .join("m.onnx")
                .is_file()
        );
    }

    #[test]
    fn disk_space_rule() {
        assert!(check_disk_space(100, 250).is_ok());
        assert!(matches!(
            check_disk_space(100, 249),
            Err(InstallError::DiskSpace {
                needed: 250,
                free: 249
            })
        ));
    }

    #[test]
    fn free_bytes_reports_something_for_temp_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(free_bytes(tmp.path()).unwrap() > 0);
    }

    #[test]
    fn user_reasons_are_plain() {
        assert!(
            InstallError::Network("x".into())
                .user_reason()
                .contains("no internet connection or GitHub is unreachable")
        );
        assert!(
            InstallError::HashMismatch {
                expected: "a".into(),
                got: "b".into()
            }
            .user_reason()
            .contains("please update SLC Local TTS")
        );
    }
}
