mod support;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use vectrcap::{
    app::{
        controller::AppController,
        desktop::{DesktopRuntime, DesktopView},
        destination_manager::{DestinationEdit, FileDestinationRepository},
        destinations::{DestinationId, DestinationSelection},
        error::AppError,
        events::*,
        types::ArtifactKind,
    },
    persistence::{
        artifacts::ArtifactStore,
        history::{History, JobStatus},
        settings::ServerOrigin,
    },
    platform::windows::{clipboard::ClipboardWriter, shell_actions::ShellActions},
    platform::{
        CredentialClientFactory, CredentialStore,
        windows::credentials::{CredentialBackend, CredentialRecord, WindowsCredentialStore},
    },
    vectr::{client::UploadToken, uploader::ClientFactory},
};
use zeroize::Zeroizing;
#[derive(Clone, Copy, PartialEq)]
enum Cut {
    Read,
    Write,
}
type Gate = (Cut, mpsc::Sender<()>, mpsc::Receiver<()>);
#[derive(Default)]
struct Memory {
    entries: Mutex<HashMap<String, CredentialRecord>>,
    gate: Mutex<Option<Gate>>,
}
impl Memory {
    fn cut(&self, at: Cut) {
        let gate = {
            let mut gate = self.gate.lock().unwrap();
            if gate.as_ref().is_some_and(|g| g.0 == at) {
                gate.take()
            } else {
                None
            }
        };
        if let Some((_, arrived, release)) = gate {
            arrived.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    }
    fn arm(&self, at: Cut) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (arrived, rx) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        *self.gate.lock().unwrap() = Some((at, arrived, gate));
        (rx, release)
    }
}
fn copy_record(r: &CredentialRecord) -> CredentialRecord {
    CredentialRecord {
        target: r.target.clone(),
        owner: r.owner.clone(),
        kind: r.kind,
        persistence: r.persistence,
        flags: r.flags,
        attributes: r.attributes,
        blob: Zeroizing::new(r.blob.to_vec()),
    }
}
struct Backend(Arc<Memory>);
impl CredentialBackend for Backend {
    fn read(&self, target: &str) -> Result<Option<CredentialRecord>, AppError> {
        self.0.cut(Cut::Read);
        Ok(self.0.entries.lock().unwrap().get(target).map(copy_record))
    }
    fn write(&self, record: &CredentialRecord) -> Result<(), AppError> {
        self.0.cut(Cut::Write);
        self.0
            .entries
            .lock()
            .unwrap()
            .insert(record.target.clone(), copy_record(record));
        Ok(())
    }
    fn delete(&self, target: &str) -> Result<(), AppError> {
        self.0.entries.lock().unwrap().remove(target);
        Ok(())
    }
}
type Store = WindowsCredentialStore<Backend>;
fn token(s: &str) -> UploadToken {
    UploadToken::new(s.into()).unwrap()
}
struct NoopShell;
impl ShellActions for NoopShell {
    fn open_path(&self, _path: &std::path::Path) -> Result<(), AppError> {
        Ok(())
    }
    fn open_url(&self, _url: &str) -> Result<(), AppError> {
        Ok(())
    }
    fn reveal_path(&self, _path: &std::path::Path) -> Result<(), AppError> {
        Ok(())
    }
}
struct NoopClipboard;
impl ClipboardWriter for NoopClipboard {
    fn write_text(&self, _text: &str) -> Result<(), AppError> {
        Ok(())
    }
}
fn controller(root: &std::path::Path, store: Arc<Store>) -> AppController {
    AppController::open(
        Box::new(FileDestinationRepository::open(&root.join("settings.json")).unwrap()),
        Arc::new(History::open(&root.join("history.db")).unwrap()),
        store,
        Arc::new(ArtifactStore::new(root.join("spool")).unwrap()),
        Arc::new(NoopShell),
        Arc::new(NoopClipboard),
    )
    .unwrap()
}
fn seed(app: &mut AppController, name: &str, origin: ServerOrigin, value: &str) -> DestinationId {
    let revision = app.settings().revision;
    let mut destinations = app.settings().destinations.clone();
    let id = destinations.add(name, origin, None).unwrap();
    let effects = app.dispatch(AppCommand::ApplyDestinations {
        edit: DestinationEdit {
            destinations,
            tokens: vec![(id, token(value))],
        },
        revision,
    });
    assert!(matches!(effects.as_slice(), [AppEffect::SettingsSaved(_)]));
    id
}
fn wait(
    client: &vectrcap::app::desktop::DesktopClient,
    rx: &mpsc::Receiver<()>,
    condition: impl Fn(&DesktopView) -> bool,
) -> DesktopView {
    loop {
        let state = client.snapshot();
        if condition(&state) {
            return state;
        }
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }
}

