//! Bounded shared collection for Gallery/List. No network or retention side effects.
use super::*;
pub const HISTORY_PAGE_SIZE: usize = 50;
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum HistoryType {
    #[default]
    All,
    Images,
    Recordings,
    Files,
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum HistoryStatus {
    #[default]
    Any,
    Uploaded,
    Failed,
    Waiting,
    Uncertain,
    LocalOnly,
    Cancelled,
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum HistorySort {
    #[default]
    Newest,
    Oldest,
    Name,
    Size,
}
#[derive(Clone, PartialEq, Eq)]
pub struct HistoryCursor {
    pub created_ms: i64,
    pub id: Uuid,
    pub name: String,
    pub size: u64,
}
impl HistoryCursor {
    pub fn at(r: &HistoryRecord) -> Self {
        Self {
            created_ms: r.created_ms,
            id: r.id,
            name: r.name.clone(),
            size: r.size,
        }
    }
}
#[derive(Clone, Default, PartialEq, Eq)]
pub struct HistoryQuery {
    pub search: String,
    pub kind: HistoryType,
    pub favorites_only: bool,
    pub after: Option<HistoryCursor>,
    pub status: HistoryStatus,
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
    pub sort: HistorySort,
}
#[derive(Clone)]
pub struct HistoryRecord {
    pub recording: Option<crate::recording::review::RecordingMetadata>,
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub size: u64,
    pub created_ms: i64,
    pub favorite: bool,
    pub local_available: bool,
    pub retryable: bool,
    pub artifact: Artifact,
    pub job: Option<UploadJob>,
}
#[derive(Clone, Default)]
pub struct HistoryPage {
    pub records: Vec<HistoryRecord>,
    pub has_more: bool,
}
impl History {
    pub(crate) fn artifact_is_recorded(&self, id: ArtifactId) -> Result<bool, AppError> {
        Ok(self.lock()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM artifacts WHERE id=?1)",
            [id.0.to_string()],
            |r| r.get(0),
        )?)
    }

    pub fn browse(&self, q: &HistoryQuery) -> Result<HistoryPage, AppError> {
        if q.search.len() > 256
            || q.search.chars().any(char::is_control)
            || q.after
                .as_ref()
                .is_some_and(|c| c.id.is_nil() || c.name.len() > 4096 || c.size > i64::MAX as u64)
            || q.since_ms.zip(q.until_ms).is_some_and(|(a, b)| a > b)
        {
            return Err(AppError::InvalidInput("invalid history query"));
        }
        let (order, after) = match q.sort {
            HistorySort::Newest => ("h.created_ms DESC,h.id DESC", "(h.created_ms,h.id)<(?5,?4)"),
            HistorySort::Oldest => ("h.created_ms,h.id", "(h.created_ms,h.id)>(?5,?4)"),
            HistorySort::Name => (
                "a.display_name COLLATE NOCASE,h.id",
                "(a.display_name COLLATE NOCASE,h.id)>(?9 COLLATE NOCASE,?4)",
            ),
            HistorySort::Size => ("a.size DESC,h.id DESC", "(a.size,h.id)<(?10,?4)"),
        };
        let kind = match q.kind {
            HistoryType::All => "all",
            HistoryType::Images => "images",
            HistoryType::Recordings => "recordings",
            HistoryType::Files => "files",
        };
        let status = match q.status {
            HistoryStatus::Any => "any",
            HistoryStatus::Uploaded => "completed",
            HistoryStatus::Failed => "failed",
            HistoryStatus::Waiting => "waiting",
            HistoryStatus::Uncertain => "uncertain",
            HistoryStatus::LocalOnly => "local",
            HistoryStatus::Cancelled => "cancelled",
        };
        let sql=format!("SELECT h.id FROM history_items h JOIN artifacts a ON a.id=h.artifact_id
    LEFT JOIN jobs j ON j.id=(SELECT n.id FROM jobs n WHERE n.artifact_id=a.id ORDER BY n.acceptance_ordinal DESC LIMIT 1)
    WHERE (?1='' OR instr(lower(a.display_name),lower(?1))>0 OR instr(a.kind,lower(?1))>0 OR EXISTS(SELECT 1 FROM jobs s WHERE s.artifact_id=a.id AND s.status='completed' AND instr(lower(s.receipt_url),lower(?1))>0))
    AND (?2='all' OR (?2='images' AND (a.kind='screenshot' OR (a.kind='file' AND (lower(a.display_name) GLOB '*.png' OR lower(a.display_name) GLOB '*.jpg' OR lower(a.display_name) GLOB '*.jpeg' OR lower(a.display_name) GLOB '*.bmp' OR lower(a.display_name) GLOB '*.webp')))) OR (?2='recordings' AND (a.kind IN ('gif','mp4') OR (a.kind='file' AND (lower(a.display_name) GLOB '*.gif' OR lower(a.display_name) GLOB '*.mp4')))) OR (?2='files' AND a.kind='file'))
    AND (?3=0 OR h.favorite=1) AND (?4 IS NULL OR {after}) AND (?6 IS NULL OR h.created_ms>=?6) AND (?7 IS NULL OR h.created_ms<=?7)
    AND (?8='any' OR j.status=?8 OR (?8='local' AND j.id IS NULL) OR (?8='waiting' AND j.status IN ('pending','running','interrupted')))
    AND (?9 IS NULL OR 1=1) AND (?10 IS NULL OR 1=1) ORDER BY {order} LIMIT 51");
        let ids = {
            let c = self.lock()?;
            let mut s = c.prepare(&sql)?;
            s.query_map(
                params![
                    q.search,
                    kind,
                    q.favorites_only,
                    q.after.as_ref().map(|c| c.id.to_string()),
                    q.after.as_ref().map(|c| c.created_ms),
                    q.since_ms,
                    q.until_ms,
                    status,
                    q.after.as_ref().map(|c| &c.name),
                    q.after.as_ref().map(|c| c.size as i64)
                ],
                |r| r.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?
        };
        let has_more = ids.len() > HISTORY_PAGE_SIZE;
        let mut records = vec![];
        for id in ids.into_iter().take(HISTORY_PAGE_SIZE) {
            if let Some(row) = self.history_record(parse_uuid(&id)?)? {
                records.push(row);
            }
        }
        Ok(HistoryPage { records, has_more })
    }
    pub fn history_record(&self, id: Uuid) -> Result<Option<HistoryRecord>, AppError> {
        let c = self.lock()?;
        let row=c.query_row("SELECT a.id,a.path,a.display_name,a.kind,a.size,a.sha256,h.created_ms,h.favorite,
   (SELECT j.id FROM jobs j WHERE j.artifact_id=a.id ORDER BY j.acceptance_ordinal DESC LIMIT 1),
   (SELECT e.path FROM archive_entries e WHERE e.artifact_id=a.id AND e.removed_ms IS NULL ORDER BY e.created_ms DESC LIMIT 1)
   FROM history_items h JOIN artifacts a ON a.id=h.artifact_id WHERE h.id=?1",[id.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?,r.get::<_,Vec<u8>>(5)?,r.get::<_,i64>(6)?,r.get::<_,bool>(7)?,r.get::<_,Option<String>>(8)?,r.get::<_,Option<String>>(9)?))).optional()?;
        drop(c);
        let Some((artifact, path, name, kind, size, sha256, created_ms, favorite, job, archive)) =
            row
        else {
            return Ok(None);
        };
        let mut path = PathBuf::from(path);
        if !local_available_path(&path)
            && let Some(archive) = archive
        {
            path = PathBuf::from(archive);
        }
        let artifact = Artifact {
            id: ArtifactId(parse_uuid(&artifact)?),
            path,
            display_name: name.clone(),
            kind: match kind.as_str() {
                "screenshot" => ArtifactKind::Screenshot,
                "gif" => ArtifactKind::Gif,
                "mp4" => ArtifactKind::Mp4,
                "file" => ArtifactKind::File,
                _ => return Err(corrupt()),
            },
            size: u64::try_from(size).map_err(|_| corrupt())?,
            sha256: sha256.try_into().map_err(|_| corrupt())?,
        };
        let local_available = local_available_path(&artifact.path);
        let size = artifact.size;
        let job = job
            .map(|id| self.job(JobId(parse_uuid(&id)?)))
            .transpose()?
            .flatten();
        let retryable = job.as_ref().is_some_and(|j| matches!(&j.status,JobStatus::Failed{code} if matches!(code.as_str(),"transient"|"rate_limited"|"retry_exhausted")) && local_available_path(&j.artifact.path));
        let recording = self.recording_metadata(artifact.id)?;
        Ok(Some(HistoryRecord {
            recording,
            id,
            name,
            kind,
            size,
            created_ms,
            favorite,
            local_available,
            retryable,
            artifact,
            job,
        }))
    }
    pub fn set_favorite(&self, id: Uuid, favorite: bool) -> Result<(), AppError> {
        if id.is_nil()
            || self.lock()?.execute(
                "UPDATE history_items SET favorite=?1 WHERE id=?2",
                params![favorite, id.to_string()],
            )? != 1
        {
            return Err(AppError::InvalidInput("history item no longer available"));
        }
        Ok(())
    }
    /// Removes records only. Job sources, archives, local files and remote files survive.
    pub fn remove_history(&self, ids: &[Uuid]) -> Result<(), AppError> {
        validate_selection(ids)?;
        let mut c = self.lock()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for id in ids {
            let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM history_items h JOIN jobs j ON j.artifact_id=h.artifact_id WHERE h.id=?1 AND j.status NOT IN ('completed','cancelled'))",[id.to_string()],|r|r.get(0))?;
            if active {
                return Err(AppError::InUse(
                    "settle or cancel unfinished uploads before removing history",
                ));
            }
        }
        for id in ids {
            if tx.execute("DELETE FROM history_items WHERE id=?1", [id.to_string()])? != 1 {
                return Err(AppError::InvalidInput("history item no longer available"));
            }
        }
        tx.commit()?;
        Ok(())
    }
    pub fn retry_history_jobs(&self, ids: &[Uuid]) -> Result<Vec<JobId>, AppError> {
        validate_selection(ids)?;
        let mut jobs = vec![];
        for id in ids {
            let row = self
                .history_record(*id)?
                .ok_or(AppError::InvalidInput("history item no longer available"))?;
            if let Some(job) = row.job
                && matches!(&job.status,JobStatus::Failed{code} if matches!(code.as_str(),"transient"|"rate_limited"|"retry_exhausted"))
                && local_available_path(&job.artifact.path)
            {
                jobs.push(job.id);
            }
        }
        Ok(jobs)
    }
}
pub fn validate_selection(ids: &[Uuid]) -> Result<(), AppError> {
    let mut seen = HashSet::new();
    if ids.is_empty() || ids.len() > 100 || ids.iter().any(|id| id.is_nil() || !seen.insert(*id)) {
        return Err(AppError::InvalidInput(
            "select 1 to 100 distinct history items",
        ));
    }
    Ok(())
}

/// Reads only the recorded finalized bytes through a handle denying writers and
/// deletes. Used for local viewing/preview; never follows a remote URL.
/// Does not contact UNC paths, mapped network drives, or directory junctions.
/// Every directory component is opened relative to its checked parent handle.
#[allow(unsafe_code)]
pub struct LocalFile {
    file: std::fs::File,
    _directories: Vec<std::fs::File>,
}
impl LocalFile {
    pub fn metadata(&self) -> std::io::Result<std::fs::Metadata> {
        self.file.metadata()
    }
}
impl std::io::Read for LocalFile {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut self.file, buffer)
    }
}
impl std::io::Seek for LocalFile {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        std::io::Seek::seek(&mut self.file, position)
    }
}
#[allow(unsafe_code)]
pub fn open_local_file(path: &Path) -> Result<LocalFile, AppError> {
    use std::path::{Component, Prefix};
    let mut parts = path.components();
    let drive = match parts.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => d,
            _ => {
                return Err(AppError::InvalidInput(
                    "only local disk files can be previewed",
                ));
            }
        },
        _ => return Err(AppError::InvalidInput("local path must be absolute")),
    };
    if !matches!(parts.next(), Some(Component::RootDir)) {
        return Err(AppError::InvalidInput("local path must be absolute"));
    }
    let root = format!("{}:\\", drive as char);
    let wide: Vec<u16> = root.encode_utf16().chain(Some(0)).collect();
    let kind = unsafe { windows_sys::Win32::Storage::FileSystem::GetDriveTypeW(wide.as_ptr()) };
    if !matches!(kind, 2 | 3 | 5 | 6) {
        return Err(AppError::InvalidInput(
            "network and unknown drives are not previewed",
        ));
    }
    let names = parts
        .map(|part| match part {
            Component::Normal(name) => name
                .to_str()
                .map(str::to_owned)
                .ok_or(AppError::InvalidInput("invalid local filename")),
            _ => Err(AppError::InvalidInput("invalid local path component")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (last, parents) = names
        .split_last()
        .ok_or(AppError::InvalidInput("local file path required"))?;
    let directory = crate::persistence::winfs::open_root(Path::new(&root))
        .map_err(|e| AppError::io("opening local drive", &e))?;
    let resolved = crate::persistence::winfs::final_path(&directory)
        .map_err(|e| AppError::io("checking local drive", &e))?;
    let rest = resolved
        .strip_prefix(r"\\?\")
        .ok_or(AppError::InvalidInput("unrecognized local drive"))?;
    if !matches!(rest.as_bytes(),[drive,b':',b'\\',..]if drive.is_ascii_alphabetic()) {
        return Err(AppError::InvalidInput("network drive is not previewed"));
    }
    let mut directories = vec![directory];
    for name in parents {
        let next =
            crate::persistence::winfs::open_child_dir(directories.last().unwrap(), name, false)
                .map_err(|e| AppError::io("opening local preview folder", &e))?;
        directories.push(next);
    }
    let file = crate::persistence::winfs::open_child_read(directories.last().unwrap(), last)
        .map_err(|e| AppError::io("opening local finalized file", &e))?;
    Ok(LocalFile {
        file,
        _directories: directories,
    })
}
pub fn local_available_path(path: &Path) -> bool {
    open_local_file(path).is_ok()
}
pub fn verified_local(artifact: &Artifact, max_bytes: u64) -> Result<LocalFile, AppError> {
    verified_local_cancellable(
        artifact,
        max_bytes,
        &tokio_util::sync::CancellationToken::new(),
    )
}
pub fn verified_local_cancellable(
    artifact: &Artifact,
    max_bytes: u64,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<LocalFile, AppError> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, SeekFrom};
    if artifact.size > max_bytes {
        return Err(AppError::InvalidInput("file exceeds preview limit"));
    }
    if cancel.is_cancelled() {
        return Err(AppError::Cancelled);
    }
    let mut file = open_local_file(&artifact.path)?;
    if file
        .metadata()
        .map_err(|e| AppError::io("checking finalized file", &e))?
        .len()
        != artifact.size
    {
        return Err(AppError::ArtifactChanged);
    }
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        let read = file
            .read(&mut buffer)
            .map_err(|e| AppError::io("checking finalized bytes", &e))?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(read as u64)
            .ok_or(AppError::ArtifactChanged)?;
        if size > artifact.size {
            return Err(AppError::ArtifactChanged);
        }
        hash.update(&buffer[..read]);
    }
    let hash: [u8; 32] = hash.finalize().into();
    if size != artifact.size || hash != artifact.sha256 {
        return Err(AppError::ArtifactChanged);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|e| AppError::io("reading finalized bytes", &e))?;
    Ok(file)
}

impl History {
    pub fn retry_selected_uploads(&self, ids: &[JobId]) -> Result<(), AppError> {
        if ids.len() > 100 {
            return Err(AppError::InvalidInput("too many retry jobs"));
        }
        let uuids: Vec<_> = ids.iter().map(|id| id.0).collect();
        validate_selection(&uuids)?;
        let mut c = self.lock()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for id in ids {
            if tx.execute("UPDATE jobs SET status='pending',failure_code=NULL,updated_ms=?2 WHERE id=?1 AND status='failed' AND failure_code IN ('transient','rate_limited','retry_exhausted') AND cancel_requested=0",params![id.0.to_string(),now_ms()])?!=1{return Err(AppError::InvalidInput("selected upload is no longer retryable"));}
            tx.execute(
                "UPDATE upload_retry SET failures=0 WHERE job_id=?1",
                [id.0.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
