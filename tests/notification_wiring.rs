use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use vectrcap::{
    app::{
        controller::AppController,
        destination_manager::{DestinationEdit, FileDestinationRepository},
        destinations::DestinationSelection,
        error::AppError,
        events::*,
        notifications::{CompletionStatus, RequestedCompletionAction},
        types::ArtifactKind,
    },
    persistence::{artifacts::ArtifactStore, history::History, settings::ServerOrigin},
    platform::windows::{
        clipboard::ClipboardWriter,
        credentials::{CredentialBackend, CredentialRecord, WindowsCredentialStore},
        shell_actions::ShellActions,
    },
    vectr::{
        client::UploadToken,
        queue::{QueueCommand, QueueController, QueueEffect},
    },
};

#[derive(Default)]
struct FakeShell {
    opened: Mutex<Vec<PathBuf>>,
    revealed: Mutex<Vec<PathBuf>>,
    opened_urls: Mutex<Vec<String>>,
}
impl ShellActions for FakeShell {
    fn open_path(&self, path: &Path) -> Result<(), AppError> {
        self.opened.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }
    fn open_url(&self, url: &str) -> Result<(), AppError> {
        self.opened_urls.lock().unwrap().push(url.to_string());
        Ok(())
    }
    fn reveal_path(&self, path: &Path) -> Result<(), AppError> {
        self.revealed.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }
}

#[derive(Default)]
struct FakeClipboard {
    written: Mutex<Vec<String>>,
    fail: Mutex<bool>,
}
impl ClipboardWriter for FakeClipboard {
    fn write_text(&self, text: &str) -> Result<(), AppError> {
        if *self.fail.lock().unwrap() {
            return Err(AppError::Persistence("fake clipboard failure"));
        }
        self.written.lock().unwrap().push(text.to_string());
        Ok(())
    }
}

#[derive(Default)]
struct Memory(Mutex<HashMap<String, CredentialRecord>>);
impl CredentialBackend for Memory {
    fn read(&self, target: &str) -> Result<Option<CredentialRecord>, AppError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(target)
            .map(|r| CredentialRecord {
                target: r.target.clone(),
                owner: r.owner.clone(),
                kind: r.kind,
                persistence: r.persistence,
                flags: r.flags,
                attributes: r.attributes,
                blob: zeroize::Zeroizing::new(r.blob.to_vec()),
            }))
    }
    fn write(&self, record: &CredentialRecord) -> Result<(), AppError> {
        self.0.lock().unwrap().insert(
            record.target.clone(),
            CredentialRecord {
                target: record.target.clone(),
                owner: record.owner.clone(),
                kind: record.kind,
                persistence: record.persistence,
                flags: record.flags,
                attributes: record.attributes,
                blob: zeroize::Zeroizing::new(record.blob.to_vec()),
            },
        );
        Ok(())
    }
    fn delete(&self, target: &str) -> Result<(), AppError> {
        self.0.lock().unwrap().remove(target);
        Ok(())
    }
}
type Store = WindowsCredentialStore<Memory>;

fn open(
    root: &Path,
) -> (
    AppController,
    Arc<History>,
    Arc<ArtifactStore>,
    Arc<FakeShell>,
    Arc<FakeClipboard>,
) {
    let history = Arc::new(History::open(&root.join("history.db")).unwrap());
    let artifacts = Arc::new(ArtifactStore::new(root.join("spool")).unwrap());
    let store = Arc::new(Store::with_backend(Memory::default()));
    let shell = Arc::new(FakeShell::default());
    let clipboard = Arc::new(FakeClipboard::default());
    let app = AppController::open(
        Box::new(FileDestinationRepository::open(&root.join("settings.json")).unwrap()),
        history.clone(),
        store,
        artifacts.clone(),
        shell.clone(),
        clipboard.clone(),
    )
    .unwrap();
    (app, history, artifacts, shell, clipboard)
}

/// Seeds a destination with a usable token.
fn seed(app: &mut AppController) {
    let revision = app.settings().revision;
    let mut destinations = app.settings().destinations.clone();
    let id = destinations
        .add(
            "Personal",
            ServerOrigin::parse("https://example.invalid").unwrap(),
            None,
        )
        .unwrap();
    app.dispatch(AppCommand::ApplyDestinations {
        edit: DestinationEdit {
            destinations,
            tokens: vec![(id, UploadToken::new("synthetic-token".into()).unwrap())],
        },
        revision,
    });
}

/// Stages a file, runs it through BeginWorkflow/AcceptWorkflow, admits it on
/// a queue controller sharing the same History, and settles it. Returns the
/// accepted job's id.
fn accept_and_settle(
    app: &mut AppController,
    history: &Arc<History>,
    artifacts: &Arc<ArtifactStore>,
    source_name: &str,
    kind: ArtifactKind,
    result: Result<vectrcap::app::types::UploadReceipt, AppError>,
) -> vectrcap::app::types::JobId {
    let source = std::env::temp_dir().join(format!("{}-{source_name}", uuid::Uuid::new_v4()));
    std::fs::write(&source, b"synthetic").unwrap();
    let artifact = artifacts.stage_file(&source, kind).unwrap();
    std::fs::remove_file(&source).ok();
    let workflow = uuid::Uuid::new_v4();
    app.dispatch(AppCommand::BeginWorkflow {
        id: workflow,
        selection: DestinationSelection::Inherit,
    });
    let effects = app.dispatch(AppCommand::AcceptWorkflow {
        id: workflow,
        artifact,
    });
    let job_id = effects
        .iter()
        .find_map(|effect| match effect {
            AppEffect::JobAccepted(id) => Some(*id),
            _ => None,
        })
        .unwrap();
    let mut queue = QueueController::new(history.clone()).unwrap();
    queue.handle(QueueCommand::Admit(job_id));
    // A nonempty upload result requires a recorded session; the real client
    // would set this once the server confirms one, which this fixture skips.
    history.set_session(job_id, uuid::Uuid::new_v4()).unwrap();
    let attempt = history.job(job_id).unwrap().unwrap().attempt;
    let effects = queue.handle(QueueCommand::Settled {
        job: job_id,
        attempt,
        result,
    });
    for effect in effects {
        if let QueueEffect::Changed(id) = effect {
            app.on_event(AppEvent::Queue(QueueEffect::Changed(id)));
        }
    }
    job_id
}