#[test]
fn active_workflows_then_durable_jobs_protect_credentials_and_keep_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(Memory::default());
    let store = Arc::new(Store::with_backend(Backend(memory)));
    let mut app = controller(dir.path(), store);
    let id = seed(
        &mut app,
        "Personal",
        ServerOrigin::parse("https://example.invalid").unwrap(),
        "synthetic-a",
    );
    let workflow = uuid::Uuid::new_v4();
    assert!(matches!(
        app.dispatch(AppCommand::BeginWorkflow {
            id: workflow,
            selection: DestinationSelection::Specific(id)
        })
        .as_slice(),
        [AppEffect::WorkflowAccepted(_)]
    ));
    let replacement = |app: &AppController| AppCommand::ApplyDestinations {
        edit: DestinationEdit {
            destinations: app.settings().destinations.clone(),
            tokens: vec![(id, token("synthetic-b"))],
        },
        revision: app.settings().revision,
    };
    assert!(matches!(
        app.dispatch(replacement(&app)).as_slice(),
        [AppEffect::Error(AppError::InUse(_))]
    ));
    let mut renamed = app.settings().destinations.clone();
    renamed.rename(id, "Renamed").unwrap();
    app.dispatch(AppCommand::ApplyDestinations {
        edit: DestinationEdit {
            destinations: renamed,
            tokens: vec![],
        },
        revision: 1,
    });
    let source = dir.path().join("source.txt");
    std::fs::write(&source, b"synthetic file").unwrap();
    let artifact = ArtifactStore::new(dir.path().join("spool"))
        .unwrap()
        .stage_file(&source, ArtifactKind::File)
        .unwrap();
    let accepted = app.dispatch(AppCommand::AcceptWorkflow {
        id: workflow,
        artifact,
    });
    let [AppEffect::JobAccepted(job), AppEffect::Queue(_)] = accepted.as_slice() else {
        panic!("job not accepted");
    };
    assert_eq!(app.active_workflow_count(), 0);
    let history = History::open(&dir.path().join("history.db")).unwrap();
    assert_eq!(
        history.job(*job).unwrap().unwrap().destination.label,
        "Personal"
    );
    assert!(matches!(
        app.dispatch(replacement(&app)).as_slice(),
        [AppEffect::Error(AppError::InUse(_))]
    ));
}

#[test]
fn configured_launch_stays_in_tray_and_missing_same_origin_token_never_falls_back() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::with_backend(Backend(Arc::new(Memory::default()))));
    let mut app = controller(dir.path(), store.clone());
    let origin = ServerOrigin::parse("https://example.invalid").unwrap();
    let a = seed(&mut app, "One", origin.clone(), "synthetic-a");
    let b = seed(&mut app, "Two", origin, "synthetic-b");
    assert_ne!(
        app.settings().destinations.get(a).unwrap().credential_ref,
        app.settings().destinations.get(b).unwrap().credential_ref
    );
    store
        .delete(
            app.settings()
                .destinations
                .get(b)
                .unwrap()
                .credential_ref
                .as_ref()
                .unwrap(),
        )
        .unwrap();
    assert!(matches!(
        app.dispatch(AppCommand::BeginWorkflow {
            id: uuid::Uuid::new_v4(),
            selection: DestinationSelection::Specific(b)
        })
        .as_slice(),
        [AppEffect::Error(AppError::NeedsSetup(_))]
    ));
    assert_eq!(app.active_workflow_count(), 0);
    drop(app);
    let app = controller(dir.path(), store);
    assert_eq!(app.window, None);
    assert_eq!(app.settings().destinations.default_id(), Some(a));
}

