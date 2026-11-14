//! Local persistence: settings, durable jobs/history, immutable staging, and the archive.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, TimeZone, Utc};
use sha2::{Digest, Sha256};

use crate::app::error::AppError;

pub mod archive;
pub mod artifacts;
pub mod filename;
pub mod history;
pub mod settings;
pub(crate) mod winfs;

/// Prefix of in-progress files the app writes before an atomic rename.
pub(crate) const PARTIAL_PREFIX: &str = ".vectrcap-partial-";
/// Space left free on a volume after staging/archiving a file.
pub(crate) const FREE_SPACE_RESERVE: u64 = 64 * 1024 * 1024;
const COPY_BUFFER_BYTES: usize = 64 * 1024;

/// Default archive root: the user's Windows Pictures known folder + `VectrCap`.
/// Resolved at use time; never created or changed here.
pub fn default_archive_root() -> Result<PathBuf, AppError> {
    dirs::picture_dir()
        .map(|p| p.join("VectrCap"))
        .ok_or(AppError::Io {
            stage: "resolving the Pictures folder",
            kind: io::ErrorKind::NotFound,
        })
}

/// Named interleaving point. Unit tests attach a closure to run another
/// "process" exactly here; in normal builds this compiles to nothing.
#[inline]
pub(crate) fn hook(_point: &'static str) {
    #[cfg(test)]
    test_support::fire(_point);
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::cell::RefCell;
    use std::path::Path;

    type Hook = Box<dyn FnMut(&'static str)>;
    thread_local!(static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) });

    pub(crate) fn set_hook(f: impl FnMut(&'static str) + 'static) {
        HOOK.with(|h| *h.borrow_mut() = Some(Box::new(f)));
    }

    pub(crate) fn clear_hook() {
        HOOK.with(|h| *h.borrow_mut() = None);
    }

    pub(super) fn fire(point: &'static str) {
        // Taken out while running so the hook may call back into the library.
        let Some(mut f) = HOOK.with(|h| h.borrow_mut().take()) else {
            return;
        };
        f(point);
        HOOK.with(|h| {
            h.borrow_mut().get_or_insert(f);
        });
    }

    /// Directory junction (no elevation needed), the Windows fixture for
    /// redirected roots and descendant escapes.
    pub(crate) fn junction(link: &Path, target: &Path) {
        let status = std::process::Command::new("cmd")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "mklink /J failed");
    }

    /// Clears the read-only attribute (attribute writes are not subject to
    /// share modes, so this works even while another handle is open).
    pub(crate) fn make_writable(path: &Path) {
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(path, perms).unwrap();
    }

    /// Open-for-write, rename, and delete attempts by another opener.
    pub(crate) fn mutation_attempts(path: &Path) -> Vec<std::io::Result<()>> {
        use std::io::Write;
        vec![
            std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .and_then(|mut f| f.write_all(b"XX")),
            std::fs::rename(path, path.with_extension("moved")),
            std::fs::remove_file(path),
        ]
    }

    pub(crate) fn assert_sharing_violations(results: &[std::io::Result<()>]) {
        assert_eq!(results.len(), 3, "the interleaving point was not reached");
        for r in results {
            assert_eq!(
                r.as_ref().err().and_then(std::io::Error::raw_os_error),
                Some(32),
                "expected ERROR_SHARING_VIOLATION, got {r:?}"
            );
        }
    }
}

pub(crate) fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

pub(crate) fn local_from_ms(ms: i64) -> Result<DateTime<Local>, AppError> {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .ok_or(AppError::Persistence("stored timestamp is invalid"))
}

/// Streams at most `expected + 1` bytes into `dst` and returns the byte count
/// and SHA-256. A count different from `expected` means the source changed.
pub(crate) fn copy_hashing(
    src: &mut impl Read,
    dst: &mut impl Write,
    expected: u64,
) -> io::Result<(u64, [u8; 32])> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; COPY_BUFFER_BYTES];
    let mut limited = src.take(expected.saturating_add(1));
    let mut total = 0u64;
    loop {
        let n = match limited.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        hasher.update(&buf[..n]);
        dst.write_all(&buf[..n])?;
        total += n as u64;
    }
    Ok((total, hasher.finalize().into()))
}

/// Size and SHA-256 of a regular file, refusing reparse points/symlinks.
pub(crate) fn fingerprint(path: &Path) -> io::Result<(u64, [u8; 32])> {
    let mut file = open_no_follow(path)?;
    let len = file.metadata()?.len();
    copy_hashing(&mut file, &mut io::sink(), len)
}

/// Opens a regular file for reading without following a final reparse point;
/// anything else yields `ErrorKind::InvalidInput`.
#[cfg(windows)]
pub(crate) fn open_no_follow(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
    };

    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(not(windows))]
pub(crate) fn open_no_follow(path: &Path) -> io::Result<File> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    File::open(path)
}

/// Removes a file the app created, clearing the read-only attribute it may carry.
pub(crate) fn remove_owned_file(path: &Path) -> io::Result<()> {
    let mut perms = fs::symlink_metadata(path)?.permissions();
    if perms.readonly() {
        // On Windows this only clears FILE_ATTRIBUTE_READONLY on our own file.
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(path, perms)?;
    }
    fs::remove_file(path)
}

pub(crate) fn ensure_free_space(dir: &Path, needed: u64) -> Result<(), AppError> {
    const STAGE: &str = "checking free disk space";
    let available = available_space(dir).map_err(|e| AppError::io(STAGE, &e))?;
    if available < needed.saturating_add(FREE_SPACE_RESERVE) {
        return Err(AppError::Io {
            stage: STAGE,
            kind: io::ErrorKind::StorageFull,
        });
    }
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn available_space(dir: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let mut wide: Vec<u16> = dir.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains NUL",
        ));
    }
    wide.push(0);
    let mut free: u64 = 0;
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call,
    // `free` is a valid writable u64, and the two optional outputs may be null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(free)
    }
}

#[cfg(not(windows))]
fn available_space(_dir: &Path) -> io::Result<u64> {
    // ponytail: Windows-first app; add statvfs here if a non-Windows build ships.
    Ok(u64::MAX)
}