#[test]
fn completed_job_produces_a_visible_notice() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _shell, _clipboard) = open(dir.path());
    seed(&mut app);
    let receipt =
        vectrcap::app::types::UploadReceipt::new("https://links.example/report", "report.pdf")
            .unwrap();
    let job_id = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "report.pdf",
        ArtifactKind::File,
        Ok(receipt),
    );
    let notices = app.visible_notices();
    assert_eq!(notices.len(), 1);
    let notice = &notices[0];
    assert!(!notice.view.is_batch());
    assert_eq!(notice.view.items()[0].job_id(), job_id);
    assert_eq!(
        notice.view.items()[0].status(),
        &CompletionStatus::uploaded()
    );
    assert!(
        notice
            .actions
            .iter()
            .any(|a| matches!(a, RequestedCompletionAction::OpenLink(_)))
    );
}

#[test]
fn failed_job_produces_a_notice_without_a_link() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _shell, _clipboard) = open(dir.path());
    seed(&mut app);
    let job_id = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "report.pdf",
        ArtifactKind::File,
        Err(AppError::Network),
    );
    let notices = app.visible_notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].view.items()[0].job_id(), job_id);
    assert!(matches!(
        notices[0].view.items()[0].status(),
        CompletionStatus::Failed(_)
    ));
    assert!(!notices[0].actions.iter().any(|a| matches!(
        a,
        RequestedCompletionAction::OpenLink(_) | RequestedCompletionAction::CopyLink(_)
    )));
}

#[test]
fn executable_file_source_is_reveal_only_even_when_present() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _shell, _clipboard) = open(dir.path());
    seed(&mut app);
    let receipt =
        vectrcap::app::types::UploadReceipt::new("https://links.example/tool", "tool.exe").unwrap();
    accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "tool.exe",
        ArtifactKind::File,
        Ok(receipt),
    );
    let notices = app.visible_notices();
    assert_eq!(notices.len(), 1);
    assert!(
        notices[0]
            .actions
            .iter()
            .any(|a| matches!(a, RequestedCompletionAction::ShowFolder(_)))
    );
    assert!(
        !notices[0]
            .actions
            .iter()
            .any(|a| matches!(a, RequestedCompletionAction::OpenLocal(_)))
    );
}

#[test]
fn dismiss_removes_a_visible_notice() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _shell, _clipboard) = open(dir.path());
    seed(&mut app);
    let receipt =
        vectrcap::app::types::UploadReceipt::new("https://links.example/report", "report.pdf")
            .unwrap();
    accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "report.pdf",
        ArtifactKind::File,
        Ok(receipt),
    );
    let id = app.visible_notices()[0].view.id();
    app.dispatch(AppCommand::DismissNotice(id));
    assert!(app.visible_notices().is_empty());
}

#[test]
fn disabling_notifications_withdraws_visible_notices() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _shell, _clipboard) = open(dir.path());
    seed(&mut app);
    let receipt =
        vectrcap::app::types::UploadReceipt::new("https://links.example/report", "report.pdf")
            .unwrap();
    accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "report.pdf",
        ArtifactKind::File,
        Ok(receipt),
    );
    assert_eq!(app.visible_notices().len(), 1);
    app.dispatch(AppCommand::SetNotificationsEnabled(false));
    assert!(app.visible_notices().is_empty());
    assert!(!app.notifications_enabled());
}

#[test]
fn dismissing_an_unknown_notice_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _shell, _clipboard) = open(dir.path());
    seed(&mut app);
    let receipt =
        vectrcap::app::types::UploadReceipt::new("https://links.example/report", "report.pdf")
            .unwrap();
    accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "report.pdf",
        ArtifactKind::File,
        Ok(receipt),
    );
    let id = app.visible_notices()[0].view.id();
    app.dispatch(AppCommand::DismissNotice(id));
    let effects = app.dispatch(AppCommand::DismissNotice(id));
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::Error(_)))
    );
}

#[test]
fn open_local_action_calls_the_injected_shell_opener_with_the_spool_path() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, shell, _clipboard) = open(dir.path());
    seed(&mut app);
    let receipt =
        vectrcap::app::types::UploadReceipt::new("https://links.example/report", "report.pdf")
            .unwrap();
    let job_id = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "report.pdf",
        ArtifactKind::File,
        Ok(receipt),
    );
    let spool_path = history.job(job_id).unwrap().unwrap().artifact.path;
    let notice = &app.visible_notices()[0];
    let id = notice.view.id();
    app.dispatch(AppCommand::ExecuteCompletionAction {
        notice: id,
        action: RequestedCompletionAction::OpenLocal(job_id),
    });
    assert_eq!(shell.opened.lock().unwrap().as_slice(), &[spool_path]);
}

#[test]
fn copy_link_action_writes_the_receipt_url_to_the_injected_clipboard() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _shell, clipboard) = open(dir.path());
    seed(&mut app);
    let receipt =
        vectrcap::app::types::UploadReceipt::new("https://links.example/report", "report.pdf")
            .unwrap();
    let job_id = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "report.pdf",
        ArtifactKind::File,
        Ok(receipt),
    );
    let id = app.visible_notices()[0].view.id();
    app.dispatch(AppCommand::ExecuteCompletionAction {
        notice: id,
        action: RequestedCompletionAction::CopyLink(job_id),
    });
    assert_eq!(
        clipboard.written.lock().unwrap().as_slice(),
        &["https://links.example/report".to_string()]
    );
}

