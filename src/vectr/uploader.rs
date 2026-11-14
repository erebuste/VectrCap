use super::{
    client::VectrClient,
    protocol::{self, UploadStatus},
    retry,
};
use crate::{
    app::{
        destinations::DestinationSnapshot,
        error::AppError,
        types::{JobId, UploadReceipt},
    },
    persistence::{
        artifacts::{ArtifactStore, VerifiedSource},
        history::{History, JobStatus, UploadJob},
    },
};
use bytes::Bytes;
use std::{
    io::{Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{Mutex, Notify, mpsc::Sender},
    time::Instant,
};
use tokio_util::sync::CancellationToken;
/// Implementations must resolve the exact destination/origin/generation binding.
/// A missing credential is NeedsSetup, never a fallback to another uploader.
pub trait ClientFactory: Send + Sync {
    fn client(&self, destination: &DestinationSnapshot) -> Result<VectrClient, AppError>;
}
impl<F> ClientFactory for F
where
    F: Fn(&DestinationSnapshot) -> Result<VectrClient, AppError> + Send + Sync,
{
    fn client(&self, destination: &DestinationSnapshot) -> Result<VectrClient, AppError> {
        self(destination)
    }
}
#[derive(Debug, Clone)]
pub enum UploadEvent {
    Recovered {
        job: JobId,
        attempt: u32,
        acknowledged: u64,
    },
    Acknowledged {
        job: JobId,
        attempt: u32,
        index: u32,
        bytes: u64,
    },
    Payload {
        job: JobId,
        attempt: u32,
        bytes: u64,
        elapsed: Duration,
        started: Instant,
    },
    Completed {
        job: JobId,
        attempt: u32,
        receipt: UploadReceipt,
    },
}
pub struct Uploader {
    pub(crate) history: Arc<History>,
    store: Arc<ArtifactStore>,
    factory: Arc<dyn ClientFactory>,
    worker: Mutex<()>,
    pub(crate) controls: Notify,
}
impl Uploader {
    pub fn new(
        history: Arc<History>,
        store: Arc<ArtifactStore>,
        factory: Arc<dyn ClientFactory>,
    ) -> Self {
        Self {
            history,
            store,
            factory,
            worker: Mutex::new(()),
            controls: Notify::new(),
        }
    }
    pub fn notify_controls(&self) {
        self.controls.notify_waiters();
    }
    pub async fn run(
        &self,
        job: UploadJob,
        cancel: CancellationToken,
        events: Sender<UploadEvent>,
    ) -> Result<UploadReceipt, AppError> {
        let _worker = self
            .worker
            .try_lock()
            .map_err(|_| AppError::InUse("upload worker is already active"))?;
        let mut job = self
            .history
            .job(job.id)?
            .ok_or(AppError::InvalidInput("unknown upload job"))?;
        if let JobStatus::Completed(r) = job.status {
            return Ok(r);
        }
        if job.status == JobStatus::Uncertain && job.session.is_none() {
            return Err(AppError::Uncertain(
                "resolve previous submission before restart",
            ));
        }
        self.checkpoint(job.id, &cancel)?;
        // QueueController may already have started this exact durable attempt.
        if job.status != JobStatus::Running {
            self.history.transition(job.id, JobStatus::Running)?;
            job = self
                .history
                .job(job.id)?
                .ok_or(AppError::Persistence("upload job disappeared"))?;
        }
        let result = self.consume(&mut job, &cancel, &events).await;
        match result {
            Ok(receipt) => {
                self.history
                    .record_receipt(job.id, job.attempt, job.session, &receipt)?;
                // Ancillary retry cleanup cannot invalidate a durable receipt.
                let _ = self.history.clear_upload_retry(job.id);
                let _ = events.try_send(UploadEvent::Completed {
                    job: job.id,
                    attempt: job.attempt,
                    receipt: receipt.clone(),
                });
                Ok(receipt)
            }
            Err(error) => {
                let current = self
                    .history
                    .job(job.id)?
                    .ok_or(AppError::Persistence("upload job disappeared"))?;
                if let JobStatus::Completed(receipt) = current.status {
                    return Ok(receipt);
                }
                if current.status == JobStatus::Running {
                    let status = match error {
                        AppError::Paused => {
                            if job.session.is_some() {
                                JobStatus::Interrupted
                            } else {
                                JobStatus::Pending
                            }
                        }
                        AppError::Cancelled => JobStatus::Cancelled,
                        AppError::Uncertain(_) => JobStatus::Uncertain,
                        _ => JobStatus::Failed {
                            code: retry::failure_code(&error).into(),
                        },
                    };
                    self.history.transition(job.id, status)?;
                }
                Err(error)
            }
        }
    }
    fn checkpoint(&self, id: JobId, cancel: &CancellationToken) -> Result<(), AppError> {
        let job = self
            .history
            .job(id)?
            .ok_or(AppError::InvalidInput("unknown upload job"))?;
        if cancel.is_cancelled() || job.status == JobStatus::Cancelled {
            return Err(AppError::Cancelled);
        }
        if job.paused || self.history.queue_paused()? {
            return Err(AppError::Paused);
        }
        Ok(())
    }
    async fn consume(
        &self,
        job: &mut UploadJob,
        cancel: &CancellationToken,
        events: &Sender<UploadEvent>,
    ) -> Result<UploadReceipt, AppError> {
        // This same guard stays alive through all reads, requests and retries.
        // Never verify then reopen the path; never touch or convert user originals.
        let store = self.store.clone();
        let artifact = job.artifact.clone();
        let mut source = tokio::task::spawn_blocking(move || store.open_verified(&artifact))
            .await
            .map_err(|_| AppError::Io {
                stage: "opening upload source",
                kind: std::io::ErrorKind::Other,
            })??;
        let d = &job.destination;
        if d.credential.destination() != d.id || d.credential.origin() != &d.origin {
            return Err(AppError::Credential("destination binding mismatch"));
        }
        let client = self.factory.client(d)?;
        if client.origin() != &d.origin {
            return Err(AppError::Credential("credential resolver changed origin"));
        }
        if let Some(deadline) = self.history.upload_retry_deadline(job.id)? {
            self.wait_deadline(job, deadline, cancel).await?;
        }
        self.checkpoint(job.id, cancel)?;
        if source.is_empty() {
            return match client
                .upload_empty(&job.artifact.display_name, d.link_domain.as_ref())
                .await
            {
                Ok(r) => Ok(r),
                Err(error) => {
                    self.rejected_creation_cooldown(job, "empty", &error)?;
                    Err(creation_error(error))
                }
            };
        }
        let (chunk, received) = if let Some(id) = job.session {
            match self.safe_status(&client, job, id, cancel).await? {
                UploadStatus::Done(receipt) => return Ok(receipt),
                UploadStatus::Pending {
                    chunk_bytes,
                    received,
                } => (chunk_bytes, received),
            }
        } else {
            let s = match client
                .start(
                    &job.artifact.display_name,
                    source.len(),
                    d.link_domain.as_ref(),
                )
                .await
            {
                Ok(s) => s,
                Err(error) => {
                    self.rejected_creation_cooldown(job, "create", &error)?;
                    return Err(creation_error(error));
                }
            };
            self.history
                .set_session(job.id, s.id)
                .map_err(|_| AppError::Uncertain("created session could not be recorded"))?;
            job.session = Some(s.id);
            (s.chunk_bytes, Vec::new())
        };
        let id = job
            .session
            .ok_or(AppError::Protocol("missing upload session"))?;
        let count = protocol::piece_count(source.len(), chunk)?;
        protocol::validate_received(&received, count)?;
        let acknowledged = received.iter().try_fold(0u64, |sum, &index| {
            sum.checked_add(piece_len(source.len(), chunk, index))
                .ok_or(AppError::Protocol("progress overflow"))
        })?;
        self.emit(
            job,
            cancel,
            events,
            UploadEvent::Recovered {
                job: job.id,
                attempt: job.attempt,
                acknowledged,
            },
        )
        .await?;
        for index in 0..count {
            if received.binary_search(&index).is_ok() {
                continue;
            }
            self.checkpoint(job.id, cancel)?;
            let (returned, payload) = tokio::task::spawn_blocking(move || {
                let payload = read_piece(&mut source, chunk, index);
                (source, payload)
            })
            .await
            .map_err(|_| AppError::Io {
                stage: "reading upload source",
                kind: std::io::ErrorKind::Other,
            })?;
            source = returned;
            let payload = payload?;
            let operation = format!("piece.{index}");
            loop {
                self.before_retry(job, &operation, cancel).await?;
                self.checkpoint(job.id, cancel)?;
                let start = Instant::now();
                let meter = Arc::new(AtomicU64::new(0));
                let result = client
                    .put_piece_observed(id, index, payload.clone(), meter.clone())
                    .await;
                let _ = events.try_send(UploadEvent::Payload {
                    job: job.id,
                    attempt: job.attempt,
                    bytes: meter.load(Ordering::Relaxed),
                    elapsed: start.elapsed(),
                    started: start,
                });
                match result {
                    Ok(()) => {
                        self.history.clear_operation_retry(job.id, &operation)?;
                        self.emit(
                            job,
                            cancel,
                            events,
                            UploadEvent::Acknowledged {
                                job: job.id,
                                attempt: job.attempt,
                                index,
                                bytes: payload.len() as u64,
                            },
                        )
                        .await?;
                        break;
                    }
                    Err(e) => self.schedule_retry(job, &operation, &e)?,
                }
            }
        }
        loop {
            self.before_retry(job, "complete", cancel).await?;
            self.checkpoint(job.id, cancel)?;
            match client.complete(id).await {
                Ok(receipt) => return Ok(receipt),
                Err(error) => {
                    if !retry::eligible(&error) {
                        return Err(error);
                    }
                    self.schedule_retry(job, "complete", &error)?;
                    if matches!(error, AppError::HttpStatus { code: 429, .. }) {
                        self.before_retry(job, "complete", cancel).await?;
                    }
                    // Resolve remote success before any completion resubmission.
                    // Do not overwrite the durable completion retry slot on status.
                    self.checkpoint(job.id, cancel)?;
                    match self.safe_status(&client, job, id, cancel).await? {
                        UploadStatus::Done(receipt) => return Ok(receipt),
                        UploadStatus::Pending {
                            received,
                            chunk_bytes,
                        } => {
                            if chunk_bytes != chunk {
                                return Err(AppError::Protocol("session chunk size changed"));
                            }
                            protocol::validate_received(&received, count)?;
                            if received.len() != count as usize {
                                return Err(AppError::Protocol("completed transfer lost pieces"));
                            }
                        }
                    }
                }
            }
        }
    }
    async fn safe_status(
        &self,
        client: &VectrClient,
        job: &UploadJob,
        id: uuid::Uuid,
        cancel: &CancellationToken,
    ) -> Result<UploadStatus, AppError> {
        loop {
            // Preserve a pending piece/complete retry across pause/restart; the
            // mandatory status query is read-only, so it must not reset that slot.
            self.before_retry(job, "status", cancel).await?;
            self.checkpoint(job.id, cancel)?;
            match client.status(id).await {
                Ok(status) => {
                    if matches!(status, UploadStatus::Pending { .. }) {
                        self.history.clear_operation_retry(job.id, "status")?;
                    }
                    return Ok(status);
                }
                Err(e) => {
                    self.schedule_retry(job, "status", &e)?;
                }
            }
        }
    }
    fn schedule_retry(
        &self,
        job: &UploadJob,
        operation: &str,
        error: &AppError,
    ) -> Result<(), AppError> {
        if !retry::eligible(error) {
            return Err(error.clone());
        }
        let count = self
            .history
            .upload_retry(job.id, operation)?
            .map_or(1, |(n, _)| n.saturating_add(1));
        let deadline = unix_ms()?
            .checked_add(
                i64::try_from(retry::delay(count, error).as_millis())
                    .map_err(|_| AppError::Protocol("retry delay exceeds clock range"))?,
            )
            .ok_or(AppError::Protocol("retry delay exceeds clock range"))?;
        self.history
            .set_upload_retry(job.id, operation, count.min(5), deadline)?;
        if count > retry::RETRIES {
            return Err(error.clone());
        }
        Ok(())
    }
    async fn emit(
        &self,
        job: &UploadJob,
        cancel: &CancellationToken,
        events: &Sender<UploadEvent>,
        event: UploadEvent,
    ) -> Result<(), AppError> {
        loop {
            let notified = self.controls.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            self.checkpoint(job.id, cancel)?;
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(AppError::Cancelled),
                _ = &mut notified => continue,
                permit = events.reserve() => {
                    if let Ok(permit) = permit {
                        permit.send(event);
                    }
                    return Ok(());
                }
            }
        }
    }
    fn rejected_creation_cooldown(
        &self,
        job: &UploadJob,
        operation: &str,
        error: &AppError,
    ) -> Result<(), AppError> {
        if let AppError::HttpStatus {
            code: 429,
            retry_after: Some(delay),
        } = error
        {
            let deadline = unix_ms()?
                .checked_add(
                    i64::try_from(delay.as_millis())
                        .map_err(|_| AppError::Protocol("retry delay exceeds clock range"))?,
                )
                .ok_or(AppError::Protocol("retry delay exceeds clock range"))?;
            self.history
                .set_upload_retry(job.id, operation, 1, deadline)?;
        }
        Ok(())
    }
    async fn wait_deadline(
        &self,
        job: &UploadJob,
        deadline: i64,
        cancel: &CancellationToken,
    ) -> Result<(), AppError> {
        loop {
            let notified = self.controls.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            self.checkpoint(job.id, cancel)?;
            let delay = Duration::from_millis(deadline.saturating_sub(unix_ms()?).max(0) as u64);
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(AppError::Cancelled),
                _ = &mut notified => continue,
                _ = tokio::time::sleep(delay) => return self.checkpoint(job.id,cancel),
            }
        }
    }
    async fn before_retry(
        &self,
        job: &UploadJob,
        operation: &str,
        cancel: &CancellationToken,
    ) -> Result<(), AppError> {
        loop {
            let notified = self.controls.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            self.checkpoint(job.id, cancel)?;
            if let Some((n, deadline)) = self.history.upload_retry(job.id, operation)? {
                if n > retry::RETRIES {
                    return Err(AppError::Http("retry budget exhausted"));
                }
                let delay =
                    Duration::from_millis(deadline.saturating_sub(unix_ms()?).max(0) as u64);
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(AppError::Cancelled),
                    _ = &mut notified => continue,
                    _ = tokio::time::sleep(delay) => {},
                }
            }
            return self.checkpoint(job.id, cancel);
        }
    }
}
fn creation_error(error: AppError) -> AppError {
    match error {
        AppError::Tls | AppError::Credential(_) | AppError::InvalidInput(_) => error,
        AppError::HttpStatus {
            code: 400..=499, ..
        } => error,
        _ => AppError::Uncertain("submission outcome is unknown; explicit resolution required"),
    }
}
fn unix_ms() -> Result<i64, AppError> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AppError::InvalidInput("system clock predates epoch"))?
            .as_millis(),
    )
    .map_err(|_| AppError::InvalidInput("system clock exceeds supported range"))
}
fn piece_len(size: u64, chunk: u64, index: u32) -> u64 {
    chunk.min(size - u64::from(index) * chunk)
}
fn read_piece(source: &mut VerifiedSource, chunk: u64, index: u32) -> Result<Bytes, AppError> {
    let offset = u64::from(index)
        .checked_mul(chunk)
        .ok_or(AppError::Protocol("piece offset overflow"))?;
    if offset >= source.len() {
        return Err(AppError::Protocol("piece index exceeds source"));
    }
    source
        .seek(SeekFrom::Start(offset))
        .map_err(|e| AppError::io("seeking verified upload source", &e))?;
    let mut bytes = vec![0; piece_len(source.len(), chunk, index) as usize];
    source
        .read_exact(&mut bytes)
        .map_err(|e| AppError::io("reading verified upload source", &e))?;
    Ok(Bytes::from(bytes))
}