#[test]
fn accept_versus_credential_mutation_is_serialized_in_both_orders() {
    for accept_first in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let memory = Arc::new(Memory::default());
        let store = Arc::new(Store::with_backend(Backend(memory.clone())));
        let mut app = controller(dir.path(), store.clone());
        let id = seed(
            &mut app,
            "Personal",
            ServerOrigin::parse("https://example.invalid").unwrap(),
            "synthetic-a",
        );
        let settings = app.settings().clone();
        drop(app);
        let history = History::open(&dir.path().join("history.db")).unwrap();
        history.set_queue_paused(true).unwrap();
        let source = dir.path().join("source.txt");
        std::fs::write(&source, b"synthetic").unwrap();
        let artifact = ArtifactStore::new(dir.path().join("spool"))
            .unwrap()
            .stage_file(&source, ArtifactKind::File)
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let runtime = DesktopRuntime::spawn(
            dir.path().into(),
            store.clone(),
            Arc::new(CredentialClientFactory::new(store)),
            Arc::new(move || {
                let _ = tx.send(());
            }),
        )
        .unwrap();
        wait(&runtime.client, &rx, |s| s.settings.is_some());
        let (arrived, release) = memory.arm(if accept_first { Cut::Read } else { Cut::Write });
        let workflow = uuid::Uuid::new_v4();
        let request = uuid::Uuid::new_v4();
        let begin = AppCommand::BeginWorkflow {
            id: workflow,
            selection: DestinationSelection::Specific(id),
        };
        let mutate = AppCommand::SaveConnection {
            request,
            edit: DestinationEdit {
                destinations: settings.destinations,
                tokens: vec![(id, token("synthetic-b"))],
            },
            revision: 1,
        };
        let (first, second) = if accept_first {
            (begin, mutate)
        } else {
            (mutate, begin)
        };
        runtime.client.submit(first).unwrap();
        arrived.recv_timeout(Duration::from_secs(5)).unwrap();
        runtime.client.submit(second).unwrap();
        runtime
            .client
            .submit(AppCommand::AcceptWorkflow {
                id: workflow,
                artifact,
            })
            .unwrap();
        assert_eq!(runtime.client.snapshot().settings.unwrap().revision, 1);
        release.send(()).unwrap();
        let state = wait(&runtime.client, &rx, |s| {
            !s.jobs.is_empty() && s.save_result.is_some()
        });
        let result = state.save_result.unwrap();
        assert_eq!(result.0, request);
        assert_eq!(
            state.jobs[0].destination.credential.generation(),
            if accept_first { 1 } else { 2 }
        );
        if accept_first {
            assert!(matches!(result.1, Err(AppError::InUse(_))));
        } else {
            assert!(result.1.is_ok());
        }
        assert_eq!(state.accepted_workflows, 0);
        runtime.client.submit(AppCommand::Quit).unwrap();
        wait(&runtime.client, &rx, |s| s.lifecycle == Lifecycle::Exited);
        assert_eq!(history.shell_jobs().unwrap().len(), 1);
        assert!(history.queue_paused().unwrap());
    }
}

