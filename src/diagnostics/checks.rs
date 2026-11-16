//! Read-only aggregate/metadata probes. Never decode media, load settings or
//! history rows, enumerate audio endpoints, or create capture/audio streams.
use super::report::*;
use crate::{
    app::{destinations::DestinationSnapshot, error::AppError},
    persistence::{history::History, settings::ServerOrigin},
    vectr::{client::VectrClient, uploader::ClientFactory, uploader_info::UploaderInfoSupport},
};
use std::{
    fs,
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
    },
    time::{Duration, Instant},
};

pub const SCAN_ENTRY_BUDGET: usize = 512;
const SCAN_TIME_BUDGET: Duration = Duration::from_millis(100);
const SCAN_DEPTH_BUDGET: usize = 8;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(20);
pub struct StorageRoots {
    pub spool: PathBuf,
    pub archive: Option<PathBuf>,
}

pub fn error_code(error: &AppError) -> ErrorCode {
    match error {
        AppError::Io {
            kind: std::io::ErrorKind::NotFound,
            ..
        } => ErrorCode::Missing,
        AppError::Io {
            kind: std::io::ErrorKind::PermissionDenied,
            ..
        } => ErrorCode::PermissionDenied,
        AppError::Io { .. } => ErrorCode::Io,
        AppError::NeedsSetup(_) | AppError::Credential(_) => ErrorCode::NeedsSetup,
        AppError::InvalidInput(_) => ErrorCode::InvalidInput,
        AppError::InUse(_) => ErrorCode::Busy,
        AppError::Capture(_) => ErrorCode::Capture,
        AppError::Encoder(_) => ErrorCode::Encoder,
        AppError::HttpStatus { code: 401, .. } => ErrorCode::Unauthorized,
        AppError::HttpStatus { code: 403, .. } => ErrorCode::Forbidden,
        AppError::HttpStatus { code: 429, .. } => ErrorCode::RateLimited,
        AppError::HttpStatus {
            code: 300..=399, ..
        } => ErrorCode::Redirect,
        AppError::HttpStatus { .. } | AppError::Http(_) => ErrorCode::Http,
        AppError::Tls => ErrorCode::Tls,
        AppError::Network | AppError::RetryExhausted => ErrorCode::Network,
        AppError::Protocol(_) | AppError::Uncertain(_) => ErrorCode::Protocol,
        AppError::Cancelled | AppError::Paused => ErrorCode::Cancelled,
        AppError::Persistence(_) => ErrorCode::Persistence,
        AppError::ArtifactChanged => ErrorCode::Changed,
    }
}
pub fn health_result(result: Result<(), AppError>) -> CheckResult {
    match result {
        Ok(()) => CheckResult {
            stage: StageCode::Health,
            availability: Availability::Available,
            error: None,
        },
        Err(e) => CheckResult {
            stage: StageCode::Health,
            availability: Availability::Unavailable,
            error: Some(error_code(&e)),
        },
    }
}
pub fn uploader_result(result: Result<UploaderInfoSupport, AppError>) -> UploaderCheck {
    let (status, error) = match result {
        Ok(UploaderInfoSupport::Available(_)) => (TokenStatus::Verified, None),
        Ok(UploaderInfoSupport::Unsupported404) => (TokenStatus::UnverifiedManualDomain, None),
        Err(e) => (
            if matches!(e, AppError::NeedsSetup(_) | AppError::Credential(_)) {
                TokenStatus::NeedsSetup
            } else {
                TokenStatus::Failed
            },
            Some(error_code(&e)),
        ),
    };
    UploaderCheck {
        stage: StageCode::Uploader,
        status,
        error,
    }
}

/// Each probe retains its own result. Neither a health failure nor a credential
/// failure discards the other probe's successfully diagnosed outcome.
pub async fn test_connection(
    health: Result<HealthClient, AppError>,
    client: Result<VectrClient, AppError>,
) -> ConnectionCheck {
    test_connection_cancellable(health, client, tokio_util::sync::CancellationToken::new()).await
}

