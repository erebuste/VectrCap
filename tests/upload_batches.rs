use vectrcap::app::batches::*;
#[test]
fn batch_links_follow_submission_order() {
    let mut b = UploadBatch::new(3, true).unwrap();
    b.resolve(2, BatchOutcome::Success("https://v.example/3".into()))
        .unwrap();
    b.resolve(0, BatchOutcome::Success("https://v.example/1".into()))
        .unwrap();
    assert!(b.take_automatic_copy().is_none());
    b.resolve(1, BatchOutcome::Failed).unwrap();
    assert_eq!(
        b.take_automatic_copy().unwrap(),
        "https://v.example/1\nhttps://v.example/3"
    );
}
#[test]
fn failed_items_remain_retryable() {
    let mut b = UploadBatch::new(2, true).unwrap();
    b.resolve(0, BatchOutcome::Failed).unwrap();
    b.resolve(1, BatchOutcome::Uncertain).unwrap();
    assert_eq!(b.retryable_indices(), vec![0]);
    assert!(b.take_automatic_copy().is_none());
}
#[test]
fn per_file_receipts_do_not_race_batch_clipboard() {
    let mut b = UploadBatch::new(2, true).unwrap();
    b.resolve(1, BatchOutcome::Success("https://v.example/b".into()))
        .unwrap();
    assert!(b.take_automatic_copy().is_none());
    b.resolve(0, BatchOutcome::Success("https://v.example/a".into()))
        .unwrap();
    assert!(b.take_automatic_copy().is_some());
    assert!(b.take_automatic_copy().is_none());
    assert!(b.resolve(0, BatchOutcome::Failed).is_err());
}
#[test]
fn separate_batches_have_distinct_results() {
    let mut a = UploadBatch::new(1, true).unwrap();
    let b = UploadBatch::new(1, true).unwrap();
    a.resolve(0, BatchOutcome::Success("https://v.example/a".into()))
        .unwrap();
    assert!(b.available_links().is_empty());
    assert_eq!(a.available_links(), "https://v.example/a");
}
#[test]
fn original_files_survive_all_task_and_cleanup_paths() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("original.zip");
    std::fs::write(&path, b"original").unwrap();
    let store =
        vectrcap::persistence::artifacts::ArtifactStore::new(dir.path().join("spool")).unwrap();
    let artifact = store
        .stage_file(&path, vectrcap::app::types::ArtifactKind::File)
        .unwrap();
    assert_eq!(std::fs::read(&artifact.path).unwrap(), b"original");
    drop(store);
    assert_eq!(std::fs::read(path).unwrap(), b"original");
}
mod task11_support;
use std::sync::Arc;
use vectrcap::{
    app::{
        cli::LaunchRequest, controller::AppController,
        destination_manager::FileDestinationRepository, events::*,
    },
    persistence::{
        artifacts::ArtifactStore,
        history::{History, JobStatus},
    },
};
fn wired(
    root: &std::path::Path,
    clipboard: Arc<task11_support::Clipboard>,
    credentials: Arc<task11_support::Store>,
) -> (AppController, Arc<History>, Arc<ArtifactStore>) {
    let history = Arc::new(History::open(&root.join("history.db")).unwrap());
    let artifacts = Arc::new(ArtifactStore::new(root.join("spool")).unwrap());
    let app = AppController::open(
        Box::new(FileDestinationRepository::open(&root.join("settings.json")).unwrap()),
        history.clone(),
        credentials,
        artifacts.clone(),
        Arc::new(task11_support::NoShell),
        clipboard,
    )
    .unwrap();
    (app, history, artifacts)
}
fn seed(app: &mut AppController) {
    let mut destinations = app.settings().destinations.clone();
    let id = destinations
        .add(
            "Original",
            vectrcap::persistence::settings::ServerOrigin::parse("https://v.example").unwrap(),
            None,
        )
        .unwrap();
    let revision = app.settings().revision;
    let effects = app.dispatch(AppCommand::ApplyDestinations {
        edit: vectrcap::app::destination_manager::DestinationEdit {
            destinations,
            tokens: vec![(
                id,
                vectrcap::vectr::client::UploadToken::new("synthetic-token".into()).unwrap(),
            )],
        },
        revision,
    });
    assert!(effects.iter().all(|e| !matches!(e, AppEffect::Error(_))));
}
#[test]
fn controller_copies_batch_once_in_order_and_recovers_without_duplicate_job() {
    let dir = tempfile::tempdir().unwrap();
    let clipboard = Arc::new(task11_support::Clipboard::default());
    let credentials = Arc::new(task11_support::Store::default());
    let (mut app, history, store) = wired(dir.path(), clipboard.clone(), credentials.clone());
    seed(&mut app);
    let paths: Vec<_> = (0..2)
        .map(|i| {
            let p = dir.path().join(format!("{i}.zip"));
            std::fs::write(&p, []).unwrap();
            p
        })
        .collect();
    let request =
        LaunchRequest::upload(paths.iter().map(|p| p.as_os_str().into()).collect()).unwrap();
    let id = request.request_id;
    app.accept_launch(request.clone()).unwrap();
    let artifact = store
        .stage_file(&paths[0], vectrcap::app::types::ArtifactKind::File)
        .unwrap();
    app.on_event(AppEvent::FileStaged {
        batch: id,
        index: 0,
        display_name: "0.zip".into(),
        outcome: Ok(artifact),
    });
    assert_eq!(history.shell_jobs().unwrap().len(), 1);
    drop(app);
    let (mut app, _, _) = wired(dir.path(), clipboard.clone(), credentials);
    let (_, effects) = app.accept_launch(request).unwrap();
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, AppEffect::StageIngressFile { index: 1, .. }))
    );
    assert!(!effects.iter().any(|e| matches!(
        e,
        AppEffect::StageIngressFile { index: 0, .. } | AppEffect::StageFiles { .. }
    )));
    let artifact = store
        .stage_file(&paths[1], vectrcap::app::types::ArtifactKind::File)
        .unwrap();
    app.on_event(AppEvent::FileStaged {
        batch: id,
        index: 1,
        display_name: "1.zip".into(),
        outcome: Ok(artifact),
    });
    let jobs = history.shell_jobs().unwrap();
    assert_eq!(jobs.len(), 2);
    for (position, job) in jobs.iter().rev().enumerate() {
        history.transition(job.id, JobStatus::Running).unwrap();
        let current = history.job(job.id).unwrap().unwrap();
        let receipt = vectrcap::app::types::UploadReceipt::new(
            &format!("https://v.example/{}", job.artifact.display_name),
            "file",
        )
        .unwrap();
        history
            .record_receipt(job.id, current.attempt, None, &receipt)
            .unwrap();
        app.on_event(AppEvent::Queue(
            vectrcap::vectr::queue::QueueEffect::Changed(job.id),
        ));
        if position == 0 {
            assert!(clipboard.0.lock().unwrap().is_empty());
        }
    }
    assert_eq!(
        *clipboard.0.lock().unwrap(),
        vec!["https://v.example/0.zip\nhttps://v.example/1.zip"]
    );
    assert_eq!(app.pending_ingress_count().unwrap(), 0);
}
#[test]
fn later_ingress_bookkeeping_failure_preserves_earlier_staging_effect() {
    let dir = tempfile::tempdir().unwrap();
    let credentials = Arc::new(task11_support::Store::default());
    let (mut app, _, _) = wired(
        dir.path(),
        Arc::new(task11_support::Clipboard::default()),
        credentials,
    );
    seed(&mut app);
    let db = dir.path().join("ingress.db");
    let mut ingress = vectrcap::app::ingress::Ingress::open(&db).unwrap();
    ingress
        .accept(LaunchRequest::upload(vec![dir.path().join("a.zip").into_os_string()]).unwrap())
        .unwrap();
    ingress
        .accept(
            LaunchRequest::import(dir.path().join("missing.vectrcap").into_os_string()).unwrap(),
        )
        .unwrap();
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_import_settlement BEFORE UPDATE OF done ON ingress WHEN OLD.kind=1 BEGIN SELECT RAISE(FAIL,'fixture'); END;").unwrap();
    let effects = app.resume_ingress().unwrap();
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, AppEffect::StageFiles { .. }))
    );
    assert!(effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
}