#[test]
fn clipboard_failure_flips_the_action_to_copy_again_without_reuploading() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _shell, clipboard) = open(dir.path());
    seed(&mut app);
    let receipt =
        vectrcap::app::types::UploadReceipt::new("https://links.example/report", "report.pdf")
            .unwrap();
    let job_id = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "report.pdf",
        ArtifactKind::File,
        Ok(receipt),
    );
    let id = app.visible_notices()[0].view.id();
    *clipboard.fail.lock().unwrap() = true;
    let effects = app.dispatch(AppCommand::ExecuteCompletionAction {
        notice: id,
        action: RequestedCompletionAction::CopyLink(job_id),
    });
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::Error(_)))
    );
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::Queue(_)))
    );
    assert!(
        app.visible_notices()[0]
            .actions
            .iter()
            .any(|a| matches!(a, RequestedCompletionAction::CopyAgain(_)))
    );
    assert!(clipboard.written.lock().unwrap().is_empty());
}

#[test]
fn stale_action_against_a_dismissed_notice_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, shell, clipboard) = open(dir.path());
    seed(&mut app);
    let receipt =
        vectrcap::app::types::UploadReceipt::new("https://links.example/report", "report.pdf")
            .unwrap();
    let job_id = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "report.pdf",
        ArtifactKind::File,
        Ok(receipt),
    );
    let id = app.visible_notices()[0].view.id();
    app.dispatch(AppCommand::DismissNotice(id));
    let effects = app.dispatch(AppCommand::ExecuteCompletionAction {
        notice: id,
        action: RequestedCompletionAction::OpenLink(job_id),
    });
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::Error(_)))
    );
    assert!(shell.opened_urls.lock().unwrap().is_empty());
    assert!(clipboard.written.lock().unwrap().is_empty());
}

#[test]
fn action_re_resolves_against_current_history_before_executing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, shell, _clipboard) = open(dir.path());
    seed(&mut app);
    let receipt =
        vectrcap::app::types::UploadReceipt::new("https://links.example/report", "report.pdf")
            .unwrap();
    let job_id = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "report.pdf",
        ArtifactKind::File,
        Ok(receipt),
    );
    let spool_path = history.job(job_id).unwrap().unwrap().artifact.path;
    std::fs::remove_file(&spool_path).unwrap();
    let id = app.visible_notices()[0].view.id();
    let effects = app.dispatch(AppCommand::ExecuteCompletionAction {
        notice: id,
        action: RequestedCompletionAction::OpenLocal(job_id),
    });
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::Error(_)))
    );
    assert!(shell.opened.lock().unwrap().is_empty());
}

#[test]
fn notification_preference_survives_restart_and_can_be_enabled_again() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, ..) = open(dir.path());
    let revision = app.settings().revision;
    let effects = app.dispatch(AppCommand::SetNotificationsEnabled(false));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, AppEffect::SettingsSaved(_)))
    );
    assert!(!app.notifications_enabled());
    assert_eq!(app.settings().revision, revision + 1);
    drop(app);
    let (mut app, ..) = open(dir.path());
    assert!(!app.notifications_enabled());
    app.dispatch(AppCommand::SetNotificationsEnabled(true));
    drop(app);
    let (app, ..) = open(dir.path());
    assert!(app.notifications_enabled());
}

#[test]
fn failed_preference_save_preserves_runtime_and_disk_state() {
    use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt};
    let dir = tempfile::tempdir().unwrap();
    let (mut app, ..) = open(dir.path());
    let old = app.settings().clone();
    let held = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(dir.path().join("settings.json"))
        .unwrap();
    let effects = app.dispatch(AppCommand::SetNotificationsEnabled(false));
    assert!(effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
    assert!(app.notifications_enabled());
    assert_eq!(app.settings(), &old);
    drop(held);
    assert_eq!(
        vectrcap::persistence::settings::Settings::load(&dir.path().join("settings.json")).unwrap(),
        old
    );
}

#[test]
fn preference_update_rejects_external_edits_and_repeated_value_preserves_revision() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, ..) = open(dir.path());
    let revision = app.settings().revision;
    app.dispatch(AppCommand::SetNotificationsEnabled(true));
    assert_eq!(app.settings().revision, revision);
    let mut external = app.settings().clone();
    external.revision += 1;
    external.save(&dir.path().join("settings.json")).unwrap();
    let effects = app.dispatch(AppCommand::SetNotificationsEnabled(false));
    assert!(effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
    assert!(app.notifications_enabled());
    assert_eq!(
        vectrcap::persistence::settings::Settings::load(&dir.path().join("settings.json")).unwrap(),
        external
    );
}

#[test]
fn batch_summary_preserves_exact_order_and_resolves_current_history() {
    use vectrcap::app::types::JobId;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _, _) = open(dir.path());
    seed(&mut app);
    let first = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "first.txt",
        ArtifactKind::File,
        Err(AppError::Network),
    );
    let second = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "second.txt",
        ArtifactKind::File,
        Err(AppError::Network),
    );
    let missing = JobId(uuid::Uuid::new_v4());
    let effects = app.dispatch(AppCommand::ShowBatchSummary(vec![second, missing, first]));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, AppEffect::Window(Some(Window::BatchSummary))))
    );
    assert_eq!(app.window, Some(Window::BatchSummary));
    let rows = app.batch_summary().unwrap();
    assert_eq!(
        rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![second, missing, first]
    );
    assert!(rows[1].job.is_none());
    assert!(rows[0].local_available);
    std::fs::remove_file(&rows[0].job.as_ref().unwrap().artifact.path).unwrap();
    let refreshed = app.batch_summary().unwrap();
    assert!(!refreshed[0].local_available);
    assert!(refreshed[0].job.is_some());
    app.dispatch(AppCommand::Show(Window::Uploads));
    app.dispatch(AppCommand::Show(Window::BatchSummary));
    assert_eq!(
        app.batch_summary()
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        vec![second, missing, first]
    );
}