pub async fn test_connection_cancellable(
    health: Result<HealthClient, AppError>,
    client: Result<VectrClient, AppError>,
    cancel: tokio_util::sync::CancellationToken,
) -> ConnectionCheck {
    let (health, uploader) = tokio::join!(
        async {
            match health {
                Ok(client) => {
                    tokio::select! {biased; _=cancel.cancelled()=>Err(AppError::Cancelled),result=client.check()=>result}
                }
                Err(e) => Err(e),
            }
        },
        async {
            match client {
                Ok(client) => {
                    tokio::select! {biased; _=cancel.cancelled()=>Err(AppError::Cancelled),result=tokio::time::timeout(CONNECTION_TIMEOUT,client.uploader_info())=>result.unwrap_or(Err(AppError::Network))}
                }
                Err(e) => Err(e),
            }
        }
    );
    ConnectionCheck {
        health: health_result(health),
        uploader: uploader_result(uploader),
    }
}

/// Separate unauthenticated transport: no token, cookies, proxy credentials,
/// domain override, automatic redirect or retry. Fixture CA trust is scoped.
pub struct HealthClient {
    origin: ServerOrigin,
    http: reqwest::Client,
}
impl HealthClient {
    pub fn new(origin: ServerOrigin) -> Result<Self, AppError> {
        Self::build(origin, None)
    }
    pub fn with_root_certificate(
        origin: ServerOrigin,
        root: reqwest::Certificate,
    ) -> Result<Self, AppError> {
        Self::build(origin, Some(root))
    }
    fn build(origin: ServerOrigin, root: Option<reqwest::Certificate>) -> Result<Self, AppError> {
        Ok(Self {
            origin,
            http: crate::vectr::client::build_http(root)?,
        })
    }
    pub async fn check(&self) -> Result<(), AppError> {
        // Read neither response text nor headers into any diagnostic DTO.
        let response = self
            .http
            .get(format!("{}/healthz", self.origin.as_str()))
            .header(reqwest::header::CACHE_CONTROL, "no-store")
            .timeout(CONNECTION_TIMEOUT)
            .send()
            .await
            .map_err(crate::vectr::client::transport_error)?;
        let code = response.status().as_u16();
        if code != 200 {
            return Err(AppError::HttpStatus {
                code,
                retry_after: None,
            });
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Health {
            ok: bool,
        }
        let body = crate::vectr::client::bounded_body(response).await?;
        let response: Health = serde_json::from_slice(&body)
            .map_err(|_| AppError::Protocol("malformed health response"))?;
        if response.ok {
            Ok(())
        } else {
            Err(AppError::Protocol("health response is not healthy"))
        }
    }
}

pub fn collect(history: &History, roots: &StorageRoots, destinations: u32) -> DiagnosticSnapshot {
    let mut snapshot = DiagnosticSnapshot {
        destinations,
        ..Default::default()
    };
    match history.diagnostic_counts() {
        Ok((pending, completed)) => {
            snapshot.pending_jobs = pending;
            snapshot.completed_jobs = completed;
            snapshot.history = CheckResult {
                stage: StageCode::History,
                availability: Availability::Available,
                error: None,
            };
        }
        Err(e) => {
            snapshot.history = CheckResult {
                stage: StageCode::History,
                availability: Availability::Unavailable,
                error: Some(error_code(&e)),
            }
        }
    }
    // Secondary scan failure cannot discard an already available queue summary.
    snapshot.storage = scan_storage(roots);
    snapshot
}

struct Budget {
    remaining: usize,
    started: Instant,
}
impl Budget {
    fn exhausted(&self) -> bool {
        self.remaining == 0 || self.started.elapsed() >= SCAN_TIME_BUDGET
    }
}
fn partial(out: &mut StorageSummary, code: ErrorCode) {
    out.partial = true;
    out.errors += 1;
    out.error.get_or_insert(code);
}
pub fn scan_storage(roots: &StorageRoots) -> [StorageSummary; 2] {
    let mut budget = Budget {
        remaining: SCAN_ENTRY_BUDGET,
        started: Instant::now(),
    };
    let mut out = [
        StorageSummary::unprobed(StorageLabel::Spool),
        StorageSummary::unprobed(StorageLabel::Archive),
    ];
    for (summary, path) in out
        .iter_mut()
        .zip([Some(roots.spool.as_path()), roots.archive.as_deref()])
    {
        match path {
            Some(path) => scan_dir(path, 0, &mut budget, summary),
            None => {
                summary.availability = Availability::Unavailable;
                partial(summary, ErrorCode::Missing);
            }
        }
    }
    out
}
fn scan_dir(path: &Path, depth: usize, budget: &mut Budget, out: &mut StorageSummary) {
    if budget.exhausted() || depth >= SCAN_DEPTH_BUDGET {
        partial(out, ErrorCode::BudgetExceeded);
        return;
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) => {
            out.availability = Availability::Unavailable;
            partial(out, error_code(&AppError::io("storage metadata", &e)));
            return;
        }
    };
    if metadata.file_attributes() & 0x400 != 0 {
        if depth == 0 {
            out.availability = Availability::Unavailable;
        }
        partial(out, ErrorCode::ReparsePoint);
        return;
    }
    let entries = match fs::read_dir(path) {
        Ok(entries) => {
            out.availability = Availability::Available;
            entries
        }
        Err(e) => {
            out.availability = Availability::Unavailable;
            partial(out, error_code(&AppError::io("storage directory", &e)));
            return;
        }
    };
    // The iterator and recursion are bounded; file data is never opened.
    for entry in entries {
        if budget.exhausted() {
            partial(out, ErrorCode::BudgetExceeded);
            return;
        }
        budget.remaining -= 1;
        out.entries += 1;
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                partial(out, error_code(&AppError::io("storage entry", &e)));
                continue;
            }
        };
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(meta) => meta,
            Err(e) => {
                partial(out, error_code(&AppError::io("storage entry", &e)));
                continue;
            }
        };
        if metadata.file_attributes() & 0x400 != 0 {
            partial(out, ErrorCode::ReparsePoint);
            continue;
        }
        if metadata.is_file() {
            out.files += 1;
            out.bytes = out.bytes.saturating_add(metadata.len());
        } else if metadata.is_dir() {
            scan_dir(&entry.path(), depth + 1, budget, out);
        }
    }
}

