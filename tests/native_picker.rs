//! Deterministic lifecycle coverage for the native file picker boundary
//! (`spawn_pick_with`/`PickerGate`), without ever opening a real dialog. The
//! existing `#[ignore]`d smoke tests in `file_dialog.rs` are manual-only and
//! prove nothing on their own; these tests exercise the same gating logic
//! through the injectable picker closure instead.
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use vectrcap::{
    app::{
        desktop::DesktopRuntime,
        error::AppError,
        events::{AppCommand, Lifecycle},
    },
    platform::{
        CredentialClientFactory,
        windows::{
            credentials::{CredentialBackend, CredentialRecord, WindowsCredentialStore},
            file_dialog::{PickerGate, spawn_pick_with},
        },
    },
};

fn copy_record(r: &CredentialRecord) -> CredentialRecord {
    CredentialRecord {
        target: r.target.clone(),
        owner: r.owner.clone(),
        kind: r.kind,
        persistence: r.persistence,
        flags: r.flags,
        attributes: r.attributes,
        blob: zeroize::Zeroizing::new(r.blob.to_vec()),
    }
}
#[derive(Default)]
struct Memory(Mutex<HashMap<String, CredentialRecord>>);
impl CredentialBackend for Memory {
    fn read(&self, target: &str) -> Result<Option<CredentialRecord>, AppError> {
        Ok(self.0.lock().unwrap().get(target).map(copy_record))
    }
    fn write(&self, record: &CredentialRecord) -> Result<(), AppError> {
        self.0
            .lock()
            .unwrap()
            .insert(record.target.clone(), copy_record(record));
        Ok(())
    }
    fn delete(&self, target: &str) -> Result<(), AppError> {
        self.0.lock().unwrap().remove(target);
        Ok(())
    }
}
type Store = WindowsCredentialStore<Memory>;

async fn runtime() -> (DesktopRuntime, std::sync::mpsc::Receiver<()>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::with_backend(Memory::default()));
    let (tx, rx) = std::sync::mpsc::channel();
    let runtime = DesktopRuntime::spawn(
        dir.path().into(),
        store.clone(),
        Arc::new(CredentialClientFactory::new(store)),
        Arc::new(move || {
            let _ = tx.send(());
        }),
    )
    .unwrap();
    // Keep the tempdir alive for the runtime's lifetime by leaking it: these
    // tests never read History/spool, only picker-level gating.
    std::mem::forget(dir);
    (runtime, rx)
}

fn wait(
    client: &vectrcap::app::desktop::DesktopClient,
    rx: &std::sync::mpsc::Receiver<()>,
    condition: impl Fn(&vectrcap::app::desktop::DesktopView) -> bool,
) -> vectrcap::app::desktop::DesktopView {
    loop {
        let state = client.snapshot();
        if condition(&state) {
            return state;
        }
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_flight_rejects_a_concurrent_picker_while_one_is_open() {
    let (runtime, rx) = runtime().await;
    wait(&runtime.client, &rx, |s| s.settings.is_some());
    let gate = PickerGate::default();
    let (arrived_tx, arrived_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    spawn_pick_with(runtime.client.clone(), gate.clone(), move || {
        arrived_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        Ok(None)
    });
    arrived_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // A second picker while the first is still open must be rejected
    // synchronously, and its closure must never run.
    let second_invoked = Arc::new(AtomicBool::new(false));
    let flag = second_invoked.clone();
    spawn_pick_with(runtime.client.clone(), gate.clone(), move || {
        flag.store(true, Ordering::SeqCst);
        Ok(None)
    });
    assert!(
        !second_invoked.load(Ordering::SeqCst),
        "a concurrent picker must never run while one is already open"
    );
    assert!(matches!(
        runtime.client.snapshot().error,
        Some(AppError::InUse(_))
    ));
    release_tx.send(()).unwrap();
    // Once the first picker's thread finishes, its guard drops and the gate
    // is free again: a third picker must actually run (not be rejected as
    // busy). The guard's drop races the check below only by however long the
    // OS takes to finish that already-returning thread, so poll briefly.
    let third_invoked = Arc::new(AtomicBool::new(false));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline && !third_invoked.load(Ordering::SeqCst) {
        let flag = third_invoked.clone();
        spawn_pick_with(runtime.client.clone(), gate.clone(), move || {
            flag.store(true, Ordering::SeqCst);
            Ok(None)
        });
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        third_invoked.load(Ordering::SeqCst),
        "the gate must release once the first picker completes"
    );
    runtime.client.submit(AppCommand::Quit).unwrap();
    wait(&runtime.client, &rx, |s| s.lifecycle == Lifecycle::Exited);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_picker_is_never_opened_once_shutdown_has_begun() {
    let (runtime, rx) = runtime().await;
    wait(&runtime.client, &rx, |s| s.settings.is_some());
    runtime.client.submit(AppCommand::Quit).unwrap();
    wait(&runtime.client, &rx, |s| s.lifecycle != Lifecycle::Running);
    let gate = PickerGate::default();
    let invoked = Arc::new(AtomicBool::new(false));
    let flag = invoked.clone();
    // No thread is spawned at all once shutdown has begun, so this closure
    // can never run — not a race, a synchronous check on the calling thread.
    spawn_pick_with(runtime.client.clone(), gate.clone(), move || {
        flag.store(true, Ordering::SeqCst);
        Ok(Some(vec![std::env::temp_dir().join("late-selection.txt")]))
    });
    assert!(
        !invoked.load(Ordering::SeqCst),
        "the picker must never open once Quit/Draining has begun"
    );
    // A refused-before-open attempt must not leave the gate stuck busy: a
    // second attempt on the same (already-shutting-down) client is refused
    // the same way, not by the gate.
    let second_invoked = Arc::new(AtomicBool::new(false));
    let flag = second_invoked.clone();
    spawn_pick_with(runtime.client.clone(), gate.clone(), move || {
        flag.store(true, Ordering::SeqCst);
        Ok(None)
    });
    assert!(!second_invoked.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_picker_submits_nothing() {
    let (runtime, rx) = runtime().await;
    wait(&runtime.client, &rx, |s| s.settings.is_some());
    let gate = PickerGate::default();
    spawn_pick_with(runtime.client.clone(), gate.clone(), || Ok(None));
    // Give the detached thread a moment to run and (not) submit anything.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(runtime.client.snapshot().error, None);
    let reused = Arc::new(AtomicBool::new(false));
    let flag = reused.clone();
    spawn_pick_with(runtime.client.clone(), gate.clone(), move || {
        flag.store(true, Ordering::SeqCst);
        Ok(None)
    });
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        reused.load(Ordering::SeqCst),
        "the gate must be reusable after a plain cancel"
    );
    runtime.client.submit(AppCommand::Quit).unwrap();
    wait(&runtime.client, &rx, |s| s.lifecycle == Lifecycle::Exited);
}
