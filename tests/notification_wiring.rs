use std::{
    collections::HashMap,
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
    platform::windows::credentials::{CredentialBackend, CredentialRecord, WindowsCredentialStore},
    vectr::{
        client::UploadToken,
        queue::{QueueCommand, QueueController, QueueEffect},
    },
};

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

fn open(root: &std::path::Path) -> (AppController, Arc<History>, Arc<ArtifactStore>) {
    let history = Arc::new(History::open(&root.join("history.db")).unwrap());
    let artifacts = Arc::new(ArtifactStore::new(root.join("spool")).unwrap());
    let store = Arc::new(Store::with_backend(Memory::default()));
    let app = AppController::open(
        Box::new(FileDestinationRepository::open(&root.join("settings.json")).unwrap()),
        history.clone(),
        store,
        artifacts.clone(),
    )
    .unwrap();
    (app, history, artifacts)
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
    let (mut app, history, artifacts) = open(dir.path());
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
    let (mut app, history, artifacts) = open(dir.path());
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
    let (mut app, history, artifacts) = open(dir.path());
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
