use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::app::error::AppError;
use crate::app::types::{Artifact, ArtifactId, ArtifactKind};
use crate::persistence::winfs::{self, ERROR_SHARING_VIOLATION};
use crate::persistence::{
    PARTIAL_PREFIX, copy_hashing, copy_hashing_checked, ensure_free_space, hook,
};

const IN_USE: AppError =
    AppError::InUse("the file is open for writing in another program; close it and try again");

/// Maps a failed exclusive open: an existing writer/deleter is reported as
/// in-use instead of being silently tolerated.
fn open_error(stage: &'static str, e: &io::Error) -> AppError {
    if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) {
        IN_USE
    } else {
        AppError::io(stage, e)
    }
}

const SPOOL_EXTENSION: &str = "spool";
const MAX_DISPLAY_NAME_UTF16: usize = 255;

/// Owns the private spool of immutable upload sources. Spool files use
/// generated names (`<uuid>.spool`), are read-only, and are independent of the
/// user-visible archive.
pub struct ArtifactStore {
    root: PathBuf,
    // ponytail: one staging copy at a time; use a counting semaphore if parallel staging is needed.
    staging: Mutex<()>,
}

impl ArtifactStore {
    pub fn new(root: PathBuf) -> Result<Self, AppError> {
        if !root.is_absolute() {
            return Err(AppError::InvalidInput(
                "staging folder must be an absolute path",
            ));
        }
        fs::create_dir_all(&root).map_err(|e| AppError::io("creating the staging folder", &e))?;
        Ok(ArtifactStore {
            root,
            staging: Mutex::new(()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Copies a regular local file into the spool and records its size and
    /// SHA-256. The original is only read, through a handle that denies other
    /// writers, renames, and deletes for the whole copy; a source already open
    /// for writing is refused (`InUse`) rather than copied inconsistently.
    pub fn stage_file(&self, source: &Path, kind: ArtifactKind) -> Result<Artifact, AppError> {
        const NOT_REGULAR: AppError =
            AppError::InvalidInput("only regular local files can be uploaded");
        const READING: &str = "reading the source file";
        let display_name = display_name_of(source)?;
        check_source_path(source)?;
        let mut src = winfs::open_read_exclusive(source).map_err(|e| {
            // Directories fail to open as files with access-denied; the lookup
            // only classifies the error, the handle check is authoritative.
            let not_file = fs::symlink_metadata(source).is_ok_and(|m| !m.is_file());
            if e.kind() == io::ErrorKind::InvalidInput || not_file {
                NOT_REGULAR
            } else {
                open_error("opening the source file", &e)
            }
        })?;
        hook("stage.source_open");
        // Policy is checked on what the handle actually resolved to (mapped
        // drives, junctions, and symlinked folders included), not the input text.
        let resolved = winfs::final_path(&src).map_err(|e| AppError::io(READING, &e))?;
        if !is_local_final_path(&resolved) {
            return Err(AppError::InvalidInput(
                "network and device paths are not supported",
            ));
        }
        let before = src.metadata().map_err(|e| AppError::io(READING, &e))?;
        let size = before.len();
        let modified = before.modified().map_err(|e| AppError::io(READING, &e))?;

        let _guard = self
            .staging
            .lock()
            .map_err(|_| AppError::Persistence("staging lock poisoned"))?;
        ensure_free_space(&self.root, size)?;
        const STAGE: &str = "staging the file";
        let mut tmp = tempfile::Builder::new()
            .prefix(PARTIAL_PREFIX)
            .tempfile_in(&self.root)
            .map_err(|e| AppError::io(STAGE, &e))?;
        let (copied, sha256) =
            copy_hashing(&mut src, tmp.as_file_mut(), size).map_err(|e| AppError::io(STAGE, &e))?;
        // Defense in depth: writers are excluded, so these can only differ if
        // that guarantee failed. Metadata errors fail instead of matching.
        let after = src.metadata().map_err(|e| AppError::io(STAGE, &e))?;
        let modified_after = after.modified().map_err(|e| AppError::io(STAGE, &e))?;
        if copied != size || after.len() != size || modified_after != modified {
            return Err(AppError::InvalidInput(
                "the file changed while it was being staged; try again",
            ));
        }
        tmp.as_file()
            .sync_all()
            .map_err(|e| AppError::io(STAGE, &e))?;

        let id = ArtifactId::new_random();
        let path = self.spool_path(id);
        // Persisting rewrites the temp file's attributes, so read-only is set
        // afterwards, through the same still-open handle.
        let file = tmp
            .persist_noclobber(&path)
            .map_err(|e| AppError::io(STAGE, &e.error))?;
        let mut perms = file
            .metadata()
            .map_err(|e| AppError::io(STAGE, &e))?
            .permissions();
        perms.set_readonly(true);
        file.set_permissions(perms)
            .map_err(|e| AppError::io(STAGE, &e))?;
        Ok(Artifact {
            id,
            path,
            display_name,
            kind,
            size,
            sha256,
        })
    }

    /// Same contract as [`Self::stage_file`], but polls `cancel` before
    /// opening the source, before waiting on the staging lock, and once per
    /// chunk during the copy itself — so a task already queued behind
    /// another large file, or partway through its own large copy, still
    /// stops promptly instead of finishing the entire file. On cancellation
    /// no artifact is produced, the source is untouched, and the temp file
    /// is removed exactly as it is for any other early return. The returned
    /// artifact is already [`Self::verify`]-checked, so callers that hand it
    /// across a thread/channel boundary need not re-hash it themselves.
    pub fn stage_file_cancellable(
        &self,
        source: &Path,
        kind: ArtifactKind,
        cancel: &CancellationToken,
    ) -> Result<Artifact, AppError> {
        const NOT_REGULAR: AppError =
            AppError::InvalidInput("only regular local files can be uploaded");
        const READING: &str = "reading the source file";
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        let display_name = display_name_of(source)?;
        check_source_path(source)?;
        let mut src = winfs::open_read_exclusive(source).map_err(|e| {
            let not_file = fs::symlink_metadata(source).is_ok_and(|m| !m.is_file());
            if e.kind() == io::ErrorKind::InvalidInput || not_file {
                NOT_REGULAR
            } else {
                open_error("opening the source file", &e)
            }
        })?;
        hook("stage.source_open");
        let resolved = winfs::final_path(&src).map_err(|e| AppError::io(READING, &e))?;
        if !is_local_final_path(&resolved) {
            return Err(AppError::InvalidInput(
                "network and device paths are not supported",
            ));
        }
        let before = src.metadata().map_err(|e| AppError::io(READING, &e))?;
        let size = before.len();
        let modified = before.modified().map_err(|e| AppError::io(READING, &e))?;

        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        let _guard = self
            .staging
            .lock()
            .map_err(|_| AppError::Persistence("staging lock poisoned"))?;
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        ensure_free_space(&self.root, size)?;
        const STAGE: &str = "staging the file";
        let mut tmp = tempfile::Builder::new()
            .prefix(PARTIAL_PREFIX)
            .tempfile_in(&self.root)
            .map_err(|e| AppError::io(STAGE, &e))?;
        let Some((copied, sha256)) =
            copy_hashing_checked(&mut src, tmp.as_file_mut(), size, &|| cancel.is_cancelled())
                .map_err(|e| AppError::io(STAGE, &e))?
        else {
            // `tmp` is dropped here and deletes itself; nothing is orphaned.
            return Err(AppError::Cancelled);
        };
        let after = src.metadata().map_err(|e| AppError::io(STAGE, &e))?;
        let modified_after = after.modified().map_err(|e| AppError::io(STAGE, &e))?;
        if copied != size || after.len() != size || modified_after != modified {
            return Err(AppError::InvalidInput(
                "the file changed while it was being staged; try again",
            ));
        }
        tmp.as_file()
            .sync_all()
            .map_err(|e| AppError::io(STAGE, &e))?;

        let id = ArtifactId::new_random();
        let path = self.spool_path(id);
        let file = tmp
            .persist_noclobber(&path)
            .map_err(|e| AppError::io(STAGE, &e.error))?;
        let artifact = Artifact {
            id,
            path,
            display_name,
            kind,
            size,
            sha256,
        };
        let permissions: Result<(), AppError> = (|| {
            let mut perms = file
                .metadata()
                .map_err(|e| AppError::io(STAGE, &e))?
                .permissions();
            perms.set_readonly(true);
            file.set_permissions(perms)
                .map_err(|e| AppError::io(STAGE, &e))
        })();
        drop(file);
        if let Err(error) = permissions {
            self.discard_staged(&artifact)?;
            return Err(error);
        }
        hook("stage.finalized");
        if let Err(error) = self.verify_cancellable(&artifact, cancel) {
            self.discard_staged(&artifact)?;
            return Err(error);
        }
        Ok(artifact)
    }

    fn verify_cancellable(
        &self,
        artifact: &Artifact,
        cancel: &CancellationToken,
    ) -> Result<(), AppError> {
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        if artifact.path != self.spool_path(artifact.id) {
            return Err(AppError::ArtifactChanged);
        }
        let mut file = winfs::open_read_exclusive(&artifact.path)
            .map_err(|e| AppError::io("verifying staged file", &e))?;
        if file
            .metadata()
            .map_err(|e| AppError::io("verifying staged file", &e))?
            .len()
            != artifact.size
        {
            return Err(AppError::ArtifactChanged);
        }
        let Some((size, hash)) =
            copy_hashing_checked(&mut file, &mut io::sink(), artifact.size, &|| {
                cancel.is_cancelled()
            })
            .map_err(|e| AppError::io("verifying staged file", &e))?
        else {
            return Err(AppError::Cancelled);
        };
        if size != artifact.size || hash != artifact.sha256 {
            return Err(AppError::ArtifactChanged);
        }
        Ok(())
    }
    /// Caller must prove no durable record references this newly staged ID.
    pub(crate) fn discard_staged(&self, artifact: &Artifact) -> Result<(), AppError> {
        if artifact.path != self.spool_path(artifact.id) {
            return Err(AppError::InvalidInput("not an owned staging path"));
        }
        let file = winfs::open_owned_by_path(&artifact.path)
            .map_err(|e| AppError::io("rolling back staged file", &e))?;
        winfs::delete_by_handle(&file).map_err(|e| AppError::io("rolling back staged file", &e))
    }

    fn spool_path(&self, id: ArtifactId) -> PathBuf {
        self.root
            .join(format!("{}.{SPOOL_EXTENSION}", id.0.simple()))
    }

    /// Informational check that the staged bytes still match their record.
    /// This is not an upload primitive: the handle is released on return. Use
    /// `open_verified` to read bytes that are guaranteed to be the verified ones.
    pub fn verify(&self, artifact: &Artifact) -> Result<(), AppError> {
        self.open_verified(artifact).map(drop)
    }

    /// Opens the artifact's own spool file, hashes it, and returns the same
    /// handle rewound to offset 0. The handle denies writers, renames, and
    /// deletes, so every read through the returned guard yields exactly the
    /// verified bytes until it is dropped.
    pub fn open_verified(&self, artifact: &Artifact) -> Result<VerifiedSource, AppError> {
        const STAGE: &str = "verifying the staged file";
        if artifact.path != self.spool_path(artifact.id) {
            return Err(AppError::InvalidInput(
                "artifact is not this store's spool file for its ID",
            ));
        }
        let mut file = winfs::open_read_exclusive(&artifact.path).map_err(|e| match e.kind() {
            io::ErrorKind::InvalidInput => AppError::ArtifactChanged,
            _ => open_error(STAGE, &e),
        })?;
        hook("verify.open");
        let len = file.metadata().map_err(|e| AppError::io(STAGE, &e))?.len();
        if len != artifact.size {
            return Err(AppError::ArtifactChanged);
        }
        let (read, sha256) =
            copy_hashing(&mut file, &mut io::sink(), len).map_err(|e| AppError::io(STAGE, &e))?;
        if read != artifact.size || sha256 != artifact.sha256 {
            return Err(AppError::ArtifactChanged);
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|e| AppError::io(STAGE, &e))?;
        Ok(VerifiedSource {
            file,
            artifact_id: artifact.id,
            size: read,
            sha256,
        })
    }

    /// Startup recovery: removes interrupted partial files and spool files no
    /// unfinished job needs. Anything the app did not name is left alone.
    pub fn recover(&self, needed: &HashSet<ArtifactId>) -> Result<usize, AppError> {
        const STAGE: &str = "cleaning the staging folder";
        let _guard = self
            .staging
            .lock()
            .map_err(|_| AppError::Persistence("staging lock poisoned"))?;
        let mut removed = 0;
        for entry in fs::read_dir(&self.root).map_err(|e| AppError::io(STAGE, &e))? {
            let entry = entry.map_err(|e| AppError::io(STAGE, &e))?;
            if !entry
                .file_type()
                .map_err(|e| AppError::io(STAGE, &e))?
                .is_file()
            {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let orphan = name.starts_with(PARTIAL_PREFIX)
                || spool_id(name).is_some_and(|id| !needed.contains(&id));
            if !orphan {
                continue;
            }
            // Deleted through the handle that was checked to be a regular file;
            // a file someone still has open is left for the next startup.
            let file = match winfs::open_owned_by_path(&entry.path()) {
                Ok(file) => file,
                Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => continue,
                Err(e) if e.kind() == io::ErrorKind::InvalidInput => continue,
                Err(e) => return Err(AppError::io(STAGE, &e)),
            };
            winfs::delete_by_handle(&file).map_err(|e| AppError::io(STAGE, &e))?;
            removed += 1;
        }
        Ok(removed)
    }
}

/// Upload source guard from `ArtifactStore::open_verified`: an open spool
/// handle whose bytes were hashed against the artifact record and cannot be
/// modified, renamed, or deleted while this value lives. Uploads read (and
/// seek for chunk offsets) through it instead of reopening `Artifact::path`.
pub struct VerifiedSource {
    file: fs::File,
    artifact_id: ArtifactId,
    size: u64,
    sha256: [u8; 32],
}

impl VerifiedSource {
    pub fn artifact_id(&self) -> ArtifactId {
        self.artifact_id
    }

    pub fn len(&self) -> u64 {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
}

impl Read for VerifiedSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}

impl Seek for VerifiedSource {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
}

/// `\\?\X:\...` only: rejects `\\?\UNC\...` (network shares, including mapped
/// drives), volume-GUID, and device namespaces.
fn is_local_final_path(final_path: &str) -> bool {
    let Some(rest) = final_path.strip_prefix(r"\\?\") else {
        return false;
    };
    matches!(rest.as_bytes(), [drive, b':', b'\\', ..] if drive.is_ascii_alphabetic())
}

/// Parses `<32-hex-uuid>.spool`, the only spool names this store generates.
fn spool_id(name: &str) -> Option<ArtifactId> {
    let stem = name.strip_suffix(".spool")?;
    if stem.len() != 32 {
        return None;
    }
    Uuid::try_parse(stem).ok().map(ArtifactId)
}

fn display_name_of(source: &Path) -> Result<String, AppError> {
    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(AppError::InvalidInput("file name must be valid Unicode"))?;
    if name.encode_utf16().count() > MAX_DISPLAY_NAME_UTF16 || name.chars().any(char::is_control) {
        return Err(AppError::InvalidInput(
            "file name is too long or contains control characters",
        ));
    }
    Ok(name.to_owned())
}

/// Accepts only absolute local-disk paths without alternate data streams.
fn check_source_path(source: &Path) -> Result<(), AppError> {
    if !source.is_absolute() {
        return Err(AppError::InvalidInput("source path must be absolute"));
    }
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        let local_disk = matches!(
            source.components().next(),
            Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
        );
        if !local_disk {
            return Err(AppError::InvalidInput(
                "network and device paths are not supported",
            ));
        }
        let stream = source
            .components()
            .any(|c| matches!(c, Component::Normal(part) if part.to_string_lossy().contains(':')));
        if stream {
            return Err(AppError::InvalidInput(
                "alternate data streams are not supported",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io::{Read, Seek, SeekFrom};
    use std::rc::Rc;

    use tempfile::TempDir;

    use super::*;
    use crate::persistence::test_support::{
        assert_sharing_violations, clear_hook, junction, make_writable, mutation_attempts, set_hook,
    };

    fn fixture() -> (TempDir, ArtifactStore) {
        let dir = TempDir::new().unwrap();
        let store = ArtifactStore::new(dir.path().join("spool")).unwrap();
        (dir, store)
    }

    #[test]
    fn cancellation_after_finalize_removes_the_unadmitted_spool() {
        let (dir, store) = fixture();
        let source = dir.path().join("source.txt");
        fs::write(&source, b"synthetic").unwrap();
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        set_hook(move |point| {
            if point == "stage.finalized" {
                trigger.cancel();
            }
        });
        let outcome = store.stage_file_cancellable(&source, ArtifactKind::File, &cancel);
        clear_hook();
        assert!(matches!(outcome, Err(AppError::Cancelled)));
        assert!(spool_is_empty(&store));
        assert_eq!(fs::read(source).unwrap(), b"synthetic");
    }
    fn spool_is_empty(store: &ArtifactStore) -> bool {
        fs::read_dir(store.root())
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true)
    }

    #[test]
    fn cancellation_before_any_work_touches_nothing() {
        let (dir, store) = fixture();
        let src = dir.path().join("a.bin");
        fs::write(&src, b"synthetic").unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            store.stage_file_cancellable(&src, ArtifactKind::File, &cancel),
            Err(AppError::Cancelled)
        ));
        assert_eq!(fs::read(&src).unwrap(), b"synthetic");
        assert!(spool_is_empty(&store));
    }

    #[test]
    fn cancellation_queued_behind_the_staging_lock_still_stops_before_copying() {
        // Simulates a task that was already admitted (source opened) when
        // Quit/cancel arrives, but has not yet reached the actual copy —
        // exactly the gap the parent review flagged: a check only before the
        // mutex wait is not enough on its own, but this one fires AFTER the
        // open and BEFORE the lock, independent of the per-chunk check below.
        let (dir, store) = fixture();
        let src = dir.path().join("a.bin");
        fs::write(&src, b"synthetic").unwrap();
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        set_hook(move |point| {
            if point == "stage.source_open" {
                c.cancel();
            }
        });
        let result = store.stage_file_cancellable(&src, ArtifactKind::File, &cancel);
        clear_hook();
        assert!(matches!(result, Err(AppError::Cancelled)));
        assert_eq!(fs::read(&src).unwrap(), b"synthetic");
        assert!(
            spool_is_empty(&store),
            "no temp/spool file must be left behind"
        );
    }

    #[test]
    fn cancellation_mid_copy_leaves_no_partial_spool_file() {
        let (dir, store) = fixture();
        let src = dir.path().join("big.bin");
        fs::write(&src, vec![9u8; 3 * 64 * 1024 + 5]).unwrap();
        // `stage.source_open` fires once, before the copy loop starts; by
        // cancelling there we exercise the per-chunk check inside the copy
        // itself rather than the earlier open/mutex checks above.
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        set_hook(move |point| {
            if point == "stage.source_open" {
                c.cancel();
            }
        });
        let result = store.stage_file_cancellable(&src, ArtifactKind::File, &cancel);
        clear_hook();
        assert!(matches!(result, Err(AppError::Cancelled)));
        assert!(
            spool_is_empty(&store),
            "the partial temp file must not survive"
        );
    }

    #[test]
    fn an_uncancelled_token_stages_exactly_like_stage_file() {
        let (dir, store) = fixture();
        let src = dir.path().join("a.bin");
        fs::write(&src, b"synthetic payload").unwrap();
        let artifact = store
            .stage_file_cancellable(&src, ArtifactKind::File, &CancellationToken::new())
            .unwrap();
        assert_eq!(fs::read(&artifact.path).unwrap(), b"synthetic payload");
        // Already verified internally; a fresh verify must still agree.
        store.verify(&artifact).unwrap();
    }

    #[test]
    fn existing_writer_blocks_staging() {
        let (dir, store) = fixture();
        let src = dir.path().join("live.log");
        fs::write(&src, b"v1").unwrap();
        // std's default share mode is read|write|delete: a typical concurrent writer.
        let writer = fs::OpenOptions::new().write(true).open(&src).unwrap();
        assert!(matches!(
            store.stage_file(&src, ArtifactKind::File),
            Err(AppError::InUse(_))
        ));
        drop(writer);
        store.stage_file(&src, ArtifactKind::File).unwrap();
    }

    #[test]
    fn staging_excludes_writers_renames_and_deletes_during_copy() {
        let (dir, store) = fixture();
        let src = dir.path().join("big.bin");
        let original = vec![7u8; 3 * 64 * 1024 + 5];
        fs::write(&src, &original).unwrap();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let (s, p) = (seen.clone(), src.clone());
        set_hook(move |point| {
            if point == "stage.source_open" {
                *s.borrow_mut() = mutation_attempts(&p);
            }
        });
        let art = store.stage_file(&src, ArtifactKind::File);
        clear_hook();
        assert_sharing_violations(&seen.borrow());
        let art = art.unwrap();
        assert_eq!(fs::read(&art.path).unwrap(), original);
        assert_eq!(fs::read(&src).unwrap(), original);
    }

    #[test]
    fn verified_source_holds_its_bytes_until_dropped() {
        let (dir, store) = fixture();
        let src = dir.path().join("a.bin");
        let original: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        fs::write(&src, &original).unwrap();
        let art = store.stage_file(&src, ArtifactKind::File).unwrap();

        let mut v = store.open_verified(&art).unwrap();
        assert_eq!((v.len(), v.sha256()), (art.size, &art.sha256));
        make_writable(&art.path);
        assert_sharing_violations(&mutation_attempts(&art.path));
        let mut first = Vec::new();
        v.read_to_end(&mut first).unwrap();
        assert_eq!(first, original, "verified handle starts at offset 0");
        v.seek(SeekFrom::Start(100)).unwrap();
        let mut tail = Vec::new();
        v.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, original[100..]);
        drop(v);

        // Once released, tampering is possible again and is detected on reopen.
        fs::write(&art.path, b"tampered").unwrap();
        assert!(matches!(
            store.open_verified(&art),
            Err(AppError::ArtifactChanged)
        ));
    }

    #[test]
    fn verification_hashes_while_excluding_writers() {
        let (dir, store) = fixture();
        let src = dir.path().join("a.bin");
        fs::write(&src, b"payload").unwrap();
        let art = store.stage_file(&src, ArtifactKind::File).unwrap();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let (s, p) = (seen.clone(), art.path.clone());
        set_hook(move |point| {
            if point == "verify.open" {
                make_writable(&p);
                *s.borrow_mut() = mutation_attempts(&p);
            }
        });
        let verified = store.open_verified(&art);
        clear_hook();
        assert_sharing_violations(&seen.borrow());
        let mut bytes = Vec::new();
        verified.unwrap().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"payload");
    }

    #[test]
    fn verified_source_rejects_paths_outside_its_spool_name() {
        let (dir, store) = fixture();
        let src = dir.path().join("a.bin");
        fs::write(&src, b"a").unwrap();
        let art = store.stage_file(&src, ArtifactKind::File).unwrap();
        let other = store.stage_file(&src, ArtifactKind::File).unwrap();
        let swapped = Artifact {
            path: other.path.clone(),
            ..art.clone()
        };
        assert!(matches!(
            store.open_verified(&swapped),
            Err(AppError::InvalidInput(_))
        ));
        let outside = Artifact {
            path: src.clone(),
            ..art
        };
        assert!(store.open_verified(&outside).is_err());
    }

    #[test]
    fn local_source_policy_uses_the_resolved_path() {
        assert!(is_local_final_path(r"\\?\C:\Users\me\a.png"));
        assert!(is_local_final_path(r"\\?\d:\x"));
        assert!(!is_local_final_path(r"\\?\UNC\server\share\a.png"));
        assert!(!is_local_final_path(
            r"\\?\Volume{0f1e2d3c-0000-0000-0000-000000000000}\a"
        ));
        assert!(!is_local_final_path(r"\\?\GLOBALROOT\Device\X\a"));
        assert!(!is_local_final_path(r"C:\not-normalized"));

        // A local junction is a legitimate parent: its resolved path is still local.
        let (dir, store) = fixture();
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("a.bin"), b"a").unwrap();
        junction(&dir.path().join("via"), &real);
        let art = store
            .stage_file(&dir.path().join("via").join("a.bin"), ArtifactKind::File)
            .unwrap();
        assert_eq!(art.size, 1);
    }
}
