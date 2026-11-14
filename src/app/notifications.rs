//! Pure completion-notice state.
//!
//! Callers submit bounded job snapshots. The center keeps at most three visible
//! notices and returns closed [`RequestedCompletionAction`] values. It does not
//! render UI, steal focus, open files, copy links, upload, or delete history.
//!
//! Precondition for every action: the controller resolves that job against
//! current authoritative history immediately before executing it. Payloads carry
//! job IDs only. Paths and sharing URLs from the snapshot are not commands, and
//! a dismissed or evicted notice is stale. `safe_to_open` is supplied by the
//! trusted caller; this module does not classify executables. Timed dismissal
//! and the in-app surface are shell work: call [`NotificationCenter::dismiss`].

use std::collections::VecDeque;
use std::fmt;

use uuid::Uuid;

use crate::app::error::AppError;
use crate::app::types::{JobId, UploadReceipt};

pub const MAX_VISIBLE_NOTICES: usize = 3;
pub const MAX_BATCH_ITEMS: usize = 100;

const MAX_DISPLAY_NAME_SCALARS: usize = 255;
const MAX_DISPLAY_NAME_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NoticeId(Uuid);

/// Caller-supplied local-file fact. `Openable` requires both availability and
/// the caller's safe-to-open flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalPresence {
    Missing,
    Openable,
    RevealOnly,
}

#[derive(Clone, PartialEq, Eq)]
struct DisplayName(String);

impl DisplayName {
    fn parse(name: &str) -> Result<Self, AppError> {
        if name.is_empty()
            || name.len() > MAX_DISPLAY_NAME_BYTES
            || name.chars().count() > MAX_DISPLAY_NAME_SCALARS
            || name.chars().any(char::is_control)
        {
            return Err(AppError::InvalidInput(
                "display name is empty, too long, or contains control characters",
            ));
        }
        if name.chars().any(|c| c == '/' || c == '\\') {
            return Err(AppError::InvalidInput("display name must not be a path"));
        }
        Ok(DisplayName(name.to_owned()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DisplayName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Validated upload failure identifier. Never a raw exception or path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionErrorCode(String);

impl CompletionErrorCode {
    fn parse(code: &str) -> Result<Self, AppError> {
        let ok = (1..=64).contains(&code.len())
            && code.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'_' | b'.' | b'-')
            });
        if !ok {
            return Err(AppError::InvalidInput(
                "failure code must be a short lowercase identifier",
            ));
        }
        Ok(CompletionErrorCode(code.to_owned()))
    }
}

/// Closed upload outcome for one completion snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionStatus {
    Uploaded,
    Failed(CompletionErrorCode),
    LocalOnly,
}

impl CompletionStatus {
    pub fn uploaded() -> Self {
        Self::Uploaded
    }

    pub fn local_only() -> Self {
        Self::LocalOnly
    }

    pub fn failed(code: &str) -> Result<Self, AppError> {
        Ok(Self::Failed(CompletionErrorCode::parse(code)?))
    }
}

/// One job's completion facts. Receipt URLs stay inside [`UploadReceipt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobCompletion {
    job_id: JobId,
    display_name: DisplayName,
    local: LocalPresence,
    receipt: Option<UploadReceipt>,
    status: CompletionStatus,
}

impl JobCompletion {
    pub fn try_new(
        job_id: JobId,
        display_name: &str,
        local_available: bool,
        safe_to_open: bool,
        receipt: Option<UploadReceipt>,
        status: CompletionStatus,
    ) -> Result<Self, AppError> {
        if job_id.0.is_nil() {
            return Err(AppError::InvalidInput("nil job id"));
        }
        let display_name = DisplayName::parse(display_name)?;
        if safe_to_open && !local_available {
            return Err(AppError::InvalidInput(
                "safe to open requires an available local file",
            ));
        }
        let local = if !local_available {
            LocalPresence::Missing
        } else if safe_to_open {
            LocalPresence::Openable
        } else {
            LocalPresence::RevealOnly
        };
        let receipt_matches = matches!(
            (&status, receipt.is_some()),
            (CompletionStatus::Uploaded, true)
                | (CompletionStatus::Failed(_), false)
                | (CompletionStatus::LocalOnly, false)
        );
        if !receipt_matches {
            return Err(AppError::InvalidInput(
                "completion receipt does not match status",
            ));
        }
        Ok(JobCompletion {
            job_id,
            display_name,
            local,
            receipt,
            status,
        })
    }