#[test]
fn invalid_batch_selection_does_not_replace_the_previous_summary() {
    use vectrcap::app::types::JobId;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, ..) = open(dir.path());
    let id = JobId(uuid::Uuid::new_v4());
    app.dispatch(AppCommand::ShowBatchSummary(vec![id]));
    for invalid in [
        vec![],
        vec![id, id],
        vec![JobId(uuid::Uuid::nil())],
        (0..101).map(|_| JobId(uuid::Uuid::new_v4())).collect(),
    ] {
        let effects = app.dispatch(AppCommand::ShowBatchSummary(invalid));
        assert!(effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
        assert_eq!(app.batch_summary().unwrap()[0].id, id);
    }
}

#[test]
fn a_notice_cannot_navigate_to_jobs_it_does_not_authorize() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _, _) = open(dir.path());
    seed(&mut app);
    let id = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "single.txt",
        ArtifactKind::File,
        Err(AppError::Network),
    );
    let notice = app.visible_notices()[0].view.id();
    let effects = app.dispatch(AppCommand::ExecuteCompletionAction {
        notice,
        action: RequestedCompletionAction::OpenBatchSummary { jobs: vec![id] },
    });
    assert!(effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
    assert!(app.batch_summary().unwrap().is_empty());
}

#[test]
fn batch_navigation_can_resolve_a_job_outside_the_queue_snapshot() {
    use vectrcap::app::types::UploadReceipt;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _, _) = open(dir.path());
    seed(&mut app);
    let selected = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "selected.txt",
        ArtifactKind::File,
        Ok(UploadReceipt::new("https://links.example/selected", "selected.txt").unwrap()),
    );
    for i in 0..101 {
        accept_and_settle(
            &mut app,
            &history,
            &artifacts,
            &format!("other-{i}.txt"),
            ArtifactKind::File,
            Err(AppError::Network),
        );
    }
    assert!(
        !history
            .shell_jobs()
            .unwrap()
            .iter()
            .any(|j| j.id == selected)
    );
    app.dispatch(AppCommand::ShowBatchSummary(vec![selected]));
    let rows = app.batch_summary().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].job.as_ref().unwrap().id, selected);
}

#[test]
fn history_browser_searches_confirmed_links_and_preserves_controller_query() {
    use vectrcap::app::types::UploadReceipt;
    use vectrcap::persistence::history::browser::*;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _, _) = open(dir.path());
    seed(&mut app);
    let id = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "history.txt",
        ArtifactKind::File,
        Ok(UploadReceipt::new("https://links.example/unique-history-link", "history.txt").unwrap()),
    );
    let query = HistoryQuery {
        search: "unique-history-link".into(),
        ..Default::default()
    };
    app.dispatch(AppCommand::QueryHistory(query.clone()));
    app.dispatch(AppCommand::Show(Window::History));
    let page = app.history_page().unwrap();
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0].job.as_ref().unwrap().id, id);
    app.dispatch(AppCommand::SetHistoryFavorite {
        id: page.records[0].id,
        favorite: true,
    });
    assert!(app.history_page().unwrap().records[0].favorite);
    app.dispatch(AppCommand::Show(Window::Settings));
    app.dispatch(AppCommand::Show(Window::History));
    assert!(app.history_query() == &query);
    let effects = app.dispatch(AppCommand::QueryHistory(HistoryQuery {
        search: "x".repeat(257),
        ..Default::default()
    }));
    assert!(effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
    assert!(app.history_query() == &query);
}