/// Two independent single-flight gates bound diagnostic workers per controller.
/// They own no capture/queue state and never send a global application error.
#[derive(Default)]
pub struct DiagnosticService {
    refresh: Arc<AtomicBool>,
    connection: Arc<AtomicBool>,
}
struct Reset(Arc<AtomicBool>);
impl Drop for Reset {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
impl DiagnosticService {
    pub fn refresh(
        &self,
        history: Arc<History>,
        roots: StorageRoots,
        destinations: u32,
        reply: SyncSender<DiagnosticSnapshot>,
    ) {
        let failed = DiagnosticSnapshot {
            history: CheckResult {
                stage: StageCode::History,
                availability: Availability::Unavailable,
                error: Some(ErrorCode::WorkerUnavailable),
            },
            ..Default::default()
        };
        if self.refresh.swap(true, Ordering::AcqRel) {
            let _ = reply.try_send(failed);
            return;
        }
        let reset = Reset(self.refresh.clone());
        let fallback = reply.clone();
        if std::thread::Builder::new()
            .name("diagnostic-storage".into())
            .spawn(move || {
                let _reset = reset;
                let snapshot = collect(&history, &roots, destinations);
                let _ = reply.try_send(snapshot);
            })
            .is_err()
        {
            let _ = fallback.try_send(failed);
        }
    }
    pub fn connection(
        &self,
        origin: ServerOrigin,
        destination: Result<DestinationSnapshot, AppError>,
        factory: Arc<dyn ClientFactory>,
        cancel: tokio_util::sync::CancellationToken,
        reply: SyncSender<ConnectionCheck>,
    ) {
        let failed = ConnectionCheck {
            health: health_result(Err(AppError::InUse("diagnostic worker unavailable"))),
            uploader: uploader_result(Err(AppError::InUse("diagnostic worker unavailable"))),
        };
        if self.connection.swap(true, Ordering::AcqRel) {
            let _ = reply.try_send(failed);
            return;
        }
        let reset = Reset(self.connection.clone());
        let fallback = reply.clone();
        if std::thread::Builder::new()
            .name("diagnostic-connection".into())
            .spawn(move || {
                let _reset = reset;
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                let result = match runtime {
                    Ok(runtime) => runtime.block_on(async {
                        let health = HealthClient::new(origin);
                        let client = destination.and_then(|d| factory.client(&d));
                        test_connection_cancellable(health, client, cancel).await
                    }),
                    Err(_) => failed,
                };
                let _ = reply.try_send(result);
            })
            .is_err()
        {
            let _ = fallback.try_send(failed);
        }
    }
}