#[test]
fn idle_queue_pause_and_resume_publish_durable_state_without_polling() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::with_backend(Backend(Arc::new(Memory::default()))));
    let (tx, rx) = mpsc::channel();
    let runtime = DesktopRuntime::spawn(
        dir.path().into(),
        store.clone(),
        Arc::new(CredentialClientFactory::new(store)),
        Arc::new(move || {
            let _ = tx.send(());
        }),
    )
    .unwrap();
    wait(&runtime.client, &rx, |s| s.settings.is_some());
    runtime
        .client
        .submit(AppCommand::Queue(QueueAction::PauseAll))
        .unwrap();
    let paused = wait(&runtime.client, &rx, |s| s.queue_paused);
    assert!(paused.queue_paused);
    runtime
        .client
        .submit(AppCommand::Queue(QueueAction::ResumeAll))
        .unwrap();
    let resumed = wait(&runtime.client, &rx, |s| !s.queue_paused);
    assert!(!resumed.queue_paused);
    runtime.client.submit(AppCommand::Quit).unwrap();
    wait(&runtime.client, &rx, |s| s.lifecycle == Lifecycle::Exited);
}

#[test]
fn explicit_quit_is_accepted_when_command_channel_is_full() {
    let dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(Memory::default());
    let store = Arc::new(Store::with_backend(Backend(memory.clone())));
    let mut app = controller(dir.path(), store.clone());
    seed(
        &mut app,
        "Personal",
        ServerOrigin::parse("https://example.invalid").unwrap(),
        "synthetic-a",
    );
    drop(app);
    let (tx, rx) = mpsc::channel();
    let runtime = DesktopRuntime::spawn(
        dir.path().into(),
        store.clone(),
        Arc::new(CredentialClientFactory::new(store)),
        Arc::new(move || {
            let _ = tx.send(());
        }),
    )
    .unwrap();
    wait(&runtime.client, &rx, |s| s.settings.is_some());
    let (arrived, release) = memory.arm(Cut::Read);
    runtime
        .client
        .submit(AppCommand::BeginWorkflow {
            id: uuid::Uuid::new_v4(),
            selection: DestinationSelection::Inherit,
        })
        .unwrap();
    arrived.recv_timeout(Duration::from_secs(5)).unwrap();
    for _ in 0..32 {
        runtime
            .client
            .submit(AppCommand::Show(Window::Settings))
            .unwrap();
    }
    assert!(
        runtime
            .client
            .submit(AppCommand::Show(Window::Settings))
            .is_err()
    );
    let quit_accepted = runtime.client.submit(AppCommand::Quit).is_ok();
    release.send(()).unwrap();
    if quit_accepted {
        wait(&runtime.client, &rx, |s| s.lifecycle == Lifecycle::Exited);
    }
    assert!(quit_accepted);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quit_drains_real_private_tls_request_and_preserves_confirmed_receipt() {
    use support::{Fixture, RECEIPT, Reply};
    let (reply, arrived, release) = Reply::json(200, RECEIPT).blocked();
    let server = Fixture::new(vec![reply]).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::with_backend(Backend(Arc::new(Memory::default()))));
    let mut app = controller(dir.path(), store.clone());
    let id = seed(&mut app, "Personal", server.origin.clone(), "synthetic-a");
    let workflow = uuid::Uuid::new_v4();
    app.dispatch(AppCommand::BeginWorkflow {
        id: workflow,
        selection: DestinationSelection::Specific(id),
    });
    let source = dir.path().join("empty.txt");
    std::fs::write(&source, []).unwrap();
    let artifact = ArtifactStore::new(dir.path().join("spool"))
        .unwrap()
        .stage_file(&source, ArtifactKind::File)
        .unwrap();
    app.dispatch(AppCommand::AcceptWorkflow {
        id: workflow,
        artifact,
    });
    drop(app);
    let (tx, rx) = mpsc::channel();
    let runtime = DesktopRuntime::spawn(
        dir.path().into(),
        store.clone(),
        Arc::new(CredentialClientFactory::with_root_certificate(
            store,
            server.root.clone(),
        )),
        Arc::new(move || {
            let _ = tx.send(());
        }),
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), arrived.notified())
        .await
        .unwrap();
    runtime.client.submit(AppCommand::Quit).unwrap();
    let state = wait(&runtime.client, &rx, |s| s.lifecycle == Lifecycle::Draining);
    assert_eq!(state.jobs[0].status, JobStatus::Running);
    release.add_permits(1);
    let state = wait(&runtime.client, &rx, |s| s.lifecycle == Lifecycle::Exited);
    assert!(matches!(state.jobs[0].status, JobStatus::Completed(_)));
    assert!(state.queue_paused);
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    assert_eq!(server.requests.lock().unwrap()[0].path, "/api/upload");
}

