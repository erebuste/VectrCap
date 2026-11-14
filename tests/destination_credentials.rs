use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};
use vectrcap::{
    app::{
        destinations::{CredentialKey, Destinations},
        error::AppError,
    },
    persistence::settings::ServerOrigin,
    platform::{
        CredentialStore,
        windows::credentials::{CredentialBackend, CredentialRecord, WindowsCredentialStore},
    },
    vectr::client::UploadToken,
};
use zeroize::Zeroizing;

#[derive(Default)]
struct Memory {
    entries: Mutex<HashMap<String, CredentialRecord>>,
    fail_write: Mutex<bool>,
    fail_delete: Mutex<bool>,
    reads: AtomicUsize,
    fail_read: AtomicBool,
}
struct Backend(Arc<Memory>);
impl std::ops::Deref for Backend {
    type Target = Memory;
    fn deref(&self) -> &Memory {
        &self.0
    }
}
impl CredentialBackend for Backend {
    fn read(&self, target: &str) -> Result<Option<CredentialRecord>, AppError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.fail_read.load(Ordering::SeqCst) {
            return Err(AppError::Credential("protected store unavailable"));
        }
        Ok(self
            .entries
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
                blob: Zeroizing::new(r.blob.to_vec()),
            }))
    }
    fn write(&self, r: &CredentialRecord) -> Result<(), AppError> {
        if *self.fail_write.lock().unwrap() {
            return Err(AppError::Credential("store unavailable"));
        }
        self.entries.lock().unwrap().insert(
            r.target.clone(),
            CredentialRecord {
                target: r.target.clone(),
                owner: r.owner.clone(),
                kind: r.kind,
                persistence: r.persistence,
                flags: r.flags,
                attributes: r.attributes,
                blob: Zeroizing::new(r.blob.to_vec()),
            },
        );
        Ok(())
    }
    fn delete(&self, target: &str) -> Result<(), AppError> {
        if *self.fail_delete.lock().unwrap() {
            return Err(AppError::Credential("store unavailable"));
        }
        self.entries.lock().unwrap().remove(target);
        Ok(())
    }
}
fn token(s: &str) -> UploadToken {
    UploadToken::new(s.to_owned()).unwrap()
}
fn keys() -> (CredentialKey, CredentialKey) {
    let mut d = Destinations::default();
    let a = d
        .add(
            "one",
            ServerOrigin::parse("https://vectr.example").unwrap(),
            None,
        )
        .unwrap();
    let b = d
        .add(
            "two",
            ServerOrigin::parse("https://vectr.example").unwrap(),
            None,
        )
        .unwrap();
    (
        d.bind_credential(a, &HashSet::new()).unwrap(),
        d.bind_credential(b, &HashSet::new()).unwrap(),
    )
}
#[test]
fn same_origin_tokens_are_separate_and_existing_entries_cannot_be_overwritten() {
    let backend = Arc::new(Memory::default());
    let store = WindowsCredentialStore::with_backend(Backend(backend.clone()));
    let (a, b) = keys();
    store.write(&a, &token("synthetic-one")).unwrap();
    store.write(&b, &token("synthetic-two")).unwrap();
    assert_eq!(backend.entries.lock().unwrap().len(), 2);
    assert!(store.write(&a, &token("replacement")).is_err());
    store.delete(&a).unwrap();
    assert!(matches!(store.read(&a), Err(AppError::NeedsSetup(_))));
    assert!(store.read(&b).is_ok());
}
#[test]
fn encoded_capacity_is_2560_bytes_without_truncation() {
    let backend = Arc::new(Memory::default());
    let store = WindowsCredentialStore::with_backend(Backend(backend.clone()));
    let (a, b) = keys();
    store.write(&a, &token(&"x".repeat(2560))).unwrap();
    assert!(store.write(&b, &token(&"y".repeat(2561))).is_err());
    assert_eq!(backend.entries.lock().unwrap().len(), 1);
    let entries = backend.entries.lock().unwrap();
    assert_eq!(entries.values().next().unwrap().blob.len(), 2560);
}
#[test]
fn corrupt_blob_or_foreign_metadata_is_never_used_or_deleted() {
    let backend = Arc::new(Memory::default());
    let store = WindowsCredentialStore::with_backend(Backend(backend.clone()));
    let (a, _) = keys();
    store.write(&a, &token("synthetic")).unwrap();
    for blob in [vec![], vec![0xff], vec![0], vec![b' '; 4], vec![b'x'; 2561]] {
        backend
            .entries
            .lock()
            .unwrap()
            .values_mut()
            .next()
            .unwrap()
            .blob = Zeroizing::new(blob);
        assert!(store.read(&a).is_err());
    }
    backend
        .entries
        .lock()
        .unwrap()
        .values_mut()
        .next()
        .unwrap()
        .owner = "foreign".into();
    assert!(store.delete(&a).is_err());
    assert_eq!(backend.entries.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn factory_uses_only_snapshot_key_and_rejects_wrong_origin_before_store_or_network() {
    use vectrcap::{
        app::destinations::{DestinationSelection, resolve_destination},
        platform::CredentialClientFactory,
        vectr::uploader::ClientFactory,
    };
    let fixture = support::Fixture::new(vec![
        support::Reply::json(401, "{}"),
        support::Reply::json(401, "{}"),
    ])
    .await;
    let backend = Arc::new(Memory::default());
    let store = Arc::new(WindowsCredentialStore::with_backend(Backend(
        backend.clone(),
    )));
    let mut ds = Destinations::default();
    let a = ds.add("one", fixture.origin.clone(), None).unwrap();
    let b = ds.add("two", fixture.origin.clone(), None).unwrap();
    let ka = ds.bind_credential(a, &HashSet::new()).unwrap();
    let kb = ds.bind_credential(b, &HashSet::new()).unwrap();
    store.write(&ka, &token("synthetic-first")).unwrap();
    store.write(&kb, &token("synthetic-second")).unwrap();
    let snap = |id| {
        resolve_destination(
            DestinationSelection::Specific(id),
            DestinationSelection::Inherit,
            &ds,
        )
        .unwrap()
    };
    let factory =
        CredentialClientFactory::with_root_certificate(store.clone(), fixture.root.clone());
    for id in [a, b] {
        assert!(
            factory
                .client(&snap(id))
                .unwrap()
                .uploader_info()
                .await
                .is_err()
        );
    }
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].headers.contains("synthetic-first"));
    assert!(!requests[0].headers.contains("synthetic-second"));
    assert!(requests[1].headers.contains("synthetic-second"));
    drop(requests);
    let reads = backend.reads.load(Ordering::SeqCst);
    let mut bad = snap(a);
    bad.origin = ServerOrigin::parse("https://wrong.example").unwrap();
    assert!(factory.client(&bad).is_err());
    bad = snap(a);
    bad.id = b;
    assert!(factory.client(&bad).is_err());
    assert_eq!(backend.reads.load(Ordering::SeqCst), reads);
    store.delete(&ka).unwrap();
    assert!(matches!(
        factory.client(&snap(a)),
        Err(AppError::NeedsSetup(_))
    ));
    assert!(factory.client(&snap(b)).is_ok());
    backend
        .entries
        .lock()
        .unwrap()
        .values_mut()
        .next()
        .unwrap()
        .blob = Zeroizing::new(vec![0xff]);
    assert!(matches!(
        factory.client(&snap(b)),
        Err(AppError::Credential(_))
    ));
}
mod support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use vectrcap::app::{
    destination_manager::{
        CredentialJournal, DestinationEdit, DestinationManager, DestinationReferences,
        DestinationRepository, FileDestinationRepository,
    },
    destinations::{DestinationId, DestinationSelection, resolve_destination},
};
use vectrcap::persistence::{history::History, settings::Settings};
#[derive(Default)]
struct RefState {
    active: Mutex<HashSet<DestinationId>>,
    configured: Mutex<HashSet<DestinationId>>,
    fail: AtomicBool,
}
struct References(Arc<RefState>);
impl DestinationReferences for References {
    fn active_workflows(&self) -> Result<HashSet<DestinationId>, AppError> {
        if self.0.fail.load(Ordering::SeqCst) {
            return Err(AppError::Persistence("workflow references unavailable"));
        }
        Ok(self.0.active.lock().unwrap().clone())
    }
    fn validate_configured(&self, candidate: &Settings) -> Result<(), AppError> {
        if self
            .0
            .configured
            .lock()
            .unwrap()
            .iter()
            .any(|id| candidate.destinations.get(*id).is_none())
        {
            return Err(AppError::InUse("repair configured references first"));
        }
        Ok(())
    }
}
#[derive(Default)]
struct Faults {
    fail_save: AtomicBool,
    crash_before: AtomicBool,
    crash_after: AtomicBool,
    settings_writes: AtomicUsize,
    crash_journal_after: AtomicBool,
    fail_journal: AtomicBool,
    fail_cleanup_journal: AtomicBool,
}
struct Repository {
    real: FileDestinationRepository,
    faults: Arc<Faults>,
}
impl DestinationRepository for Repository {
    fn load_settings(&self) -> Result<Settings, AppError> {
        self.real.load_settings()
    }
    fn load_journal(&self) -> Result<CredentialJournal, AppError> {
        self.real.load_journal()
    }
    fn save_journal(&self, j: &CredentialJournal) -> Result<(), AppError> {
        if self.faults.fail_journal.load(Ordering::SeqCst) {
            return Err(AppError::Persistence("journal unavailable"));
        }
        self.real.save_journal(j)?;
        if self.faults.crash_journal_after.load(Ordering::SeqCst) {
            panic!("simulated crash after journal reservation");
        }
        Ok(())
    }
    fn save_settings(&self, s: &Settings) -> Result<(), AppError> {
        self.faults.settings_writes.fetch_add(1, Ordering::SeqCst);
        if self.faults.crash_before.load(Ordering::SeqCst) {
            panic!("simulated crash before commit");
        }
        if self.faults.fail_save.load(Ordering::SeqCst) {
            return Err(AppError::Persistence("injected settings failure"));
        }
        self.real.save_settings(s)?;
        if self.faults.fail_cleanup_journal.load(Ordering::SeqCst) {
            self.faults.fail_journal.store(true, Ordering::SeqCst);
        }
        if self.faults.crash_after.load(Ordering::SeqCst) {
            panic!("simulated crash after commit");
        }
        Ok(())
    }
}
struct Management {
    dir: tempfile::TempDir,
    backend: Arc<Memory>,
    store: Arc<WindowsCredentialStore<Backend>>,
    refs: Arc<RefState>,
    faults: Arc<Faults>,
    history: Arc<History>,
}
impl Management {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(&dir.path().join("history.sqlite3")).unwrap());
        let backend = Arc::new(Memory::default());
        let store = Arc::new(WindowsCredentialStore::with_backend(Backend(
            backend.clone(),
        )));
        Self {
            dir,
            backend,
            store,
            refs: Arc::new(RefState::default()),
            faults: Arc::new(Faults::default()),
            history,
        }
    }
    fn manager(&self) -> DestinationManager {
        let real = FileDestinationRepository::open(&self.dir.path().join("settings.json")).unwrap();
        DestinationManager::open(
            Box::new(Repository {
                real,
                faults: self.faults.clone(),
            }),
            self.store.clone(),
            self.history.clone(),
            Arc::new(References(self.refs.clone())),
        )
        .unwrap()
    }
    fn add(&self, m: &mut DestinationManager, name: &str) -> DestinationId {
        let mut d = m.settings().destinations.clone();
        let id = d
            .add(
                name,
                ServerOrigin::parse("https://vectr.example").unwrap(),
                None,
            )
            .unwrap();
        let rev = m.settings().revision;
        m.apply(
            DestinationEdit {
                destinations: d,
                tokens: vec![(id, token("synthetic-initial"))],
            },
            rev,
        )
        .unwrap();
        id
    }
    fn snapshot(
        &self,
        m: &DestinationManager,
        id: DestinationId,
    ) -> vectrcap::app::destinations::DestinationSnapshot {
        resolve_destination(
            DestinationSelection::Specific(id),
            DestinationSelection::Inherit,
            &m.settings().destinations,
        )
        .unwrap()
    }
    fn replace(
        &self,
        m: &mut DestinationManager,
        id: DestinationId,
    ) -> Result<vectrcap::app::destination_manager::ApplyOutcome, AppError> {
        m.apply(
            DestinationEdit {
                destinations: m.settings().destinations.clone(),
                tokens: vec![(id, token("synthetic-replaced"))],
            },
            m.settings().revision,
        )
    }
}
#[test]
fn store_failure_never_serializes_token_or_changes_settings() {
    let f = Management::new();
    let mut m = f.manager();
    let before = m.settings().clone();
    *f.backend.fail_write.lock().unwrap() = true;
    let mut d = before.destinations.clone();
    let id = d
        .add(
            "new",
            ServerOrigin::parse("https://vectr.example").unwrap(),
            None,
        )
        .unwrap();
    assert!(matches!(
        m.apply(
            DestinationEdit {
                destinations: d,
                tokens: vec![(id, token("PRIVATE_SYNTHETIC_CANARY"))]
            },
            0
        ),
        Err(AppError::Credential(_))
    ));
    assert_eq!(m.settings(), &before);
    assert_eq!(
        Settings::load(&f.dir.path().join("settings.json")).unwrap(),
        before
    );
    drop(m);
    for file in std::fs::read_dir(f.dir.path()).unwrap() {
        let bytes = std::fs::read(file.unwrap().path()).unwrap();
        assert!(
            !bytes
                .windows(b"PRIVATE_SYNTHETIC_CANARY".len())
                .any(|b| b == b"PRIVATE_SYNTHETIC_CANARY")
        );
    }
}
#[test]
fn stale_revision_is_rejected_without_credential_or_settings_writes() {
    let f = Management::new();
    let mut m = f.manager();
    let id = f.add(&mut m, "one");
    let before = f.snapshot(&m, id);
    let writes = f.faults.settings_writes.load(Ordering::SeqCst);
    let result = m.apply(
        DestinationEdit {
            destinations: m.settings().destinations.clone(),
            tokens: vec![(id, token("must-not-write"))],
        },
        0,
    );
    assert!(result.is_err());
    assert_eq!(f.faults.settings_writes.load(Ordering::SeqCst), writes);
    assert_eq!(f.backend.entries.lock().unwrap().len(), 1);
    assert!(f.store.read(&before.credential).is_ok());
}
#[test]
fn failed_settings_commit_preserves_prior_token_and_cleans_only_staged_key() {
    let f = Management::new();
    let mut m = f.manager();
    let id = f.add(&mut m, "one");
    let before = m.settings().clone();
    let key = f.snapshot(&m, id).credential;
    f.faults.fail_save.store(true, Ordering::SeqCst);
    assert!(f.replace(&mut m, id).is_err());
    assert_eq!(m.settings(), &before);
    assert_eq!(
        Settings::load(&f.dir.path().join("settings.json")).unwrap(),
        before
    );
    assert!(f.store.read(&key).is_ok());
    assert_eq!(f.backend.entries.lock().unwrap().len(), 1);
}
#[test]
fn failed_staging_cleanup_is_durable_retryable_and_generation_not_reused() {
    let f = Management::new();
    let mut m = f.manager();
    let id = f.add(&mut m, "one");
    let old = f.snapshot(&m, id).credential;
    f.faults.fail_save.store(true, Ordering::SeqCst);
    *f.backend.fail_delete.lock().unwrap() = true;
    assert!(f.replace(&mut m, id).is_err());
    assert_eq!(f.backend.entries.lock().unwrap().len(), 2);
    drop(m);
    f.faults.fail_save.store(false, Ordering::SeqCst);
    let mut m = f.manager();
    assert!(m.cleanup().unwrap());
    assert!(f.store.read(&old).is_ok());
    *f.backend.fail_delete.lock().unwrap() = false;
    assert!(!m.cleanup().unwrap());
    f.replace(&mut m, id).unwrap();
    assert_eq!(f.snapshot(&m, id).credential.generation(), 3);
    assert_eq!(f.backend.entries.lock().unwrap().len(), 1);
}
#[test]
fn postcommit_retirement_failure_keeps_new_working_binding() {
    let f = Management::new();
    let mut m = f.manager();
    let id = f.add(&mut m, "one");
    let old = f.snapshot(&m, id).credential;
    *f.backend.fail_delete.lock().unwrap() = true;
    let result = f.replace(&mut m, id).unwrap();
    assert!(result.cleanup_pending);
    let new = f.snapshot(&m, id).credential;
    assert_ne!(new, old);
    assert!(f.store.read(&new).is_ok());
    assert_eq!(
        Settings::load(&f.dir.path().join("settings.json")).unwrap(),
        *m.settings()
    );
    *f.backend.fail_delete.lock().unwrap() = false;
    assert!(!m.cleanup().unwrap());
    assert!(matches!(f.store.read(&old), Err(AppError::NeedsSetup(_))));
    assert!(f.store.read(&new).is_ok());
}
#[test]
fn origin_change_requires_explicit_new_token_and_never_reuses_old_binding() {
    let f = Management::new();
    let mut m = f.manager();
    let id = f.add(&mut m, "one");
    let old = f.snapshot(&m, id);
    let mut d = m.settings().destinations.clone();
    d.change_origin(
        id,
        ServerOrigin::parse("https://other.example").unwrap(),
        &HashSet::new(),
    )
    .unwrap();
    assert!(
        m.apply(
            DestinationEdit {
                destinations: d.clone(),
                tokens: vec![]
            },
            m.settings().revision
        )
        .is_err()
    );
    assert_eq!(f.snapshot(&m, id), old);
    m.apply(
        DestinationEdit {
            destinations: d,
            tokens: vec![(id, token("explicit-other-origin"))],
        },
        m.settings().revision,
    )
    .unwrap();
    let new = f.snapshot(&m, id);
    assert_ne!(new.credential, old.credential);
    assert_eq!(new.credential.origin(), &new.origin);
    assert!(f.store.read(&new.credential).is_ok());
    assert!(f.store.read(&old.credential).is_err());
}
#[test]
fn active_workflow_and_nonterminal_job_block_identity_edits_and_removal() {
    use vectrcap::{app::types::ArtifactKind, persistence::artifacts::ArtifactStore};
    let f = Management::new();
    let mut m = f.manager();
    let id = f.add(&mut m, "one");
    let snap = f.snapshot(&m, id);
    f.refs.active.lock().unwrap().insert(id);
    assert!(matches!(f.replace(&mut m, id), Err(AppError::InUse(_))));
    let mut removed = m.settings().destinations.clone();
    removed.remove(id, None, &HashSet::new()).unwrap();
    assert!(matches!(
        m.apply(
            DestinationEdit {
                destinations: removed.clone(),
                tokens: vec![]
            },
            m.settings().revision
        ),
        Err(AppError::InUse(_))
    ));
    f.refs.active.lock().unwrap().clear();
    let source = f.dir.path().join("synthetic.bin");
    std::fs::write(&source, b"fixture").unwrap();
    let artifact = ArtifactStore::new(f.dir.path().join("spool"))
        .unwrap()
        .stage_file(&source, ArtifactKind::File)
        .unwrap();
    f.history.create_job(&artifact, &snap).unwrap();
    assert!(matches!(f.replace(&mut m, id), Err(AppError::InUse(_))));
    let mut changed = m.settings().destinations.clone();
    changed
        .change_origin(
            id,
            ServerOrigin::parse("https://other.example").unwrap(),
            &HashSet::new(),
        )
        .unwrap();
    assert!(matches!(
        m.apply(
            DestinationEdit {
                destinations: changed,
                tokens: vec![(id, token("new"))]
            },
            m.settings().revision
        ),
        Err(AppError::InUse(_))
    ));
    assert!(matches!(
        m.apply(
            DestinationEdit {
                destinations: removed,
                tokens: vec![]
            },
            m.settings().revision
        ),
        Err(AppError::InUse(_))
    ));
}
#[test]
fn rename_domain_and_default_are_future_only_even_with_active_references() {
    use vectrcap::persistence::settings::LinkOrigin;
    let f = Management::new();
    let mut m = f.manager();
    let a = f.add(&mut m, "one");
    let b = f.add(&mut m, "two");
    let snapshot = f.snapshot(&m, a);
    f.refs.active.lock().unwrap().insert(a);
    let mut d = m.settings().destinations.clone();
    d.rename(a, "renamed").unwrap();
    d.set_link_domain(a, Some(LinkOrigin::parse("https://links.example").unwrap()))
        .unwrap();
    d.set_default(b).unwrap();
    m.apply(
        DestinationEdit {
            destinations: d,
            tokens: vec![],
        },
        m.settings().revision,
    )
    .unwrap();
    assert_eq!(snapshot.label, "one");
    assert_eq!(snapshot.link_domain, None);
    assert_eq!(snapshot.credential, f.snapshot(&m, a).credential);
    assert_eq!(m.settings().destinations.default_id(), Some(b));
}
#[test]
fn removal_requires_explicit_default_and_configured_reference_repair() {
    let f = Management::new();
    let mut m = f.manager();
    let a = f.add(&mut m, "one");
    let b = f.add(&mut m, "two");
    let old = f.snapshot(&m, a).credential;
    let kept = f.snapshot(&m, b).credential;
    let mut d = m.settings().destinations.clone();
    assert!(d.remove(a, None, &HashSet::new()).is_err());
    d.remove(a, Some(b), &HashSet::new()).unwrap();
    f.refs.configured.lock().unwrap().insert(a);
    assert!(
        m.apply(
            DestinationEdit {
                destinations: d.clone(),
                tokens: vec![]
            },
            m.settings().revision
        )
        .is_err()
    );
    assert!(f.store.read(&old).is_ok());
    f.refs.configured.lock().unwrap().clear();
    m.apply(
        DestinationEdit {
            destinations: d,
            tokens: vec![],
        },
        m.settings().revision,
    )
    .unwrap();
    assert!(f.store.read(&old).is_err());
    assert!(f.store.read(&kept).is_ok());
    assert_eq!(m.settings().destinations.default_id(), Some(b));
}
#[test]
fn restart_cleanup_before_and_after_commit_never_deletes_working_token() {
    for after in [false, true] {
        let f = Management::new();
        let mut m = f.manager();
        let id = f.add(&mut m, "one");
        let old = f.snapshot(&m, id).credential;
        f.faults.crash_before.store(!after, Ordering::SeqCst);
        f.faults.crash_after.store(after, Ordering::SeqCst);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f.replace(&mut m, id)))
                .is_err()
        );
        drop(m);
        f.faults.crash_before.store(false, Ordering::SeqCst);
        f.faults.crash_after.store(false, Ordering::SeqCst);
        let m = f.manager();
        let active = f.snapshot(&m, id).credential;
        assert_eq!(active.generation(), if after { 2 } else { 1 });
        assert!(f.store.read(&active).is_ok());
        assert_eq!(f.backend.entries.lock().unwrap().len(), 1);
        if after {
            assert!(f.store.read(&old).is_err());
        }
    }
}

