use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use uuid::Uuid;

use crate::app::error::AppError;
use crate::app::types::{Artifact, ArtifactId, ArtifactKind};
use crate::persistence::{
    PARTIAL_PREFIX, copy_hashing, ensure_free_space, fingerprint, mark_readonly, open_no_follow,
    remove_owned_file,
};

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
    /// SHA-256. The original is only read. A source that changes while being
    /// copied is refused instead of producing inconsistent bytes.
    pub fn stage_file(&self, source: &Path, kind: ArtifactKind) -> Result<Artifact, AppError> {
        const NOT_REGULAR: AppError =
            AppError::InvalidInput("only regular local files can be uploaded");
        let display_name = display_name_of(source)?;
        check_source_path(source)?;
        let meta = fs::symlink_metadata(source)
            .map_err(|e| AppError::io("reading the source file", &e))?;
        if !meta.is_file() {
            return Err(NOT_REGULAR);
        }
        let mut src = open_no_follow(source).map_err(|e| match e.kind() {
            io::ErrorKind::InvalidInput => NOT_REGULAR,
            _ => AppError::io("opening the source file", &e),
        })?;
        let before = src
            .metadata()
            .map_err(|e| AppError::io("reading the source file", &e))?;
        let size = before.len();

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
        let after = src.metadata().map_err(|e| AppError::io(STAGE, &e))?;
        if copied != size || after.len() != size || after.modified().ok() != before.modified().ok()
        {
            return Err(AppError::InvalidInput(
                "the file changed while it was being staged; try again",
            ));
        }
        tmp.as_file()
            .sync_all()
            .map_err(|e| AppError::io(STAGE, &e))?;

        let id = ArtifactId::new_random();
        let path = self
            .root
            .join(format!("{}.{SPOOL_EXTENSION}", id.0.simple()));
        tmp.persist_noclobber(&path)
            .map_err(|e| AppError::io(STAGE, &e.error))?;
        mark_readonly(&path).map_err(|e| AppError::io(STAGE, &e))?;
        Ok(Artifact {
            id,
            path,
            display_name,
            kind,
            size,
            sha256,
        })
    }

    /// Confirms the staged bytes still match what was recorded at staging time.
    pub fn verify(&self, artifact: &Artifact) -> Result<(), AppError> {
        if artifact.path.parent() != Some(self.root.as_path()) {
            return Err(AppError::InvalidInput(
                "artifact is not in this staging folder",
            ));
        }
        match fingerprint(&artifact.path) {
            Ok((size, hash)) if size == artifact.size && hash == artifact.sha256 => Ok(()),
            Ok(_) => Err(AppError::ArtifactChanged),
            Err(e) if e.kind() == io::ErrorKind::InvalidInput => Err(AppError::ArtifactChanged),
            Err(e) => Err(AppError::io("verifying the staged file", &e)),
        }
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
            if orphan {
                remove_owned_file(&entry.path()).map_err(|e| AppError::io(STAGE, &e))?;
                removed += 1;
            }
        }
        Ok(removed)
    }
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
