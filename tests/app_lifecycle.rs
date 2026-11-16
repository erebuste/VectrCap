mod desktop_support;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use vectrcap::{
    app::{
        controller::AppController,
        destination_manager::{DestinationEdit, FileDestinationRepository},
        destinations::{CredentialKey, DestinationSelection},
        error::AppError,
        events::*,
    },
    persistence::{history::History, settings::ServerOrigin},
    platform::{
        CredentialStore,
        windows::{clipboard::ClipboardWriter, shell_actions::ShellActions},
    },
    vectr::client::UploadToken,
};

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

#[derive(Default)]
struct Store(Mutex<HashMap<CredentialKey, String>>);
impl CredentialStore for Store {
    fn read(&self, key: &CredentialKey) -> Result<UploadToken, AppError> {
        self.0
            .lock()
            .unwrap()
            .get(key)
            .map(|s| UploadToken::new(s.clone()).unwrap())
            .ok_or(AppError::NeedsSetup("missing exact key"))
    }
    fn write(&self, key: &CredentialKey, _: &UploadToken) -> Result<(), AppError> {
        self.0
            .lock()
            .unwrap()
            .insert(key.clone(), "synthetic-token".into());
        Ok(())
    }
    fn delete(&self, key: &CredentialKey) -> Result<(), AppError> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}
fn app() -> (tempfile::TempDir, AppController) {
    let dir = tempfile::tempdir().unwrap();
    let history = Arc::new(History::open(&dir.path().join("history.db")).unwrap());
    let store = Arc::new(Store::default());
    let artifacts = Arc::new(
        vectrcap::persistence::artifacts::ArtifactStore::new(dir.path().join("spool")).unwrap(),
    );
    let app = AppController::open(
        Box::new(FileDestinationRepository::open(&dir.path().join("settings.json")).unwrap()),
        history,
        store,
        artifacts,
        Arc::new(NoopShell),
        Arc::new(NoopClipboard),
    )
    .unwrap();
    (dir, app)
}
#[test]
fn first_run_opens_setup_close_keeps_tray_alive() {
    let (_dir, mut app) = app();
    assert_eq!(app.window, Some(Window::Settings));
    app.dispatch(AppCommand::CloseWindow);
    assert_eq!(app.window, None);
    assert_eq!(app.lifecycle, Lifecycle::Running);
    assert!(matches!(
        app.dispatch(AppCommand::Show(Window::Uploads)).as_slice(),
        [
            AppEffect::Window(Some(Window::Uploads)),
            AppEffect::CancelConnectionTest,
            AppEffect::StopAudioTest
        ]
    ));
}
#[test]
fn quit_waits_for_worker_drain_and_rejects_new_work() {
    let (_dir, mut app) = app();
    assert!(matches!(
        app.dispatch(AppCommand::Quit).as_slice(),
        [AppEffect::StopAudioTest, AppEffect::DrainWorker]
    ));
    assert_eq!(app.lifecycle, Lifecycle::Draining);
    assert!(matches!(
        app.dispatch(AppCommand::Show(Window::Settings)).as_slice(),
        [AppEffect::Error(_)]
    ));
    assert!(app.dispatch(AppCommand::Quit).is_empty());
    assert!(matches!(
        app.on_event(AppEvent::WorkerStopped(Ok(()))).as_slice(),
        [AppEffect::Exit]
    ));
    assert_eq!(app.lifecycle, Lifecycle::Exited);
}
#[test]
fn stale_settings_save_conflicts_without_changing_revision() {
    let (_dir, mut app) = app();
    let mut destinations = app.settings().destinations.clone();
    destinations
        .add(
            "Personal",
            ServerOrigin::parse("https://example.invalid").unwrap(),
            None,
        )
        .unwrap();
    let effects = app.dispatch(AppCommand::ApplyDestinations {
        edit: DestinationEdit {
            destinations: destinations.clone(),
            tokens: vec![],
        },
        revision: 0,
    });
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, AppEffect::SettingsSaved(_)))
    );
    assert!(matches!(
        app.dispatch(AppCommand::ApplyDestinations {
            edit: DestinationEdit {
                destinations,
                tokens: vec![]
            },
            revision: 0
        })
        .as_slice(),
        [AppEffect::Error(_)]
    ));
    assert_eq!(app.settings().revision, 1);
}
#[test]
fn missing_credentials_never_accept_work_or_fall_back() {
    let (_dir, mut app) = app();
    assert!(matches!(
        app.dispatch(AppCommand::BeginWorkflow {
            id: uuid::Uuid::new_v4(),
            selection: DestinationSelection::Inherit
        })
        .as_slice(),
        [AppEffect::Error(AppError::NeedsSetup(_))]
    ));
}

#[test]
fn second_instance_exits_cleanly_and_requests_focus() {
    use vectrcap::platform::windows::instance::InstanceGuard;
    let namespace = format!("VectrCap-TEST-{}", uuid::Uuid::new_v4());
    let (tx, rx) = std::sync::mpsc::channel();
    let first = InstanceGuard::acquire(
        &namespace,
        Arc::new(move || {
            tx.send(()).unwrap();
        }),
    )
    .unwrap()
    .unwrap();
    assert!(
        InstanceGuard::acquire(&namespace, Arc::new(|| {}))
            .unwrap()
            .is_none()
    );
    rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    drop(first);
    assert!(
        InstanceGuard::acquire(&namespace, Arc::new(|| {}))
            .unwrap()
            .is_some()
    );
}

#[test]
fn actor_initializes_off_thread_and_quit_drains_without_ui_polling() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::default());
    let factory = Arc::new(vectrcap::platform::CredentialClientFactory::new(
        store.clone(),
    ));
    let (tx, rx) = std::sync::mpsc::channel();
    let runtime = desktop_support::spawn(
        dir.path().to_owned(),
        store,
        factory,
        Arc::new(move || {
            let _ = tx.send(());
        }),
    )
    .unwrap();
    rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    assert!(runtime.client.snapshot().settings.is_some());
    runtime.client.submit(AppCommand::Quit).unwrap();
    loop {
        rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        if runtime.client.snapshot().lifecycle == Lifecycle::Exited {
            break;
        }
    }
    let history = History::open(&dir.path().join("history.db")).unwrap();
    assert!(history.queue_paused().unwrap());
}

#[test]
fn failed_initialization_still_accepts_explicit_quit() {
    let dir = tempfile::tempdir().unwrap();
    let _lease = FileDestinationRepository::open(&dir.path().join("settings.json")).unwrap();
    let store = Arc::new(Store::default());
    let factory = Arc::new(vectrcap::platform::CredentialClientFactory::new(
        store.clone(),
    ));
    let (tx, rx) = std::sync::mpsc::channel();
    let runtime = desktop_support::spawn(
        dir.path().to_owned(),
        store,
        factory,
        Arc::new(move || {
            let _ = tx.send(());
        }),
    )
    .unwrap();
    rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    assert_eq!(runtime.client.snapshot().lifecycle, Lifecycle::Exited);
    assert!(runtime.client.snapshot().error.is_some());
    assert!(runtime.client.submit(AppCommand::Quit).is_ok());
}
