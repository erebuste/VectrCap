mod support;
mod task11_support;

use chrono::{Duration, Local};
use std::{collections::HashSet, fs, sync::Arc};
use support::{Fixture as Https, ID, RECEIPT, Reply};
use vectrcap::{
    app::{
        destinations::{
            DestinationSelection, DestinationSnapshot, Destinations, resolve_destination,
        },
        types::{Artifact, ArtifactKind, UploadReceipt},
    },
    persistence::{
        archive::{ArchiveStore, LocalDeleteOutcome, delete_local},
        artifacts::ArtifactStore,
        filename::FilenamePreset,
        history::{History, JobStatus, UploadJob},
        settings::{ArchivePolicy, ServerOrigin, Settings, StorageMode},
    },
    vectr::{
        client::{UploadToken, VectrClient},
        uploader::Uploader,
    },
};

struct RecoveryFixture {
    dir: tempfile::TempDir,
    history: Arc<History>,
    store: Arc<ArtifactStore>,
}
impl RecoveryFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(&dir.path().join("history.db")).unwrap());
        let store = Arc::new(ArtifactStore::new(dir.path().join("spool")).unwrap());
        Self {
            dir,
            history,
            store,
        }
    }
    fn screenshot(&self) -> Artifact {
        let path = self
            .dir
            .path()
            .join(format!("{}.png", uuid::Uuid::new_v4()));
        fs::write(&path, b"synthetic finalized pixels").unwrap();
        self.store
            .stage_file(&path, ArtifactKind::Screenshot)
            .unwrap()
    }
    fn archive(&self) -> ArchiveStore {
        ArchiveStore::new(
            self.history.clone(),
            self.dir.path().join("archive"),
            FilenamePreset::default(),
        )
    }
    fn reopen(&self) -> Arc<History> {
        Arc::new(History::open(&self.dir.path().join("history.db")).unwrap())
    }
    fn policy(&self, age: Option<u16>) -> ArchivePolicy {
        ArchivePolicy {
            storage_mode: StorageMode::AlwaysSave,
            archive_root: Some(self.dir.path().join("archive")),
            cleanup_age_days: age,
        }
    }
}
fn destination(origin: ServerOrigin) -> DestinationSnapshot {
    let mut d = Destinations::default();
    let id = d.add("synthetic destination", origin, None).unwrap();
    d.bind_credential(id, &HashSet::new()).unwrap();
    resolve_destination(
        DestinationSelection::Inherit,
        DestinationSelection::Inherit,
        &d,
    )
    .unwrap()
}
fn job(local: &RecoveryFixture, artifact: &Artifact) -> UploadJob {
    local
        .history
        .create_job(
            artifact,
            &destination(ServerOrigin::parse("https://fixture.example").unwrap()),
        )
        .unwrap()
}
fn complete(local: &RecoveryFixture, job: &UploadJob) -> UploadReceipt {
    let receipt =
        UploadReceipt::new("https://fixture.example/private-link", "synthetic.png").unwrap();
    local
        .history
        .transition(job.id, JobStatus::Running)
        .unwrap();
    let session = uuid::Uuid::new_v4();
    local.history.set_session(job.id, session).unwrap();
    local
        .history
        .record_receipt(job.id, 1, Some(session), &receipt)
        .unwrap();
    receipt
}

#[tokio::test]
async fn restart_recovers_upload_without_capturing_again() {
    let https = Https::new(vec![
        Reply::json(200, r#"{"chunkBytes":3,"received":[],"done":null}"#),
        Reply::json(204, ""),
        Reply::json(200, RECEIPT),
    ])
    .await;
    let local = RecoveryFixture::new();
    let source = local.dir.path().join("capture.png");
    fs::write(&source, b"abc").unwrap();
    let artifact = local
        .store
        .stage_file(&source, ArtifactKind::Screenshot)
        .unwrap();
    let j = local
        .history
        .create_job(&artifact, &destination(https.origin.clone()))
        .unwrap();
    local.history.transition(j.id, JobStatus::Running).unwrap();
    local
        .history
        .set_session(j.id, ID.parse().unwrap())
        .unwrap();
    // The acquisition source is gone; retry must use the immutable spool.
    fs::remove_file(source).unwrap();
    let reopened = local.reopen();
    let recovered = reopened.recover_jobs().unwrap().pop().unwrap();
    assert_eq!(recovered.id, j.id);
    assert_eq!(recovered.artifact, artifact);
    let root = https.root.clone();
    let uploader = Uploader::new(
        reopened.clone(),
        local.store.clone(),
        Arc::new(move |d: &DestinationSnapshot| {
            VectrClient::with_root_certificate(
                d.origin.clone(),
                UploadToken::new("synthetic-token".into()).unwrap(),
                root.clone(),
            )
        }),
    );
    let (tx, _rx) = tokio::sync::mpsc::channel(32);
    let receipt = uploader
        .run(recovered, tokio_util::sync::CancellationToken::new(), tx)
        .await
        .unwrap();
    assert_eq!(
        reopened.job(j.id).unwrap().unwrap().status,
        JobStatus::Completed(receipt)
    );
    let requests = https.log();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|r| r.path != "/api/uploads"));
    assert_eq!(requests[1].body, b"abc");
}