#[test]
fn history_preferences_persist_and_failed_save_keeps_the_previous_value() {
    use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt};
    use vectrcap::persistence::settings::{HistoryLayout, HistoryPreferences};
    let dir = tempfile::tempdir().unwrap();
    let (mut app, ..) = open(dir.path());
    let desired = HistoryPreferences {
        layout: HistoryLayout::List,
        preview: false,
    };
    app.dispatch(AppCommand::SetHistoryPreferences(desired.clone()));
    assert_eq!(app.settings().history, desired);
    drop(app);
    let (mut app, ..) = open(dir.path());
    assert_eq!(app.settings().history, desired);
    let revision = app.settings().revision;
    let held = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(dir.path().join("settings.json"))
        .unwrap();
    let effects = app.dispatch(AppCommand::SetHistoryPreferences(
        HistoryPreferences::default(),
    ));
    assert!(effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
    assert_eq!(app.settings().history, desired);
    assert_eq!(app.settings().revision, revision);
    drop(held);
}
#[test]
fn history_actions_resolve_current_records_and_never_upload_on_copy_failure() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, shell, clipboard) = open(dir.path());
    seed(&mut app);
    let receipt = vectrcap::app::types::UploadReceipt::new(
        "https://links.example/history-action",
        "image.png",
    )
    .unwrap();
    let job = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "image.png",
        ArtifactKind::Screenshot,
        Ok(receipt),
    );
    let row = app.history_page().unwrap().records.remove(0);
    let effects = app.dispatch(AppCommand::HistoryAction {
        ids: vec![row.id],
        action: HistoryAction::OpenLocal,
    });
    for effect in effects {
        if let AppEffect::LocalHistoryAction(task) = effect {
            task.run(tokio_util::sync::CancellationToken::new())
                .unwrap();
        }
    }
    assert_eq!(shell.opened.lock().unwrap().len(), 1);
    *clipboard.fail.lock().unwrap() = true;
    let effects = app.dispatch(AppCommand::HistoryAction {
        ids: vec![row.id],
        action: HistoryAction::CopyLinks,
    });
    assert!(effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
    assert!(!effects.iter().any(|e| matches!(e, AppEffect::Queue(_))));
    *clipboard.fail.lock().unwrap() = false;
    std::fs::remove_file(&row.artifact.path).unwrap();
    let effects = app.dispatch(AppCommand::HistoryAction {
        ids: vec![row.id],
        action: HistoryAction::OpenLocal,
    });
    assert!(effects.into_iter().any(|e| {
        match e {
            AppEffect::LocalHistoryAction(task) => task
                .run(tokio_util::sync::CancellationToken::new())
                .is_err(),
            _ => false,
        }
    }));
    app.dispatch(AppCommand::HistoryAction {
        ids: vec![row.id],
        action: HistoryAction::CopyLinks,
    });
    assert_eq!(
        clipboard.written.lock().unwrap().last().unwrap(),
        "https://links.example/history-action"
    );
    app.dispatch(AppCommand::HistoryAction {
        ids: vec![row.id],
        action: HistoryAction::Remove,
    });
    assert!(history.job(job).unwrap().is_some());
    assert!(history.history_record(row.id).unwrap().is_none());
    let effects = app.dispatch(AppCommand::HistoryAction {
        ids: vec![row.id],
        action: HistoryAction::CopyLinks,
    });
    assert!(effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
}
#[test]
fn history_removal_is_atomic_and_retry_only_targets_selected_eligible_failures() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _, _) = open(dir.path());
    seed(&mut app);
    let first = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "first.txt",
        ArtifactKind::File,
        Err(AppError::Network),
    );
    let second = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "second.txt",
        ArtifactKind::File,
        Err(AppError::Network),
    );
    let rows = app.history_page().unwrap().records;
    let first_row = rows
        .iter()
        .find(|r| r.job.as_ref().unwrap().id == first)
        .unwrap();
    let second_row = rows
        .iter()
        .find(|r| r.job.as_ref().unwrap().id == second)
        .unwrap();
    let effects = app.dispatch(AppCommand::HistoryAction {
        ids: vec![first_row.id],
        action: HistoryAction::RetryFailed,
    });
    let ids = effects
        .into_iter()
        .find_map(|e| {
            if let AppEffect::Queue(QueueCommand::RetryJobs(ids)) = e {
                Some(ids)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(ids, vec![first]);
    history.retry_selected_uploads(&ids).unwrap();
    assert_eq!(
        history.job(first).unwrap().unwrap().status,
        vectrcap::persistence::history::JobStatus::Pending
    );
    assert!(matches!(
        history.job(second).unwrap().unwrap().status,
        vectrcap::persistence::history::JobStatus::Failed { .. }
    ));
    history
        .transition(second, vectrcap::persistence::history::JobStatus::Cancelled)
        .unwrap();
    assert!(
        history
            .remove_history(&[second_row.id, first_row.id])
            .is_err()
    );
    assert!(history.history_record(second_row.id).unwrap().is_some());
    assert!(second_row.artifact.path.exists());
}

#[test]
fn a_new_upload_recreates_removed_history_without_restoring_its_favorite() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _, _) = open(dir.path());
    seed(&mut app);
    let id = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "reused.txt",
        ArtifactKind::File,
        Ok(
            vectrcap::app::types::UploadReceipt::new("https://links.example/old", "reused.txt")
                .unwrap(),
        ),
    );
    let row = app.history_page().unwrap().records.remove(0);
    history.set_favorite(row.id, true).unwrap();
    history.remove_history(&[row.id]).unwrap();
    let job = history.job(id).unwrap().unwrap();
    history.create_job(&job.artifact, &job.destination).unwrap();
    let row2 = app.history_page().unwrap().records.remove(0);
    assert_ne!(row2.id, row.id);
    assert!(!row2.favorite);
    assert!(row2.artifact.path.exists());
}
#[test]
fn history_status_filters_distinguish_failed_from_confirmed_uploads() {
    use vectrcap::persistence::history::browser::*;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _, _) = open(dir.path());
    seed(&mut app);
    let uploaded = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "uploaded.txt",
        ArtifactKind::File,
        Ok(vectrcap::app::types::UploadReceipt::new(
            "https://links.example/confirmed",
            "uploaded.txt",
        )
        .unwrap()),
    );
    let failed = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "failed.txt",
        ArtifactKind::File,
        Err(AppError::Network),
    );
    for (status, id) in [
        (HistoryStatus::Uploaded, uploaded),
        (HistoryStatus::Failed, failed),
    ] {
        let rows = history
            .browse(&HistoryQuery {
                status,
                ..Default::default()
            })
            .unwrap()
            .records;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].job.as_ref().unwrap().id, id);
    }
}

#[test]
fn bulk_history_retry_skips_missing_sources_and_reports_counts() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _, _) = open(dir.path());
    seed(&mut app);
    let valid = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "valid.txt",
        ArtifactKind::File,
        Err(AppError::Network),
    );
    let missing = accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "missing.txt",
        ArtifactKind::File,
        Err(AppError::Network),
    );
    let rows = app.history_page().unwrap().records;
    let valid_row = rows
        .iter()
        .find(|r| r.job.as_ref().unwrap().id == valid)
        .unwrap();
    let missing_row = rows
        .iter()
        .find(|r| r.job.as_ref().unwrap().id == missing)
        .unwrap();
    std::fs::remove_file(&missing_row.artifact.path).unwrap();
    let effects = app.dispatch(AppCommand::HistoryAction {
        ids: vec![missing_row.id, valid_row.id],
        action: HistoryAction::RetryFailed,
    });
    assert!(
        effects.iter().any(
            |e| matches!(e,AppEffect::Queue(QueueCommand::RetryJobs(ids)) if ids==&vec![valid])
        )
    );
    assert!(effects.iter().any(|e| matches!(
        e,
        AppEffect::HistoryRetryReport {
            queued: 1,
            skipped: 1
        }
    )));
}
#[test]
fn local_actions_are_deferred_cancellable_and_reject_removed_records() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, shell, _) = open(dir.path());
    seed(&mut app);
    accept_and_settle(
        &mut app,
        &history,
        &artifacts,
        "view.txt",
        ArtifactKind::File,
        Ok(
            vectrcap::app::types::UploadReceipt::new("https://links.example/view", "view.txt")
                .unwrap(),
        ),
    );
    let row = app.history_page().unwrap().records.remove(0);
    let effects = app.dispatch(AppCommand::HistoryAction {
        ids: vec![row.id],
        action: HistoryAction::OpenLocal,
    });
    assert!(shell.opened.lock().unwrap().is_empty());
    let task = effects
        .into_iter()
        .find_map(|e| {
            if let AppEffect::LocalHistoryAction(task) = e {
                Some(task)
            } else {
                None
            }
        })
        .unwrap();
    history.remove_history(&[row.id]).unwrap();
    assert!(
        task.run(tokio_util::sync::CancellationToken::new())
            .is_err()
    );
    assert!(shell.opened.lock().unwrap().is_empty());
}

