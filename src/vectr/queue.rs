use super::{
    protocol::{MAX_CHUNK, MAX_PIECES},
    uploader::{UploadEvent, Uploader},
};
use crate::{
    app::{
        error::AppError,
        types::{JobId, UploadReceipt},
    },
    persistence::history::{History, JobStatus, UploadJob},
};
use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Duration,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
pub const CAPACITY: usize = 32;
pub fn command_channel() -> (mpsc::Sender<QueueCommand>, mpsc::Receiver<QueueCommand>) {
    mpsc::channel(CAPACITY)
}
#[derive(Debug)]
pub enum QueueCommand {
    Admit(JobId),
    PauseJob(JobId),
    ResumeJob(JobId),
    PauseAll,
    ResumeAll,
    RetryEligible,
    Cancel(JobId),
    Restart(JobId),
    Settled {
        job: JobId,
        attempt: u32,
        result: Result<UploadReceipt, AppError>,
    },
}
#[derive(Debug)]
pub enum QueueEffect {
    Start(Box<UploadJob>),
    Pausing(JobId),
    Paused(JobId),
    CancelActive(JobId),
    Changed(JobId),
    Restarted {
        previous: JobId,
        replacement: JobId,
    },
    Progress {
        job: JobId,
        acknowledged: u64,
        speed: Option<f64>,
    },
    Error(AppError),
}
struct Active {
    job: JobId,
    attempt: u32,
    cancelled: bool,
}
/// One actor owns command/result ordering. SQLite is authoritative; effects are
/// notifications after persistence. A Start effect is one reserved durable attempt.
pub struct QueueController {
    history: Arc<History>,
    jobs: VecDeque<JobId>,
    active: Option<Active>,
    stats: HashMap<JobId, TransferStats>,
}
impl QueueController {
    pub fn new(history: Arc<History>) -> Result<Self, AppError> {
        let recovered = history.recover_jobs()?;
        if recovered.len() > CAPACITY {
            return Err(AppError::InUse(
                "queue capacity exceeded; resolve existing jobs",
            ));
        }
        Ok(Self {
            history,
            jobs: recovered.into_iter().map(|j| j.id).collect(),
            active: None,
            stats: HashMap::new(),
        })
    }
    pub fn handle(&mut self, command: QueueCommand) -> Vec<QueueEffect> {
        let mut effects = Vec::new();
        if let Err(error) = self.apply(command, &mut effects) {
            effects.push(QueueEffect::Error(error));
        }
        if let Err(error) = self.schedule(&mut effects) {
            effects.push(QueueEffect::Error(error));
        }
        effects
    }
    fn job(&self, id: JobId) -> Result<UploadJob, AppError> {
        self.history
            .job(id)?
            .ok_or(AppError::InvalidInput("unknown queue job"))
    }
    fn apply(
        &mut self,
        command: QueueCommand,
        effects: &mut Vec<QueueEffect>,
    ) -> Result<(), AppError> {
        match command {
            QueueCommand::Admit(id) => {
                let job = self.job(id)?;
                if !job.status.is_terminal() && !self.jobs.contains(&id) {
                    if self.jobs.len() >= CAPACITY {
                        return Err(AppError::InUse("upload queue is full"));
                    }
                    self.jobs.push_back(id);
                }
            }
            QueueCommand::PauseJob(id) => {
                self.history.set_job_paused(id, true)?;
                self.stats.entry(id).or_default().pause();
                effects.push(if self.active.as_ref().is_some_and(|a| a.job == id) {
                    QueueEffect::Pausing(id)
                } else {
                    QueueEffect::Paused(id)
                });
            }
            QueueCommand::ResumeJob(id) => {
                self.history.set_job_paused(id, false)?;
                self.stats.entry(id).or_default().pause();
                effects.push(QueueEffect::Changed(id));
            }
            QueueCommand::PauseAll => {
                self.history.set_queue_paused(true)?;
                for stats in self.stats.values_mut() {
                    stats.pause();
                }
                if let Some(a) = &self.active {
                    effects.push(QueueEffect::Pausing(a.job));
                }
            }
            QueueCommand::ResumeAll => {
                self.history.set_queue_paused(false)?;
                for stats in self.stats.values_mut() {
                    stats.pause();
                }
            }
            QueueCommand::RetryEligible => {
                for &id in &self.jobs {
                    if matches!(self.job(id)?.status,JobStatus::Failed{code} if code=="transient"||code=="rate_limited")
                    {
                        self.history.reset_upload_retry(id)?;
                        self.history.transition(id, JobStatus::Pending)?;
                    }
                }
            }
            QueueCommand::Cancel(id) => {
                if let Some(a) = self.active.as_mut().filter(|a| a.job == id) {
                    a.cancelled = true;
                    effects.push(QueueEffect::CancelActive(id));
                } else {
                    let job = self.job(id)?;
                    if !job.status.is_terminal() {
                        self.history.transition(id, JobStatus::Cancelled)?;
                    }
                    self.jobs.retain(|j| *j != id);
                    self.stats.remove(&id);
                    effects.push(QueueEffect::Changed(id));
                }
            }
            QueueCommand::Restart(id) => {
                if self.active.as_ref().is_some_and(|a| a.job == id) {
                    return Err(AppError::InUse("wait for the current request to settle"));
                }
                let job = self.job(id)?;
                if !matches!(
                    job.status,
                    JobStatus::Failed { .. }
                        | JobStatus::Uncertain
                        | JobStatus::Interrupted
                        | JobStatus::Cancelled
                ) {
                    return Err(AppError::InvalidInput("job does not need restart"));
                }
                if self.jobs.len() >= CAPACITY && !self.jobs.contains(&id) {
                    return Err(AppError::InUse("upload queue is full"));
                }
                let replacement = self.history.restart_upload(id)?;
                self.jobs.retain(|j| *j != id);
                self.jobs.push_back(replacement.id);
                self.stats.remove(&id);
                effects.push(QueueEffect::Restarted {
                    previous: id,
                    replacement: replacement.id,
                });
            }
            QueueCommand::Settled {
                job: id,
                attempt,
                result,
            } => {
                let job = self.job(id)?;
                if let Ok(receipt) = &result {
                    self.history
                        .record_receipt(id, attempt, job.session, receipt)?;
                }
                if self
                    .active
                    .as_ref()
                    .is_none_or(|a| a.job != id || a.attempt != attempt)
                {
                    return Ok(());
                }
                let active = self
                    .active
                    .take()
                    .ok_or(AppError::Persistence("missing active queue attempt"))?;
                let current = self.job(id)?;
                if !matches!(current.status, JobStatus::Completed(_)) {
                    if active.cancelled && !current.status.is_terminal() {
                        self.history.transition(id, JobStatus::Cancelled)?;
                    } else if current.status == JobStatus::Running {
                        let to = match result {
                            Err(AppError::Paused) => {
                                if current.session.is_some() {
                                    JobStatus::Interrupted
                                } else {
                                    JobStatus::Pending
                                }
                            }
                            Err(AppError::Cancelled) => JobStatus::Cancelled,
                            Err(AppError::Uncertain(_)) => JobStatus::Uncertain,
                            Err(ref error) => JobStatus::Failed {
                                code: super::retry::failure_code(error).into(),
                            },
                            Ok(_) => return Err(AppError::Persistence("receipt was not recorded")),
                        };
                        self.history.transition(id, to)?;
                    }
                }
                if self.job(id)?.status.is_terminal() {
                    self.jobs.retain(|j| *j != id);
                    self.stats.remove(&id);
                } else {
                    self.stats.entry(id).or_default().pause();
                }
                effects.push(QueueEffect::Changed(id));
                if self.job(id)?.paused || self.history.queue_paused()? {
                    effects.push(QueueEffect::Paused(id));
                }
            }
        }
        Ok(())
    }
    fn schedule(&mut self, effects: &mut Vec<QueueEffect>) -> Result<(), AppError> {
        if self.active.is_some() || self.history.queue_paused()? {
            return Ok(());
        }
        for &id in &self.jobs {
            let job = self.job(id)?;
            if job.paused || !matches!(job.status, JobStatus::Pending | JobStatus::Interrupted) {
                continue;
            }
            self.history.transition(id, JobStatus::Running)?;
            let job = self.job(id)?;
            self.stats.entry(id).or_default();
            self.active = Some(Active {
                job: id,
                attempt: job.attempt,
                cancelled: false,
            });
            effects.push(QueueEffect::Start(Box::new(job)));
            break;
        }
        Ok(())
    }
    pub fn transfer(&mut self, event: UploadEvent, now: Duration) -> Vec<QueueEffect> {
        let (id, attempt) = match &event {
            UploadEvent::Recovered { job, attempt, .. }
            | UploadEvent::Acknowledged { job, attempt, .. }
            | UploadEvent::Payload { job, attempt, .. }
            | UploadEvent::Completed { job, attempt, .. } => (*job, *attempt),
        };
        if self
            .active
            .as_ref()
            .is_none_or(|a| a.job != id || a.attempt != attempt)
        {
            return Vec::new();
        }
        let stats = self.stats.entry(id).or_default();
        let result = match event {
            UploadEvent::Recovered { acknowledged, .. } => {
                stats.ack.clear();
                stats.acknowledged = acknowledged;
                Ok(())
            }
            UploadEvent::Acknowledged { index, bytes, .. } => stats.acknowledge(index, bytes),
            UploadEvent::Payload {
                bytes,
                elapsed,
                started,
                ..
            } => {
                if stats.reset_at.is_some_and(|reset| started < reset) {
                    Ok(())
                } else {
                    stats.payload(now, bytes, elapsed)
                }
            }
            UploadEvent::Completed { .. } => Ok(()),
        };
        match result {
            Ok(()) => vec![QueueEffect::Progress {
                job: id,
                acknowledged: stats.acknowledged(),
                speed: stats.speed(now),
            }],
            Err(e) => vec![QueueEffect::Error(e)],
        }
    }
    /// Single-worker driver. Commands and payload events have bounded channels;
    /// a paused request settles through Uploader before its lease can be reused.
    /// Dropping command senders durably pauses the queue and drains the request.
    pub async fn serve(
        mut self,
        uploader: Arc<Uploader>,
        mut commands: mpsc::Receiver<QueueCommand>,
        notifications: mpsc::Sender<QueueEffect>,
    ) -> Result<(), AppError> {
        if !Arc::ptr_eq(&self.history, &uploader.history) {
            return Err(AppError::InvalidInput(
                "queue and uploader must share history",
            ));
        }
        type Running =
            Pin<Box<dyn Future<Output = (JobId, u32, Result<UploadReceipt, AppError>)> + Send>>;
        let (mut running, mut cancel): (Option<Running>, Option<CancellationToken>) = (None, None);
        let (event_tx, mut event_rx) = mpsc::channel(CAPACITY);
        let origin = tokio::time::Instant::now();
        let mut closed = false;
        let mut effects = Vec::new();
        self.schedule(&mut effects)?;
        loop {
            for effect in effects.drain(..) {
                match &effect {
                    QueueEffect::Start(job) => {
                        if running.is_some() {
                            return Err(AppError::InUse("duplicate worker reservation"));
                        }
                        let job = job.as_ref().clone();
                        let worker = uploader.clone();
                        let token = CancellationToken::new();
                        cancel = Some(token.clone());
                        let events = event_tx.clone();
                        running = Some(Box::pin(async move {
                            let id = job.id;
                            let attempt = job.attempt;
                            let result = worker.run(job, token, events).await;
                            (id, attempt, result)
                        }));
                    }
                    QueueEffect::CancelActive(_) => {
                        if let Some(c) = &cancel {
                            c.cancel();
                        }
                    }
                    _ => {}
                }
                let _ = notifications.try_send(effect);
            }
            uploader.notify_controls();
            if closed && running.is_none() {
                return Ok(());
            }
            tokio::select! {
                command = commands.recv(), if !closed => {
                    effects = if let Some(command) = command {
                        self.handle(command)
                    } else {
                        closed = true;
                        self.handle(QueueCommand::PauseAll)
                    };
                }
                event = event_rx.recv() => {
                    if let Some(event) = event {
                        effects = self.transfer(event,origin.elapsed());
                    }
                }
                settled = async {
                    match &mut running {
                        Some(future) => future.await,
                        None => std::future::pending().await,
                    }
                } => {
                    running = None;
                    cancel = None;
                    effects = self.handle(QueueCommand::Settled {
                        job: settled.0,
                        attempt: settled.1,
                        result: settled.2,
                    });
                }
            }
        }
    }
}
/// Bounded active payload window; paused/stale samples are discarded. Accepted
/// progress is unique server-acknowledged pieces, independently of payload speed.
#[derive(Default)]
pub struct TransferStats {
    ack: HashMap<u32, u64>,
    acknowledged: u64,
    samples: VecDeque<(Duration, u64, Duration)>,
    last: Option<Duration>,
    reset_at: Option<tokio::time::Instant>,
}
impl TransferStats {
    pub fn acknowledge(&mut self, index: u32, bytes: u64) -> Result<(), AppError> {
        if index >= MAX_PIECES || bytes == 0 || bytes > MAX_CHUNK {
            return Err(AppError::Protocol("invalid progress piece"));
        }
        if let Some(old) = self.ack.get(&index) {
            return if *old == bytes {
                Ok(())
            } else {
                Err(AppError::Protocol("piece progress changed size"))
            };
        }
        self.acknowledged = self
            .acknowledged
            .checked_add(bytes)
            .ok_or(AppError::Protocol("progress overflow"))?;
        self.ack.insert(index, bytes);
        Ok(())
    }
    pub fn acknowledged(&self) -> u64 {
        self.acknowledged
    }
    pub fn payload(&mut self, at: Duration, bytes: u64, elapsed: Duration) -> Result<(), AppError> {
        if self.last.is_some_and(|last| at < last) {
            return Err(AppError::InvalidInput("transfer clock moved backward"));
        }
        self.last = Some(at);
        if bytes > MAX_CHUNK {
            return Err(AppError::Protocol("payload sample exceeds bound"));
        }
        if elapsed.is_zero() || bytes == 0 {
            return Ok(());
        }
        self.prune(at);
        if self.samples.len() == 64 {
            self.samples.pop_front();
        }
        self.samples.push_back((at, bytes, elapsed));
        Ok(())
    }
    pub fn pause(&mut self) {
        self.samples.clear();
        self.last = None;
        self.reset_at = Some(tokio::time::Instant::now());
    }
    fn prune(&mut self, now: Duration) {
        while self
            .samples
            .front()
            .is_some_and(|(at, _, _)| now.saturating_sub(*at) > Duration::from_secs(5))
        {
            self.samples.pop_front();
        }
    }
    pub fn speed(&mut self, now: Duration) -> Option<f64> {
        if self.last.is_some_and(|last| now < last) {
            self.pause();
            return None;
        }
        self.prune(now);
        let bytes: u64 = self.samples.iter().map(|(_, b, _)| *b).sum();
        let seconds: f64 = self.samples.iter().map(|(_, _, d)| d.as_secs_f64()).sum();
        if seconds > 0.0 {
            Some(bytes as f64 / seconds)
        } else {
            None
        }
    }
}