    pub fn job_id(&self) -> JobId {
        self.job_id
    }
}

/// A single completed job, or one grouped batch in submission order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionNotice {
    Job(JobCompletion),
    Batch(Vec<JobCompletion>),
}

/// Closed intent. The shell executes this only after rereading history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestedCompletionAction {
    OpenLocal(JobId),
    ShowFolder(JobId),
    OpenLink(JobId),
    CopyLink(JobId),
    CopyAgain(JobId),
    OpenBatchSummary { jobs: Vec<JobId> },
    CopySuccessfulLinks { jobs: Vec<JobId> },
}

impl RequestedCompletionAction {
    pub fn job_ids(&self) -> Vec<JobId> {
        match self {
            Self::OpenLocal(id)
            | Self::ShowFolder(id)
            | Self::OpenLink(id)
            | Self::CopyLink(id)
            | Self::CopyAgain(id) => vec![*id],
            Self::OpenBatchSummary { jobs } | Self::CopySuccessfulLinks { jobs } => jobs.clone(),
        }
    }
}

/// Read-only view of one job inside a notice. Debug output redacts the name.
#[derive(Clone, PartialEq, Eq)]
pub struct ItemView {
    job_id: JobId,
    display_name: DisplayName,
    status: CompletionStatus,
    local: LocalPresence,
    has_receipt: bool,
    copy_failed: bool,
}

impl ItemView {
    pub fn job_id(&self) -> JobId {
        self.job_id
    }

    pub fn display_name(&self) -> &str {
        self.display_name.as_str()
    }

    pub fn status(&self) -> &CompletionStatus {
        &self.status
    }

    pub fn local(&self) -> LocalPresence {
        self.local
    }

    pub fn has_confirmed_receipt(&self) -> bool {
        self.has_receipt
    }

    pub fn copy_failed(&self) -> bool {
        self.copy_failed
    }
}

impl fmt::Debug for ItemView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ItemView")
            .field("job_id", &self.job_id)
            .field("display_name", &self.display_name)
            .field("status", &self.status)
            .field("local", &self.local)
            .field("has_receipt", &self.has_receipt)
            .field("copy_failed", &self.copy_failed)
            .finish()
    }
}

/// A visible notice. `executed` stays empty: publishing does not run an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeView {
    id: NoticeId,
    batch: bool,
    items: Vec<ItemView>,
    steals_focus: bool,
    auto_opens: bool,
    executed: Option<RequestedCompletionAction>,
}

impl NoticeView {
    pub fn id(&self) -> NoticeId {
        self.id
    }

    pub fn is_batch(&self) -> bool {
        self.batch
    }

    pub fn items(&self) -> &[ItemView] {
        &self.items
    }

    pub fn steals_focus(&self) -> bool {
        self.steals_focus
    }

    pub fn auto_opens(&self) -> bool {
        self.auto_opens
    }

    pub fn executed(&self) -> Option<&RequestedCompletionAction> {
        self.executed.as_ref()
    }
}

/// What the shell should change. There is no focus, upload, or history-deletion effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationEffect {
    Show(NoticeView),
    Dismissed {
        notice_id: NoticeId,
    },
    /// The completion was accepted and not shown. History is untouched.
    Suppressed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ItemState {
    completion: JobCompletion,
    copy_failed: bool,
}