/// Drives a native multi-file selection exactly like the real staging driver
/// in `src/app/desktop.rs`: accepts the batch, stages each path with the real
/// `ArtifactStore`, admits each resulting job on a queue sharing the same
/// History, and settles it with the given result, in order.
fn accept_batch_and_settle(
    app: &mut AppController,
    history: &Arc<History>,
    artifacts: &Arc<ArtifactStore>,
    files: Vec<(&str, Result<vectrcap::app::types::UploadReceipt, AppError>)>,
) -> Vec<vectrcap::app::types::JobId> {
    let sources: Vec<_> = files
        .iter()
        .map(|(name, _)| {
            let path = std::env::temp_dir().join(format!("{}-{name}", uuid::Uuid::new_v4()));
            std::fs::write(&path, b"synthetic").unwrap();
            path
        })
        .collect();
    let effects = app.dispatch(AppCommand::AcceptFileSelection(sources.clone()));
    let batch = effects
        .into_iter()
        .find_map(|e| match e {
            AppEffect::StageFiles { batch, .. } => Some(batch),
            _ => None,
        })
        .expect("batch accepted");
    let mut job_ids = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        let display_name = source.file_name().unwrap().to_string_lossy().into_owned();
        let outcome = artifacts.stage_file(source, ArtifactKind::File);
        std::fs::remove_file(source).ok();
        let effects = app.on_event(AppEvent::FileStaged {
            batch,
            index,
            display_name,
            outcome,
        });
        let job_id = effects
            .into_iter()
            .find_map(|e| match e {
                AppEffect::JobAccepted(id) => Some(id),
                _ => None,
            })
            .unwrap();
        job_ids.push(job_id);
    }
    let mut queue = QueueController::new(history.clone()).unwrap();
    for (job_id, (_, result)) in job_ids.iter().zip(files) {
        queue.handle(QueueCommand::Admit(*job_id));
        history.set_session(*job_id, uuid::Uuid::new_v4()).unwrap();
        let attempt = history.job(*job_id).unwrap().unwrap().attempt;
        let effects = queue.handle(QueueCommand::Settled {
            job: *job_id,
            attempt,
            result,
        });
        for effect in effects {
            if let QueueEffect::Changed(id) = effect {
                app.on_event(AppEvent::Queue(QueueEffect::Changed(id)));
            }
        }
    }
    job_ids
}

#[test]
fn batch_completion_is_one_grouped_notice_with_successful_links_only() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _shell, clipboard) = open(dir.path());
    seed(&mut app);
    let receipt =
        vectrcap::app::types::UploadReceipt::new("https://links.example/good", "good.txt").unwrap();
    let jobs = accept_batch_and_settle(
        &mut app,
        &history,
        &artifacts,
        vec![
            ("good.txt", Ok(receipt.clone())),
            ("bad.txt", Err(AppError::Network)),
        ],
    );
    // One grouped notice for the whole batch, not two independent toasts.
    let notices = app.visible_notices();
    assert_eq!(notices.len(), 1);
    let notice = &notices[0];
    assert!(notice.view.is_batch());
    assert_eq!(notice.view.items().len(), 2);
    assert_eq!(notice.view.items()[0].job_id(), jobs[0]);
    assert_eq!(notice.view.items()[1].job_id(), jobs[1]);
    assert_eq!(
        notice.view.items()[0].status(),
        &CompletionStatus::uploaded()
    );
    assert!(matches!(
        notice.view.items()[1].status(),
        CompletionStatus::Failed(_)
    ));
    // Copying "successful links" must only ever copy the confirmed one.
    let copy_action = notice
        .actions
        .iter()
        .find(|a| matches!(a, RequestedCompletionAction::CopySuccessfulLinks { .. }))
        .cloned()
        .expect("batch offers copying its successful links");
    let RequestedCompletionAction::CopySuccessfulLinks { jobs: successful } = &copy_action else {
        unreachable!()
    };
    assert_eq!(successful, &[jobs[0]]);
    app.dispatch(AppCommand::ExecuteCompletionAction {
        notice: notice.view.id(),
        action: copy_action,
    });
    assert_eq!(
        clipboard.written.lock().unwrap().as_slice(),
        [receipt.url().to_owned()]
    );
}

#[test]
fn batch_summary_opens_from_its_grouped_notice() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _shell, _clipboard) = open(dir.path());
    seed(&mut app);
    let receipt =
        vectrcap::app::types::UploadReceipt::new("https://links.example/one", "one.txt").unwrap();
    let other =
        vectrcap::app::types::UploadReceipt::new("https://links.example/two", "two.txt").unwrap();
    let jobs = accept_batch_and_settle(
        &mut app,
        &history,
        &artifacts,
        vec![("one.txt", Ok(receipt)), ("two.txt", Ok(other))],
    );
    let notice = app.visible_notices().remove(0);
    let open_action = notice
        .actions
        .iter()
        .find(|a| matches!(a, RequestedCompletionAction::OpenBatchSummary { .. }))
        .cloned()
        .unwrap();
    app.dispatch(AppCommand::ExecuteCompletionAction {
        notice: notice.view.id(),
        action: open_action,
    });
    let summary = app.batch_summary().unwrap();
    assert_eq!(summary.len(), 2);
    assert_eq!(summary.iter().map(|r| r.id).collect::<Vec<_>>(), jobs);
}