#[test]
fn a_stale_workflow_id_cannot_be_reintroduced_as_a_new_destination() {
    let f = Management::new();
    let mut m = f.manager();
    let mut d = m.settings().destinations.clone();
    let id = d
        .add(
            "stale workflow",
            ServerOrigin::parse("https://vectr.example").unwrap(),
            None,
        )
        .unwrap();
    f.refs.active.lock().unwrap().insert(id);
    assert!(matches!(
        m.apply(
            DestinationEdit {
                destinations: d,
                tokens: vec![(id, token("unrelated-uploader"))]
            },
            0
        ),
        Err(AppError::InUse(_))
    ));
    assert!(f.backend.entries.lock().unwrap().is_empty());
}

#[test]
fn settings_files_with_different_extensions_have_independent_cleanup_journals() {
    let dir = tempfile::tempdir().unwrap();
    let first = FileDestinationRepository::open(&dir.path().join("profile.json")).unwrap();
    let (key, _) = keys();
    let mut journal = CredentialJournal::default();
    journal.entries.push(key.clone());
    journal.generations.insert(key.destination(), 1);
    first.save_journal(&journal).unwrap();
    drop(first);
    let second = FileDestinationRepository::open(&dir.path().join("profile.config")).unwrap();
    assert!(second.load_journal().unwrap().entries.is_empty());
}