#[test]
fn completed_receipt_survives_clipboard_failure_and_restart() {
    let local = RecoveryFixture::new();
    let j = job(&local, &local.screenshot());
    let receipt = complete(&local, &j);
    struct FailingClipboard;
    impl vectrcap::platform::windows::clipboard::ClipboardWriter for FailingClipboard {
        fn write_text(&self, _: &str) -> Result<(), vectrcap::app::error::AppError> {
            Err(vectrcap::app::error::AppError::Persistence(
                "synthetic clipboard failure",
            ))
        }
    }
    use vectrcap::app::{
        controller::AppController,
        destination_manager::FileDestinationRepository,
        events::{AppCommand, AppEffect, HistoryAction},
    };
    let mut app = AppController::open(
        Box::new(FileDestinationRepository::open(&local.dir.path().join("settings.json")).unwrap()),
        local.history.clone(),
        Arc::new(task11_support::Store::default()),
        local.store.clone(),
        Arc::new(task11_support::NoShell),
        Arc::new(FailingClipboard),
    )
    .unwrap();
    let id = local.history.browse(&Default::default()).unwrap().records[0].id;
    let effects = app.dispatch(AppCommand::HistoryAction {
        ids: vec![id],
        action: HistoryAction::CopyLinks,
    });
    assert!(effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
    assert!(!effects.iter().any(|e| matches!(e, AppEffect::Queue(_))));
    drop(app);
    let reopened = local.reopen();
    assert!(reopened.recover_jobs().unwrap().is_empty());
    let restored = reopened.job(j.id).unwrap().unwrap();
    assert_eq!(restored.status, JobStatus::Completed(receipt));
}

#[test]
fn history_removal_cannot_delete_arbitrary_paths() {
    let local = RecoveryFixture::new();
    let original = local.dir.path().join("user-owned.txt");
    fs::write(&original, b"user data").unwrap();
    let a = local
        .store
        .stage_file(&original, ArtifactKind::File)
        .unwrap();
    let j = job(&local, &a);
    complete(&local, &j);
    assert_eq!(
        delete_local(&local.history, a.id).unwrap(),
        LocalDeleteOutcome::NoOwnedCopy
    );
    let id = local.history.browse(&Default::default()).unwrap().records[0].id;
    local.history.remove_history(&[id]).unwrap();
    assert_eq!(fs::read(original).unwrap(), b"user data");
    local.store.verify(&a).unwrap();
    assert!(matches!(
        local.history.job(j.id).unwrap().unwrap().status,
        JobStatus::Completed(_)
    ));
}

#[test]
fn settings_never_contain_token() {
    let local = RecoveryFixture::new();
    let token = UploadToken::new("TOKEN-PRIVATE-CANARY".into()).unwrap();
    let mut settings = Settings::default();
    let id = settings
        .destinations
        .add(
            "synthetic",
            ServerOrigin::parse("https://fixture.example").unwrap(),
            None,
        )
        .unwrap();
    settings
        .destinations
        .bind_credential(id, &HashSet::new())
        .unwrap();
    let path = local.dir.path().join("settings.json");
    settings.save(&path).unwrap();
    let bytes = fs::read(&path).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("TOKEN-PRIVATE-CANARY"));
    assert!(!format!("{token:?}").contains("TOKEN-PRIVATE-CANARY"));
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    value["token"] = "TOKEN-PRIVATE-CANARY".into();
    fs::write(&path, value.to_string()).unwrap();
    assert!(Settings::load(&path).is_err());
}

#[test]
fn quit_during_work_leaves_recoverable_state() {
    let local = RecoveryFixture::new();
    let a = local.screenshot();
    let j = job(&local, &a);
    local.history.transition(j.id, JobStatus::Running).unwrap();
    local
        .history
        .set_session(j.id, ID.parse().unwrap())
        .unwrap();
    local.history.set_queue_paused(true).unwrap();
    local.history.set_job_paused(j.id, true).unwrap();
    let mut app = task11_support::controller(local.dir.path());
    let effects = app.dispatch(vectrcap::app::events::AppCommand::Quit);
    assert_eq!(app.lifecycle, vectrcap::app::events::Lifecycle::Draining);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, vectrcap::app::events::AppEffect::DrainWorker))
    );
    drop(app);
    // A process exit at any instruction must leave durable work for startup.
    let reopened = local.reopen();
    let jobs = reopened.recover_jobs().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].status, JobStatus::Interrupted);
    assert_eq!(jobs[0].session, Some(ID.parse().unwrap()));
    assert!(jobs[0].paused && reopened.queue_paused().unwrap());
    local
        .store
        .recover(&reopened.artifacts_needing_spool().unwrap())
        .unwrap();
    local.store.verify(&a).unwrap();
}

