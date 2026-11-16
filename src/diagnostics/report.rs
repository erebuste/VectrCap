use crate::{app::error::AppError, platform::windows::clipboard::ClipboardWriter};
use serde::Serialize;
use std::{io::Write, path::Path, sync::Arc};

pub const MAX_REPORT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    #[default]
    NotProbed,
    Available,
    Unavailable,
}

/// Closed codes only: AppError's context is deliberately discarded, even if a
/// caller supplied a path, device name or secret in a static context string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Missing,
    PermissionDenied,
    Io,
    Persistence,
    NeedsSetup,
    Unauthorized,
    Forbidden,
    Redirect,
    RateLimited,
    Http,
    Tls,
    Network,
    Protocol,
    Busy,
    Cancelled,
    Capture,
    Encoder,
    Changed,
    InvalidInput,
    BudgetExceeded,
    ReparsePoint,
    WorkerUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StageCode {
    History,
    Storage,
    Health,
    Uploader,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CheckResult {
    pub stage: StageCode,
    pub availability: Availability,
    pub error: Option<ErrorCode>,
}
impl CheckResult {
    pub const fn unprobed(stage: StageCode) -> Self {
        Self {
            stage,
            availability: Availability::NotProbed,
            error: None,
        }
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenStatus {
    #[default]
    NotTested,
    NeedsSetup,
    Verified,
    UnverifiedManualDomain,
    Failed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct UploaderCheck {
    pub stage: StageCode,
    pub status: TokenStatus,
    pub error: Option<ErrorCode>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ConnectionCheck {
    pub health: CheckResult,
    pub uploader: UploaderCheck,
}
impl Default for ConnectionCheck {
    fn default() -> Self {
        Self {
            health: CheckResult::unprobed(StageCode::Health),
            uploader: UploaderCheck {
                stage: StageCode::Uploader,
                status: TokenStatus::NotTested,
                error: None,
            },
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageLabel {
    Spool,
    Archive,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StorageSummary {
    pub label: StorageLabel,
    pub stage: StageCode,
    pub availability: Availability,
    pub entries: u64,
    pub files: u64,
    pub bytes: u64,
    pub partial: bool,
    pub errors: u64,
    pub error: Option<ErrorCode>,
}
impl StorageSummary {
    pub const fn unprobed(label: StorageLabel) -> Self {
        Self {
            label,
            stage: StageCode::Storage,
            availability: Availability::NotProbed,
            entries: 0,
            files: 0,
            bytes: 0,
            partial: false,
            errors: 0,
            error: None,
        }
    }
}

/// No open-ended text, arbitrary JSON, identifiers or raw context can cross
/// this boundary. Unknown hardware remains unprobed rather than being opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DiagnosticSnapshot {
    pub capture: Availability,
    pub audio: Availability,
    pub encoder: Availability,
    pub destinations: u32,
    pub pending_jobs: u64,
    pub completed_jobs: u64,
    pub history: CheckResult,
    pub storage: [StorageSummary; 2],
    pub connection: ConnectionCheck,
}
impl Default for DiagnosticSnapshot {
    fn default() -> Self {
        Self {
            capture: Availability::NotProbed,
            audio: Availability::NotProbed,
            encoder: Availability::NotProbed,
            destinations: 0,
            pending_jobs: 0,
            completed_jobs: 0,
            history: CheckResult::unprobed(StageCode::History),
            storage: [
                StorageSummary::unprobed(StorageLabel::Spool),
                StorageSummary::unprobed(StorageLabel::Archive),
            ],
            connection: ConnectionCheck::default(),
        }
    }
}

/// Dedicated V1 allowlist. Serialize only: there is no diagnostic import API.
/// Never embed Settings, HistoryRecord, AppError or server DTOs here.
#[derive(Serialize)]
pub struct DiagnosticReportV1 {
    version: u8,
    application: &'static str,
    application_version: &'static str,
    snapshot: DiagnosticSnapshot,
}

/// Serialization happens exactly once, on Preview. Refresh and network results
/// cannot mutate these bytes. Copy/Save accept only this approved representation.
#[derive(Clone)]
pub struct ReportPreview(Arc<str>);
impl ReportPreview {
    pub fn new(snapshot: &DiagnosticSnapshot) -> Result<Self, AppError> {
        let report = DiagnosticReportV1 {
            version: 1,
            application: "VectrCap",
            application_version: env!("CARGO_PKG_VERSION"),
            snapshot: *snapshot,
        };
        let json = serde_json::to_string_pretty(&report)
            .map_err(|_| AppError::Persistence("encoding diagnostic report"))?;
        if json.len() > MAX_REPORT_BYTES {
            return Err(AppError::InvalidInput("diagnostic report exceeds limit"));
        }
        Ok(Self(json.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn copy(&self, clipboard: &dyn ClipboardWriter) -> Result<(), AppError> {
        clipboard.write_text(self.as_str())
    }
    /// Publish atomically in the user's selected folder. Successful publication
    /// is the primary result; no fallible bookkeeping runs afterward.
    pub fn save(&self, path: &Path, replace: bool) -> Result<(), AppError> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or(AppError::InvalidInput("select a diagnostic report folder"))?;
        let mut tmp = tempfile::Builder::new()
            .prefix(crate::persistence::PARTIAL_PREFIX)
            .tempfile_in(parent)
            .map_err(|e| AppError::io("writing diagnostic report", &e))?;
        tmp.write_all(self.as_str().as_bytes())
            .map_err(|e| AppError::io("writing diagnostic report", &e))?;
        tmp.as_file()
            .sync_all()
            .map_err(|e| AppError::io("writing diagnostic report", &e))?;
        if replace {
            crate::persistence::settings::replace_write_through(tmp.path(), path)?;
        } else {
            tmp.persist_noclobber(path)
                .map_err(|e| AppError::io("publishing diagnostic report", &e.error))?;
        }
        Ok(())
    }
}