#[test]
fn startup_rejects_broken_configured_references_before_credential_cleanup() {
    let f = Management::new();
    let mut m = f.manager();
    let id = f.add(&mut m, "one");
    let key = f.snapshot(&m, id).credential;
    drop(m);
    // Simulate settings/reference inconsistency after an external edit. The
    // adapter must leave credentials intact so the user can repair the refs.
    Settings::default()
        .save(&f.dir.path().join("settings.json"))
        .unwrap();
    f.refs.configured.lock().unwrap().insert(id);
    let repo = FileDestinationRepository::open(&f.dir.path().join("settings.json")).unwrap();
    let opened = DestinationManager::open(
        Box::new(repo),
        f.store.clone(),
        f.history.clone(),
        Arc::new(References(f.refs.clone())),
    );
    assert!(opened.is_err());
    assert!(f.store.read(&key).is_ok());
}

#[test]
fn every_native_metadata_field_is_checked_before_read_or_delete() {
    for field in 0..6 {
        let backend = Arc::new(Memory::default());
        let store = WindowsCredentialStore::with_backend(Backend(backend.clone()));
        let (key, _) = keys();
        store.write(&key, &token("synthetic")).unwrap();
        {
            let mut entries = backend.entries.lock().unwrap();
            let r = entries.values_mut().next().unwrap();
            match field {
                0 => r.target.push_str("-wrong-origin"),
                1 => r.owner = "foreign-owner".into(),
                2 => r.kind = 2,
                3 => r.persistence = 3,
                4 => r.flags = 1,
                _ => r.attributes = 1,
            }
        }
        assert!(store.read(&key).is_err());
        assert!(store.delete(&key).is_err());
        assert_eq!(backend.entries.lock().unwrap().len(), 1);
    }
}
#[test]
fn staging_capacity_and_journal_failure_happen_before_any_secret_write() {
    let f = Management::new();
    let mut m = f.manager();
    let mut d = m.settings().destinations.clone();
    let id = d
        .add(
            "one",
            ServerOrigin::parse("https://vectr.example").unwrap(),
            None,
        )
        .unwrap();
    assert!(
        m.apply(
            DestinationEdit {
                destinations: d.clone(),
                tokens: vec![(id, token(&"x".repeat(2561)))]
            },
            0
        )
        .is_err()
    );
    assert_eq!(f.backend.reads.load(Ordering::SeqCst), 0);
    assert!(f.backend.entries.lock().unwrap().is_empty());
    f.faults.fail_journal.store(true, Ordering::SeqCst);
    assert!(
        m.apply(
            DestinationEdit {
                destinations: d,
                tokens: vec![(id, token("synthetic"))]
            },
            0
        )
        .is_err()
    );
    assert!(f.backend.entries.lock().unwrap().is_empty());
    assert_eq!(m.settings().revision, 0);
}
#[test]
fn protected_store_read_failure_and_unknown_credentials_fail_closed() {
    let f = Management::new();
    let mut m = f.manager();
    let id = f.add(&mut m, "one");
    let before = m.settings().clone();
    f.backend.fail_read.store(true, Ordering::SeqCst);
    assert!(f.replace(&mut m, id).is_err());
    f.backend.fail_read.store(false, Ordering::SeqCst);
    assert_eq!(m.settings(), &before);
    assert_eq!(f.backend.entries.lock().unwrap().len(), 1);
    let mut forged = before.destinations.clone();
    let key = forged.bind_credential(id, &HashSet::new()).unwrap();
    f.store.write(&key, &token("unrelated-owned-key")).unwrap();
    assert!(f.replace(&mut m, id).is_err());
    assert!(f.store.read(&key).is_ok());
    assert_eq!(m.settings(), &before);
}
#[test]
fn cleanup_journal_failure_after_commit_cannot_rollback_the_new_binding() {
    let f = Management::new();
    let mut m = f.manager();
    let id = f.add(&mut m, "one");
    f.faults.fail_cleanup_journal.store(true, Ordering::SeqCst);
    assert!(f.replace(&mut m, id).unwrap().cleanup_pending);
    let key = f.snapshot(&m, id).credential;
    assert!(f.store.read(&key).is_ok());
    drop(m);
    f.faults.fail_journal.store(false, Ordering::SeqCst);
    f.faults.fail_cleanup_journal.store(false, Ordering::SeqCst);
    let mut m = f.manager();
    assert!(!m.cleanup().unwrap());
    assert_eq!(f.snapshot(&m, id).credential, key);
    assert!(f.store.read(&key).is_ok());
}
#[test]
fn crash_after_journal_before_secret_write_keeps_working_key_and_reservation() {
    let f = Management::new();
    let mut m = f.manager();
    let id = f.add(&mut m, "one");
    let old = f.snapshot(&m, id).credential;
    f.faults.crash_journal_after.store(true, Ordering::SeqCst);
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f.replace(&mut m, id))).is_err()
    );
    drop(m);
    f.faults.crash_journal_after.store(false, Ordering::SeqCst);
    let mut m = f.manager();
    assert!(f.store.read(&old).is_ok());
    f.replace(&mut m, id).unwrap();
    assert_eq!(f.snapshot(&m, id).credential.generation(), 3);
}
#[test]
fn startup_cleanup_protects_inflight_stale_workflow_bindings_and_scopes_deletion() {
    let f = Management::new();
    let mut m = f.manager();
    let id = f.add(&mut m, "one");
    let old = f.snapshot(&m, id).credential;
    let (unrelated, _) = keys();
    f.store
        .write(&unrelated, &token("not-owned-by-this-journal"))
        .unwrap();
    f.faults.crash_after.store(true, Ordering::SeqCst);
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f.replace(&mut m, id))).is_err()
    );
    drop(m);
    f.faults.crash_after.store(false, Ordering::SeqCst);
    f.refs.active.lock().unwrap().insert(id);
    let mut m = f.manager();
    assert!(m.cleanup().unwrap());
    assert!(f.store.read(&old).is_ok());
    let current = f.snapshot(&m, id).credential;
    assert!(f.store.read(&current).is_ok());
    f.refs.fail.store(true, Ordering::SeqCst);
    assert!(m.cleanup().is_err());
    assert!(f.store.read(&old).is_ok());
    f.refs.fail.store(false, Ordering::SeqCst);
    f.refs.active.lock().unwrap().clear();
    assert!(!m.cleanup().unwrap());
    assert!(f.store.read(&old).is_err());
    assert!(f.store.read(&current).is_ok());
    assert!(f.store.read(&unrelated).is_ok());
}
#[test]
fn file_repository_excludes_parallel_managers_and_detects_external_edits() {
    let f = Management::new();
    let mut m = f.manager();
    let id = f.add(&mut m, "one");
    let before = m.settings().clone();
    assert!(FileDestinationRepository::open(&f.dir.path().join("settings.json")).is_err());
    let mut external = before.clone();
    external.destinations.rename(id, "external change").unwrap();
    external.save(&f.dir.path().join("settings.json")).unwrap();
    assert!(f.replace(&mut m, id).is_err());
    assert!(m.cleanup().is_err());
    assert_eq!(f.backend.entries.lock().unwrap().len(), 1);
}