#[test]
fn single_file_selection_gets_an_ordinary_job_notice_not_a_one_item_batch() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _shell, _clipboard) = open(dir.path());
    seed(&mut app);
    let receipt =
        vectrcap::app::types::UploadReceipt::new("https://links.example/solo", "solo.txt").unwrap();
    let jobs = accept_batch_and_settle(
        &mut app,
        &history,
        &artifacts,
        vec![("solo.txt", Ok(receipt))],
    );
    let notices = app.visible_notices();
    assert_eq!(notices.len(), 1);
    assert!(
        !notices[0].view.is_batch(),
        "a single-file picker selection must get an ordinary job notice, not a one-item batch"
    );
    assert_eq!(notices[0].view.items()[0].job_id(), jobs[0]);
}

#[test]
fn out_of_order_staging_and_settlement_still_yields_a_picker_ordered_batch_notice() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _shell, _clipboard) = open(dir.path());
    seed(&mut app);
    let names = ["a.txt", "b.txt", "c.txt"];
    let sources: Vec<_> = names
        .iter()
        .map(|name| {
            let path = std::env::temp_dir().join(format!("{}-{name}", uuid::Uuid::new_v4()));
            std::fs::write(&path, b"synthetic").unwrap();
            path
        })
        .collect();
    let effects = app.dispatch(AppCommand::AcceptFileSelection(sources.clone()));
    let batch = effects
        .into_iter()
        .find_map(|e| match e {
            AppEffect::StageFiles { batch, .. } => Some(batch),
            _ => None,
        })
        .unwrap();
    // Stage and feed FileStaged in REVERSE picker order (c, b, a).
    let mut job_ids: Vec<Option<vectrcap::app::types::JobId>> = vec![None, None, None];
    for &index in &[2usize, 1, 0] {
        let source = &sources[index];
        let display_name = source.file_name().unwrap().to_string_lossy().into_owned();
        let outcome = artifacts.stage_file(source, ArtifactKind::File);
        std::fs::remove_file(source).ok();
        let effects = app.on_event(AppEvent::FileStaged {
            batch,
            index,
            display_name,
            outcome,
        });
        job_ids[index] = effects.into_iter().find_map(|e| match e {
            AppEffect::JobAccepted(id) => Some(id),
            _ => None,
        });
    }
    let job_ids: Vec<_> = job_ids.into_iter().map(Option::unwrap).collect();
    let mut queue = QueueController::new(history.clone()).unwrap();
    // Settle in REVERSE order too (c, b, a) — the opposite of picker order.
    for &index in &[2usize, 1, 0] {
        let job_id = job_ids[index];
        queue.handle(QueueCommand::Admit(job_id));
        history.set_session(job_id, uuid::Uuid::new_v4()).unwrap();
        let attempt = history.job(job_id).unwrap().unwrap().attempt;
        let receipt = vectrcap::app::types::UploadReceipt::new(
            &format!("https://links.example/{index}"),
            names[index],
        )
        .unwrap();
        for effect in queue.handle(QueueCommand::Settled {
            job: job_id,
            attempt,
            result: Ok(receipt),
        }) {
            if let QueueEffect::Changed(id) = effect {
                app.on_event(AppEvent::Queue(QueueEffect::Changed(id)));
            }
        }
    }
    let notices = app.visible_notices();
    assert_eq!(notices.len(), 1);
    let notice = &notices[0];
    assert!(notice.view.is_batch());
    let ids: Vec<_> = notice.view.items().iter().map(|i| i.job_id()).collect();
    assert_eq!(
        ids, job_ids,
        "notice items must stay in original picker order, never completion order"
    );
}

#[test]
fn uncertain_and_cancelled_batch_members_resolve_truthfully_without_stranding_their_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _shell, _clipboard) = open(dir.path());
    seed(&mut app);
    let names = ["ok.txt", "uncertain.txt", "cancelled.txt"];
    let sources: Vec<_> = names
        .iter()
        .map(|name| {
            let path = std::env::temp_dir().join(format!("{}-{name}", uuid::Uuid::new_v4()));
            std::fs::write(&path, b"synthetic").unwrap();
            path
        })
        .collect();
    let effects = app.dispatch(AppCommand::AcceptFileSelection(sources.clone()));
    let batch = effects
        .into_iter()
        .find_map(|e| match e {
            AppEffect::StageFiles { batch, .. } => Some(batch),
            _ => None,
        })
        .unwrap();
    let mut job_ids = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        let display_name = source.file_name().unwrap().to_string_lossy().into_owned();
        let outcome = artifacts.stage_file(source, ArtifactKind::File);
        std::fs::remove_file(source).ok();
        let effects = app.on_event(AppEvent::FileStaged {
            batch,
            index,
            display_name,
            outcome,
        });
        job_ids.push(
            effects
                .into_iter()
                .find_map(|e| match e {
                    AppEffect::JobAccepted(id) => Some(id),
                    _ => None,
                })
                .unwrap(),
        );
    }
    let mut queue = QueueController::new(history.clone()).unwrap();
    // ok.txt succeeds normally.
    queue.handle(QueueCommand::Admit(job_ids[0]));
    history
        .set_session(job_ids[0], uuid::Uuid::new_v4())
        .unwrap();
    let attempt0 = history.job(job_ids[0]).unwrap().unwrap().attempt;
    let receipt =
        vectrcap::app::types::UploadReceipt::new("https://links.example/ok", "ok.txt").unwrap();
    for effect in queue.handle(QueueCommand::Settled {
        job: job_ids[0],
        attempt: attempt0,
        result: Ok(receipt),
    }) {
        if let QueueEffect::Changed(id) = effect {
            app.on_event(AppEvent::Queue(QueueEffect::Changed(id)));
        }
    }
    // Pausing keeps the scheduler from auto-activating cancelled.txt the
    // moment uncertain.txt settles below, so Cancel below hits it while
    // still genuinely pending, never run.
    history.set_queue_paused(true).unwrap();
    // uncertain.txt's own attempt ends ambiguous — never automatically retried.
    queue.handle(QueueCommand::Admit(job_ids[1]));
    let attempt1 = history.job(job_ids[1]).unwrap().unwrap().attempt;
    for effect in queue.handle(QueueCommand::Settled {
        job: job_ids[1],
        attempt: attempt1,
        result: Err(AppError::Uncertain("creation")),
    }) {
        if let QueueEffect::Changed(id) = effect {
            app.on_event(AppEvent::Queue(QueueEffect::Changed(id)));
        }
    }
    // cancelled.txt is explicitly cancelled before it ever runs.
    for effect in queue.handle(QueueCommand::Cancel(job_ids[2])) {
        if let QueueEffect::Changed(id) = effect {
            app.on_event(AppEvent::Queue(QueueEffect::Changed(id)));
        }
    }
    let notices = app.visible_notices();
    assert_eq!(
        notices.len(),
        1,
        "the batch must flush exactly once, never be withheld forever"
    );
    let notice = &notices[0];
    assert!(notice.view.is_batch());
    let statuses: Vec<_> = notice
        .view
        .items()
        .iter()
        .map(|i| (i.job_id(), i.status().clone()))
        .collect();
    assert_eq!(statuses[0], (job_ids[0], CompletionStatus::uploaded()));
    assert_eq!(statuses[1], (job_ids[1], CompletionStatus::Uncertain));
    assert_eq!(statuses[2], (job_ids[2], CompletionStatus::Cancelled));
}