impl ItemState {
    fn view(&self) -> ItemView {
        ItemView {
            job_id: self.completion.job_id,
            display_name: self.completion.display_name.clone(),
            status: self.completion.status.clone(),
            local: self.completion.local,
            has_receipt: self.completion.receipt.is_some(),
            copy_failed: self.copy_failed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NoticeBody {
    Single(ItemState),
    Batch(Vec<ItemState>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Notice {
    id: NoticeId,
    body: NoticeBody,
}

impl Notice {
    fn items(&self) -> &[ItemState] {
        match &self.body {
            NoticeBody::Single(item) => std::slice::from_ref(item),
            NoticeBody::Batch(items) => items,
        }
    }

    fn items_mut(&mut self) -> &mut [ItemState] {
        match &mut self.body {
            NoticeBody::Single(item) => std::slice::from_mut(item),
            NoticeBody::Batch(items) => items,
        }
    }

    fn actions(&self) -> Vec<RequestedCompletionAction> {
        match &self.body {
            NoticeBody::Single(item) => item_actions(item),
            NoticeBody::Batch(items) => batch_actions(items),
        }
    }

    fn offers(&self, action: &RequestedCompletionAction) -> bool {
        self.actions().iter().any(|candidate| candidate == action)
            || self.items().iter().any(|item| {
                item_actions(item)
                    .iter()
                    .any(|candidate| candidate == action)
            })
    }

    fn view(&self) -> NoticeView {
        NoticeView {
            id: self.id,
            batch: matches!(self.body, NoticeBody::Batch(_)),
            items: self.items().iter().map(ItemState::view).collect(),
            steals_focus: false,
            auto_opens: false,
            executed: None,
        }
    }
}

fn item_actions(item: &ItemState) -> Vec<RequestedCompletionAction> {
    let id = item.completion.job_id;
    let mut actions = Vec::new();
    match item.completion.local {
        LocalPresence::Openable => actions.push(RequestedCompletionAction::OpenLocal(id)),
        LocalPresence::RevealOnly => actions.push(RequestedCompletionAction::ShowFolder(id)),
        LocalPresence::Missing => {}
    }
    if item.completion.receipt.is_some() {
        actions.push(RequestedCompletionAction::OpenLink(id));
        actions.push(if item.copy_failed {
            RequestedCompletionAction::CopyAgain(id)
        } else {
            RequestedCompletionAction::CopyLink(id)
        });
    }
    actions
}

fn batch_actions(items: &[ItemState]) -> Vec<RequestedCompletionAction> {
    let jobs = items
        .iter()
        .map(|item| item.completion.job_id)
        .collect::<Vec<_>>();
    let mut actions = vec![RequestedCompletionAction::OpenBatchSummary { jobs }];
    let successful = items
        .iter()
        .filter(|item| item.completion.receipt.is_some())
        .map(|item| item.completion.job_id)
        .collect::<Vec<_>>();
    if !successful.is_empty() {
        actions.push(RequestedCompletionAction::CopySuccessfulLinks { jobs: successful });
    }
    actions
}

fn states(jobs: Vec<JobCompletion>) -> Vec<ItemState> {
    jobs.into_iter()
        .map(|completion| ItemState {
            completion,
            copy_failed: false,
        })
        .collect()
}

/// Visible completion notices. History outside this type is the record keeper.
#[derive(Debug, PartialEq, Eq)]
pub struct NotificationCenter {
    enabled: bool,
    visible: VecDeque<Notice>,
}

impl NotificationCenter {
    pub fn new() -> Self {
        Self {
            enabled: true,
            visible: VecDeque::new(),
        }
    }

    pub fn notifications_enabled(&self) -> bool {
        self.enabled
    }

    /// Disabling withdraws visible notices and does not replay them later.
    pub fn set_notifications_enabled(&mut self, enabled: bool) -> Vec<NotificationEffect> {
        if self.enabled == enabled {
            return Vec::new();
        }
        self.enabled = enabled;
        if enabled {
            return Vec::new();
        }
        self.visible
            .drain(..)
            .map(|notice| NotificationEffect::Dismissed {
                notice_id: notice.id,
            })
            .collect()
    }

    pub fn record(
        &mut self,
        notice: CompletionNotice,
    ) -> Result<Vec<NotificationEffect>, AppError> {
        let body = self.prepare(notice)?;
        if !self.enabled {
            return Ok(vec![NotificationEffect::Suppressed]);
        }
        let mut effects = Vec::new();
        while self.visible.len() >= MAX_VISIBLE_NOTICES {
            let Some(oldest) = self.visible.pop_front() else {
                break;
            };
            effects.push(NotificationEffect::Dismissed {
                notice_id: oldest.id,
            });
        }
        let stored = Notice {
            id: NoticeId(Uuid::new_v4()),
            body,
        };
        effects.push(NotificationEffect::Show(stored.view()));
        self.visible.push_back(stored);
        Ok(effects)
    }

    pub fn dismiss(&mut self, id: NoticeId) -> Result<Vec<NotificationEffect>, AppError> {
        let Some(position) = self.visible.iter().position(|notice| notice.id == id) else {
            return Err(AppError::InvalidInput("notice is not visible"));
        };
        self.visible.remove(position);
        Ok(vec![NotificationEffect::Dismissed { notice_id: id }])
    }

    pub fn visible(&self) -> Vec<NoticeView> {
        self.visible.iter().map(Notice::view).collect()
    }

    pub fn retained_job_count(&self) -> usize {
        self.visible.iter().map(|notice| notice.items().len()).sum()
    }

    pub fn actions(&self, id: NoticeId) -> Result<Vec<RequestedCompletionAction>, AppError> {
        Ok(self.find(id)?.actions())
    }

    pub fn summary_actions(
        &self,
        id: NoticeId,
        job: JobId,
    ) -> Result<Vec<RequestedCompletionAction>, AppError> {
        let notice = self.find(id)?;
        let item = notice
            .items()
            .iter()
            .find(|item| item.completion.job_id == job)
            .ok_or(AppError::InvalidInput("job is not in this notice"))?;
        Ok(item_actions(item))
    }

    /// Confirms `action` is currently offered. Does not execute it.
    pub fn request(
        &self,
        id: NoticeId,
        action: &RequestedCompletionAction,
    ) -> Result<RequestedCompletionAction, AppError> {
        let notice = self.find(id)?;
        if notice.offers(action) {
            Ok(action.clone())
        } else {
            Err(AppError::InvalidInput("completion action is not available"))
        }
    }

    /// Keeps the confirmed receipt and switches that job to [`RequestedCompletionAction::CopyAgain`].
    pub fn note_copy_failed(&mut self, id: NoticeId, job: JobId) -> Result<(), AppError> {
        let notice = self.find_mut(id)?;
        let item = notice
            .items_mut()
            .iter_mut()
            .find(|item| item.completion.job_id == job)
            .ok_or(AppError::InvalidInput("job is not in this notice"))?;
        if item.completion.receipt.is_none() {
            return Err(AppError::InvalidInput("completion action is not available"));
        }
        item.copy_failed = true;
        Ok(())
    }

    fn prepare(&self, notice: CompletionNotice) -> Result<NoticeBody, AppError> {
        match notice {
            CompletionNotice::Job(job) => {
                self.ensure_new(&[job.job_id])?;
                Ok(NoticeBody::Single(ItemState {
                    completion: job,
                    copy_failed: false,
                }))
            }
            CompletionNotice::Batch(jobs) => {
                if jobs.is_empty() {
                    return Err(AppError::InvalidInput("completion batch is empty"));
                }
                if jobs.len() > MAX_BATCH_ITEMS {
                    return Err(AppError::InvalidInput("completion batch exceeds 100 items"));
                }
                let mut seen = Vec::with_capacity(jobs.len());
                for job in &jobs {
                    if seen.contains(&job.job_id) {
                        return Err(AppError::InvalidInput("duplicate completion job"));
                    }
                    seen.push(job.job_id);
                }
                self.ensure_new(&seen)?;
                Ok(NoticeBody::Batch(states(jobs)))
            }
        }
    }

    fn ensure_new(&self, ids: &[JobId]) -> Result<(), AppError> {
        let retained = self
            .visible
            .iter()
            .flat_map(|notice| notice.items().iter().map(|item| item.completion.job_id))
            .collect::<Vec<_>>();
        if ids.iter().any(|id| retained.contains(id)) {
            return Err(AppError::InvalidInput("duplicate completion job"));
        }
        Ok(())
    }

    fn find(&self, id: NoticeId) -> Result<&Notice, AppError> {
        self.visible
            .iter()
            .find(|notice| notice.id == id)
            .ok_or(AppError::InvalidInput("notice is not visible"))
    }

    fn find_mut(&mut self, id: NoticeId) -> Result<&mut Notice, AppError> {
        self.visible
            .iter_mut()
            .find(|notice| notice.id == id)
            .ok_or(AppError::InvalidInput("notice is not visible"))
    }
}

impl Default for NotificationCenter {
    fn default() -> Self {
        Self::new()
    }
}