#[tokio::test]
async fn same_origin_controller_snapshots_resolve_their_own_tokens() {
    use support::{Fixture, Reply};
    let server = Fixture::new(vec![Reply::json(404, ""), Reply::json(404, "")]).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::with_backend(Backend(Arc::new(Memory::default()))));
    let mut app = controller(dir.path(), store.clone());
    let a = seed(&mut app, "One", server.origin.clone(), "synthetic-a");
    let b = seed(&mut app, "Two", server.origin.clone(), "synthetic-b");
    let factory = CredentialClientFactory::with_root_certificate(store, server.root.clone());
    for id in [a, b] {
        let snapshot = vectrcap::app::destinations::resolve_destination(
            DestinationSelection::Specific(id),
            DestinationSelection::Inherit,
            &app.settings().destinations,
        )
        .unwrap();
        factory
            .client(&snapshot)
            .unwrap()
            .uploader_info()
            .await
            .unwrap();
    }
    let requests = server.requests.lock().unwrap();
    assert!(requests[0].headers.contains("authorization: synthetic-a"));
    assert!(requests[1].headers.contains("authorization: synthetic-b"));
}

/// Simulates the desktop staging driver for a controller-level test: calls
/// the real `ArtifactStore::stage_file` (or passes through its real error
/// for a bad path) and feeds the outcome into `on_event`, exactly like
/// `src/app/desktop.rs`'s staging task does.
fn stage_and_feed(
    app: &mut AppController,
    artifacts: &ArtifactStore,
    batch: uuid::Uuid,
    index: usize,
    path: &std::path::Path,
) -> Vec<AppEffect> {
    let display_name = path.file_name().unwrap().to_string_lossy().into_owned();
    let outcome = artifacts.stage_file(path, ArtifactKind::File);
    app.on_event(AppEvent::FileStaged {
        batch,
        index,
        display_name,
        outcome,
    })
}

#[test]
fn file_selection_batch_keeps_one_destination_across_a_default_change_during_staging() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::with_backend(Backend(Arc::new(Memory::default()))));
    let mut app = controller(dir.path(), store.clone());
    let first = seed(
        &mut app,
        "Original",
        ServerOrigin::parse("https://example.invalid").unwrap(),
        "synthetic-a",
    );
    let artifacts = ArtifactStore::new(dir.path().join("spool")).unwrap();
    let source_a = dir.path().join("a.txt");
    let source_b = dir.path().join("b.txt");
    std::fs::write(&source_a, b"file a").unwrap();
    std::fs::write(&source_b, b"file b").unwrap();

    let effects = app.dispatch(AppCommand::AcceptFileSelection(vec![
        source_a.clone(),
        source_b.clone(),
    ]));
    let batch = effects
        .iter()
        .find_map(|e| match e {
            AppEffect::StageFiles { batch, paths } => {
                assert_eq!(paths, &[source_a.clone(), source_b.clone()]);
                Some(*batch)
            }
            _ => None,
        })
        .expect("file selection accepted");
    assert_eq!(app.active_workflow_count(), 1);

    // Stage the first file, then change the default destination before the
    // second file is staged. Both files must still use the ORIGINAL snapshot.
    let effects_a = stage_and_feed(&mut app, &artifacts, batch, 0, &source_a);
    assert!(
        effects_a
            .iter()
            .any(|e| matches!(e, AppEffect::JobAccepted(_)))
    );
    // Still reserved: the second file has not finished staging yet.
    assert_eq!(app.active_workflow_count(), 1);

    let second = seed(
        &mut app,
        "Later default",
        ServerOrigin::parse("https://later.invalid").unwrap(),
        "synthetic-b",
    );
    app.dispatch(AppCommand::SetDefault {
        id: second,
        revision: app.settings().revision,
    });
    assert_eq!(app.settings().destinations.default_id(), Some(second));

    let effects_b = stage_and_feed(&mut app, &artifacts, batch, 1, &source_b);
    assert!(
        effects_b
            .iter()
            .any(|e| matches!(e, AppEffect::JobAccepted(_)))
    );
    // Staging is done for the whole batch; the reservation is released.
    assert_eq!(app.active_workflow_count(), 0);

    let history = History::open(&dir.path().join("history.db")).unwrap();
    let jobs = history.shell_jobs().unwrap();
    assert_eq!(jobs.len(), 2);
    for job in &jobs {
        assert_eq!(
            job.destination.id, first,
            "must keep the batch's original snapshot"
        );
        assert_eq!(job.destination.label, "Original");
    }
    // Original user files are never modified by staging.
    assert_eq!(std::fs::read(&source_a).unwrap(), b"file a");
    assert_eq!(std::fs::read(&source_b).unwrap(), b"file b");
}