#[test]
fn missing_archive_keeps_remote_link() {
    let local = RecoveryFixture::new();
    let a = local.screenshot();
    let entry = local.archive().save_finalized(&a, Local::now()).unwrap();
    let j = job(&local, &a);
    let receipt = complete(&local, &j);
    fs::remove_file(&entry.path).unwrap();
    assert_eq!(
        local
            .archive()
            .cleanup(&local.policy(Some(1)), Local::now() + Duration::days(2))
            .unwrap()
            .removed,
        0
    );
    assert_eq!(
        local.reopen().job(j.id).unwrap().unwrap().status,
        JobStatus::Completed(receipt)
    );
}

#[test]
fn cleanup_cannot_delete_replaced_or_reparse_targets() {
    let local = RecoveryFixture::new();
    let archive = local.archive();
    let entry = archive
        .save_finalized(&local.screenshot(), Local::now())
        .unwrap();
    let moved = entry.path.with_extension("moved");
    fs::rename(&entry.path, &moved).unwrap();
    fs::copy(&moved, &entry.path).unwrap();
    let later = Local::now() + Duration::days(2);
    assert_eq!(
        archive
            .cleanup(&local.policy(Some(1)), later)
            .unwrap()
            .removed,
        0
    );
    assert!(entry.path.exists() && moved.exists());
    let month = entry.path.parent().unwrap();
    let outside = local.dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let sentinel = outside.join(entry.path.file_name().unwrap());
    fs::copy(&entry.path, &sentinel).unwrap();
    fs::rename(month, month.with_extension("moved")).unwrap();
    let status = std::process::Command::new("cmd")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(month)
        .arg(&outside)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        archive
            .cleanup(&local.policy(Some(1)), later)
            .unwrap()
            .removed,
        0
    );
    assert!(
        delete_local(&local.history, entry.artifact_id).is_err(),
        "a reparse target must be refused"
    );
    assert_eq!(fs::read(sentinel).unwrap(), b"synthetic finalized pixels");
    // Remove the synthetic junction itself before TempDir's cleanup.
    fs::remove_dir(month).unwrap();
}

#[test]
fn cleanup_respects_disabled_age_pending_manual_modified_moved_and_favorites() {
    let local = RecoveryFixture::new();
    let archive = local.archive();
    let expired = archive
        .save_finalized(&local.screenshot(), Local::now())
        .unwrap();
    let pending_art = local.screenshot();
    let pending = archive.save_finalized(&pending_art, Local::now()).unwrap();
    job(&local, &pending_art);
    let manual = archive
        .save_manual(&local.screenshot(), Local::now())
        .unwrap();
    let modified = archive
        .save_finalized(&local.screenshot(), Local::now())
        .unwrap();
    fs::write(&modified.path, b"user edit").unwrap();
    let moved = archive
        .save_finalized(&local.screenshot(), Local::now())
        .unwrap();
    let moved_path = local.dir.path().join("user-moved.png");
    fs::rename(&moved.path, &moved_path).unwrap();
    let favorite = archive
        .save_finalized(&local.screenshot(), Local::now())
        .unwrap();
    let id = local
        .history
        .browse(&Default::default())
        .unwrap()
        .records
        .iter()
        .find(|r| r.artifact.id == favorite.artifact_id)
        .unwrap()
        .id;
    local.history.set_favorite(id, true).unwrap();
    let later = Local::now() + Duration::days(31);
    assert_eq!(
        archive.cleanup(&local.policy(None), later).unwrap().removed,
        0
    );
    assert_eq!(
        archive
            .cleanup(&local.policy(Some(30)), Local::now() + Duration::days(29))
            .unwrap()
            .removed,
        0
    );
    assert_eq!(
        archive
            .cleanup(&local.policy(Some(30)), later)
            .unwrap()
            .removed,
        2
    );
    assert!(!expired.path.exists() && !favorite.path.exists());
    assert!(pending.path.exists() && manual.path.exists() && moved_path.exists());
    assert_eq!(fs::read(modified.path).unwrap(), b"user edit");
    assert!(local.history.history_record(id).unwrap().unwrap().favorite);
}

#[test]
fn app_paths_preserve_existing_v1_layout_without_creating_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("application");
    let paths = vectrcap::persistence::AppPaths::from_root(root.clone()).unwrap();
    assert_eq!(vectrcap::persistence::AppPaths::LAYOUT_VERSION, 1);
    assert_eq!(paths.settings, root.join("settings.json"));
    assert_eq!(paths.history, root.join("history.db"));
    assert_eq!(paths.spool, root.join("spool"));
    assert_eq!(paths.root, root);
    assert!(!root.exists());
    assert!(vectrcap::persistence::AppPaths::from_root("relative".into()).is_err());
}