#[test]
fn concurrent_creates_of_one_generation_cannot_overwrite_the_first_token() {
    use std::{sync::mpsc, time::Duration};
    struct RaceBackend {
        memory: Arc<Memory>,
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
        first: AtomicBool,
    }
    impl CredentialBackend for RaceBackend {
        fn read(&self, target: &str) -> Result<Option<CredentialRecord>, AppError> {
            Backend(self.memory.clone()).read(target)
        }
        fn write(&self, r: &CredentialRecord) -> Result<(), AppError> {
            if self.first.swap(false, Ordering::SeqCst) {
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
            }
            Backend(self.memory.clone()).write(r)
        }
        fn delete(&self, target: &str) -> Result<(), AppError> {
            Backend(self.memory.clone()).delete(target)
        }
    }
    let memory = Arc::new(Memory::default());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let store = Arc::new(WindowsCredentialStore::with_backend(RaceBackend {
        memory: memory.clone(),
        entered: entered_tx,
        release: Mutex::new(release_rx),
        first: AtomicBool::new(true),
    }));
    let (key, _) = keys();
    let first_store = store.clone();
    let first_key = key.clone();
    let first = std::thread::spawn(move || first_store.write(&first_key, &token("first-token")));
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (result_tx, result_rx) = mpsc::channel();
    let second_store = store.clone();
    let second = std::thread::spawn(move || {
        result_tx
            .send(second_store.write(&key, &token("second-token")))
            .unwrap();
    });
    let early = result_rx.recv_timeout(Duration::from_millis(150));
    release_tx.send(()).unwrap();
    first.join().unwrap().unwrap();
    second.join().unwrap();
    let result = early.unwrap_or_else(|_| result_rx.recv().unwrap());
    assert!(result.is_err());
    assert_eq!(
        memory
            .entries
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .blob
            .as_slice(),
        b"first-token"
    );
}