#[test]
fn bad_file_alongside_valid_file_in_a_batch_still_uploads_the_valid_one() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::with_backend(Backend(Arc::new(Memory::default()))));
    let mut app = controller(dir.path(), store);
    seed(
        &mut app,
        "Personal",
        ServerOrigin::parse("https://example.invalid").unwrap(),
        "synthetic-a",
    );
    let artifacts = ArtifactStore::new(dir.path().join("spool")).unwrap();
    let good = dir.path().join("good.txt");
    std::fs::write(&good, b"valid file").unwrap();
    let missing = dir.path().join("does-not-exist.txt");

    let effects = app.dispatch(AppCommand::AcceptFileSelection(vec![
        good.clone(),
        missing.clone(),
    ]));
    let batch = effects
        .iter()
        .find_map(|e| match e {
            AppEffect::StageFiles { batch, .. } => Some(*batch),
            _ => None,
        })
        .unwrap();

    let good_effects = stage_and_feed(&mut app, &artifacts, batch, 0, &good);
    let job_id = good_effects
        .iter()
        .find_map(|e| match e {
            AppEffect::JobAccepted(id) => Some(*id),
            _ => None,
        })
        .expect("the valid sibling file is still accepted");

    let bad_effects = stage_and_feed(&mut app, &artifacts, batch, 1, &missing);
    assert!(matches!(
        bad_effects.as_slice(),
        [AppEffect::FileRejected { display_name, .. }] if display_name == "does-not-exist.txt"
    ));

    // The batch fully resolved: no leaked workflow reservation.
    assert_eq!(app.active_workflow_count(), 0);
    let history = History::open(&dir.path().join("history.db")).unwrap();
    let jobs = history.shell_jobs().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, job_id);
    assert_eq!(std::fs::read(&good).unwrap(), b"valid file");
}

#[test]
fn file_selection_without_a_configured_destination_fails_before_touching_any_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::with_backend(Backend(Arc::new(Memory::default()))));
    let mut app = controller(dir.path(), store);
    let path = dir.path().join("a.txt");
    std::fs::write(&path, b"content").unwrap();
    let effects = app.dispatch(AppCommand::AcceptFileSelection(vec![path.clone()]));
    assert!(matches!(
        effects.as_slice(),
        [AppEffect::Error(AppError::NeedsSetup(_))]
    ));
    assert_eq!(app.active_workflow_count(), 0);
    // Nothing was staged: the source still has its original bytes, and no
    // spool directory was ever populated.
    assert_eq!(std::fs::read(&path).unwrap(), b"content");
    assert!(
        !dir.path().join("spool").exists()
            || std::fs::read_dir(dir.path().join("spool"))
                .unwrap()
                .next()
                .is_none()
    );
}