#[test]
fn restarting_an_interrupted_batch_member_resolves_its_slot_without_stranding_its_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, history, artifacts, _shell, _clipboard) = open(dir.path());
    seed(&mut app);
    let names = ["ok.txt", "interrupted.txt"];
    let sources: Vec<_> = names
        .iter()
        .map(|name| {
            let path = std::env::temp_dir().join(format!("{}-{name}", uuid::Uuid::new_v4()));
            std::fs::write(&path, b"synthetic").unwrap();
            path
        })
        .collect();
    let effects = app.dispatch(AppCommand::AcceptFileSelection(sources.clone()));
    let batch = effects
        .into_iter()
        .find_map(|e| match e {
            AppEffect::StageFiles { batch, .. } => Some(batch),
            _ => None,
        })
        .unwrap();
    let mut job_ids = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        let display_name = source.file_name().unwrap().to_string_lossy().into_owned();
        let outcome = artifacts.stage_file(source, ArtifactKind::File);
        std::fs::remove_file(source).ok();
        let effects = app.on_event(AppEvent::FileStaged {
            batch,
            index,
            display_name,
            outcome,
        });
        job_ids.push(
            effects
                .into_iter()
                .find_map(|e| match e {
                    AppEffect::JobAccepted(id) => Some(id),
                    _ => None,
                })
                .unwrap(),
        );
    }
    let mut queue = QueueController::new(history.clone()).unwrap();
    // ok.txt succeeds.
    queue.handle(QueueCommand::Admit(job_ids[0]));
    history
        .set_session(job_ids[0], uuid::Uuid::new_v4())
        .unwrap();
    let attempt0 = history.job(job_ids[0]).unwrap().unwrap().attempt;
    let receipt =
        vectrcap::app::types::UploadReceipt::new("https://links.example/ok", "ok.txt").unwrap();
    for effect in queue.handle(QueueCommand::Settled {
        job: job_ids[0],
        attempt: attempt0,
        result: Ok(receipt),
    }) {
        if let QueueEffect::Changed(id) = effect {
            app.on_event(AppEvent::Queue(QueueEffect::Changed(id)));
        }
    }
    // interrupted.txt was mid-flight with a known session when its request
    // was paused — `Interrupted`, not a closed outcome, so `Changed` alone
    // must NOT resolve its batch slot: it is expected to resume on its own.
    // Pausing before the settle keeps the queue from immediately
    // auto-resuming an interrupted job once it transitions below: `Restart`
    // is meant to be an explicit choice, not something racing a live
    // scheduler, and this is also exactly how a real shutdown/pause leaves a
    // mid-flight request interrupted instead of instantly retried.
    history.set_queue_paused(true).unwrap();
    history
        .set_session(job_ids[1], uuid::Uuid::new_v4())
        .unwrap();
    let attempt1 = history.job(job_ids[1]).unwrap().unwrap().attempt;
    for effect in queue.handle(QueueCommand::Settled {
        job: job_ids[1],
        attempt: attempt1,
        result: Err(AppError::Paused),
    }) {
        if let QueueEffect::Changed(id) = effect {
            app.on_event(AppEvent::Queue(QueueEffect::Changed(id)));
        }
    }
    assert_eq!(
        history.job(job_ids[1]).unwrap().unwrap().status,
        vectrcap::persistence::history::JobStatus::Interrupted
    );
    assert!(
        app.visible_notices().is_empty(),
        "an interrupted (not closed) sibling must still withhold the batch notice"
    );
    // The user explicitly restarts it — never automatic — which retires the
    // old job as cancelled and admits an unrelated replacement.
    for effect in queue.handle(QueueCommand::Restart(job_ids[1])) {
        if let QueueEffect::Restarted {
            previous,
            replacement,
        } = effect
        {
            app.on_event(AppEvent::Queue(QueueEffect::Restarted {
                previous,
                replacement,
            }));
        }
    }
    let notices = app.visible_notices();
    assert_eq!(
        notices.len(),
        1,
        "an explicit restart must still release the batch, not strand the successful sibling"
    );
    let notice = &notices[0];
    assert!(notice.view.is_batch());
    let statuses: Vec<_> = notice
        .view
        .items()
        .iter()
        .map(|i| (i.job_id(), i.status().clone()))
        .collect();
    // The restarted slot is truthfully closed as Cancelled — never a
    // fabricated success — in its ORIGINAL picker position; the unrelated
    // replacement job is not part of this (already-closed) batch at all.
    assert_eq!(
        statuses,
        vec![
            (job_ids[0], CompletionStatus::uploaded()),
            (job_ids[1], CompletionStatus::Cancelled),
        ]
    );
}