#[test]
fn file_selection_bounds_batch_size() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::with_backend(Backend(Arc::new(Memory::default()))));
    let mut app = controller(dir.path(), store);
    seed(
        &mut app,
        "Personal",
        ServerOrigin::parse("https://example.invalid").unwrap(),
        "synthetic-a",
    );
    let too_many = (0..101)
        .map(|i| dir.path().join(format!("f{i}.txt")))
        .collect();
    assert!(matches!(
        app.dispatch(AppCommand::AcceptFileSelection(too_many))
            .as_slice(),
        [AppEffect::Error(AppError::InvalidInput(_))]
    ));
    assert_eq!(app.active_workflow_count(), 0);
    assert!(matches!(
        app.dispatch(AppCommand::AcceptFileSelection(vec![]))
            .as_slice(),
        [AppEffect::Error(AppError::InvalidInput(_))]
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_file_batch_settles_two_files_through_a_fixture_tls_server() {
    use support::{Fixture, RECEIPT, Reply};
    let server = Fixture::new(vec![Reply::json(200, RECEIPT), Reply::json(200, RECEIPT)]).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::with_backend(Backend(Arc::new(Memory::default()))));
    let mut app = controller(dir.path(), store.clone());
    seed(&mut app, "Personal", server.origin.clone(), "synthetic-a");
    drop(app);
    let first = dir.path().join("first.txt");
    let second = dir.path().join("second.txt");
    // Empty files use the single-request multipart path, keeping this fixture
    // to one reply per file instead of a full create/put/complete sequence.
    std::fs::write(&first, []).unwrap();
    std::fs::write(&second, []).unwrap();
    let (tx, rx) = mpsc::channel();
    let runtime = DesktopRuntime::spawn(
        dir.path().into(),
        store.clone(),
        Arc::new(CredentialClientFactory::with_root_certificate(
            store,
            server.root.clone(),
        )),
        Arc::new(move || {
            let _ = tx.send(());
        }),
    )
    .unwrap();
    wait(&runtime.client, &rx, |s| s.settings.is_some());
    runtime
        .client
        .submit(AppCommand::AcceptFileSelection(vec![first, second]))
        .unwrap();
    let state = wait(&runtime.client, &rx, |s| {
        s.jobs.len() == 2 && s.jobs.iter().all(|j| j.status.is_terminal())
    });
    assert!(
        state
            .jobs
            .iter()
            .all(|j| matches!(j.status, JobStatus::Completed(_)))
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    runtime.client.submit(AppCommand::Quit).unwrap();
    wait(&runtime.client, &rx, |s| s.lifecycle == Lifecycle::Exited);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quitting_during_staging_never_orphans_an_accepted_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::with_backend(Backend(Arc::new(Memory::default()))));
    let mut app = controller(dir.path(), store.clone());
    seed(
        &mut app,
        "Personal",
        ServerOrigin::parse("https://example.invalid").unwrap(),
        "synthetic-a",
    );
    drop(app);
    let path = dir.path().join("during-quit.txt");
    std::fs::write(&path, b"synthetic").unwrap();
    let (tx, rx) = mpsc::channel();
    let runtime = DesktopRuntime::spawn(
        dir.path().into(),
        store.clone(),
        Arc::new(CredentialClientFactory::new(store)),
        Arc::new(move || {
            let _ = tx.send(());
        }),
    )
    .unwrap();
    wait(&runtime.client, &rx, |s| s.settings.is_some());
    runtime
        .client
        .submit(AppCommand::AcceptFileSelection(vec![path]))
        .unwrap();
    runtime.client.submit(AppCommand::Quit).unwrap();
    wait(&runtime.client, &rx, |s| s.lifecycle == Lifecycle::Exited);
    // Whatever landed in the spool must have a durable job record: staging
    // and quitting concurrently must never leave an orphaned spool file, and
    // must never silently lose the accepted file either.
    let history = History::open(&dir.path().join("history.db")).unwrap();
    let jobs = history.shell_jobs().unwrap();
    let spool_dir = dir.path().join("spool");
    let spool_files = std::fs::read_dir(&spool_dir)
        .map(|entries| entries.filter_map(|e| e.ok()).count())
        .unwrap_or(0);
    assert!(jobs.len() <= 1 && spool_files <= 1);
    assert_eq!(jobs.len(), spool_files);
}

#[test]
fn staged_success_delivered_after_quit_keeps_its_reserved_destination_and_durable_job() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::with_backend(Backend(Arc::new(Memory::default()))));
    let mut app = controller(dir.path(), store);
    let destination = seed(
        &mut app,
        "Original",
        ServerOrigin::parse("https://example.invalid").unwrap(),
        "synthetic-token",
    );
    let artifacts = ArtifactStore::new(dir.path().join("spool")).unwrap();
    let source = dir.path().join("accepted-before-quit.txt");
    std::fs::write(&source, b"synthetic").unwrap();
    let effects = app.dispatch(AppCommand::AcceptFileSelection(vec![source.clone()]));
    let batch = effects
        .into_iter()
        .find_map(|e| {
            if let AppEffect::StageFiles { batch, .. } = e {
                Some(batch)
            } else {
                None
            }
        })
        .unwrap();
    let artifact = artifacts.stage_file(&source, ArtifactKind::File).unwrap();
    app.dispatch(AppCommand::Quit);
    let effects = app.on_event(AppEvent::FileStaged {
        batch,
        index: 0,
        display_name: "accepted-before-quit.txt".into(),
        outcome: Ok(artifact),
    });
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, AppEffect::JobAccepted(_))),
        "successful staging accepted before Quit must retain its destination reservation and durably commit"
    );
    let jobs = History::open(&dir.path().join("history.db"))
        .unwrap()
        .shell_jobs()
        .unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].destination.id, destination);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_staging_backlog_gets_explicit_busy_feedback_without_losing_any_file() {
    // Exceeds the staging pool's bounded pending backlog (64) within one
    // accepted batch: beyond that cap, a file must be bounced with an
    // explicit busy outcome instead of growing an unbounded queue of open
    // source handles — never silently dropped and never stuck forever.
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::with_backend(Backend(Arc::new(Memory::default()))));
    let mut app = controller(dir.path(), store.clone());
    seed(
        &mut app,
        "Personal",
        ServerOrigin::parse("https://example.invalid").unwrap(),
        "synthetic-a",
    );
    drop(app);
    const TOTAL: usize = 80;
    let paths: Vec<_> = (0..TOTAL)
        .map(|i| {
            let p = dir.path().join(format!("f{i}.txt"));
            std::fs::write(&p, b"x").unwrap();
            p
        })
        .collect();
    let (tx, rx) = mpsc::channel();
    let runtime = DesktopRuntime::spawn(
        dir.path().into(),
        store.clone(),
        Arc::new(CredentialClientFactory::new(store)),
        Arc::new(move || {
            let _ = tx.send(());
        }),
    )
    .unwrap();
    wait(&runtime.client, &rx, |s| s.settings.is_some());
    runtime
        .client
        .submit(AppCommand::AcceptFileSelection(paths))
        .unwrap();
    // Staging a handful of tiny local files never touches the network, so
    // this resolves independently of any upload outcome.
    let state = wait(&runtime.client, &rx, |s| {
        s.jobs.len() + s.failed_uploads.len() >= TOTAL
    });
    assert_eq!(
        state.jobs.len() + state.failed_uploads.len(),
        TOTAL,
        "every selected file must resolve: a durable job or an explicit rejection, never lost"
    );
    assert!(
        state
            .failed_uploads
            .iter()
            .any(|(_, e)| matches!(e, AppError::InUse(_))),
        "the backlog beyond the bounded pending cap must surface an explicit busy rejection"
    );
    runtime.client.submit(AppCommand::Quit).unwrap();
    wait(&runtime.client, &rx, |s| s.lifecycle == Lifecycle::Exited);
}
