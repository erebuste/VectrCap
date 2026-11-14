use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Duration, Local, TimeZone};
use tempfile::TempDir;
use vectrcap::app::destinations::{DestinationSelection, Destinations, resolve_destination};
use vectrcap::app::error::AppError;
use vectrcap::app::types::{Artifact, ArtifactKind, JobId, UploadReceipt};
use vectrcap::persistence::archive::{ArchiveOrigin, ArchiveStore};
use vectrcap::persistence::artifacts::ArtifactStore;
use vectrcap::persistence::history::{History, JobStatus};
use vectrcap::persistence::settings::{ArchivePolicy, ServerOrigin, StorageMode};

struct Fixture {
    dir: TempDir,
    store: ArtifactStore,
    history: Arc<History>,
    root: PathBuf,
    n: std::cell::Cell<u32>,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let store = ArtifactStore::new(dir.path().join("spool")).unwrap();
        let history = Arc::new(History::open(&dir.path().join("history.sqlite3")).unwrap());
        let root = dir.path().join("Pictures").join("VectrCap");
        Fixture {
            dir,
            store,
            history,
            root,
            n: Default::default(),
        }
    }

    fn archive(&self) -> ArchiveStore {
        ArchiveStore::new(self.history.clone(), self.root.clone())
    }

    fn policy(&self, mode: StorageMode, cleanup_age_days: Option<u16>) -> ArchivePolicy {
        ArchivePolicy {
            storage_mode: mode,
            archive_root: Some(self.root.clone()),
            cleanup_age_days,
        }
    }

    fn screenshot(&self) -> Artifact {
        self.n.set(self.n.get() + 1);
        let name = format!("VectrCap_2026-11-14_08-03-09_0000000{}.png", self.n.get());
        let work = self.dir.path().join(&name);
        fs::write(&work, format!("png {}", self.n.get())).unwrap();
        self.store
            .stage_file(&work, ArtifactKind::Screenshot)
            .unwrap()
    }

    fn job(&self, artifact: &Artifact, status: &[JobStatus]) -> JobId {
        let mut d = Destinations::default();
        let id = d
            .add(
                "Phone",
                ServerOrigin::parse("https://vectr.example").unwrap(),
                None,
            )
            .unwrap();
        d.bind_credential(id, &HashSet::new()).unwrap();
        let dest = resolve_destination(
            DestinationSelection::Inherit,
            DestinationSelection::Inherit,
            &d,
        )
        .unwrap();
        let job = self.history.create_job(artifact, &dest).unwrap();
        for s in status {
            match s {
                JobStatus::Completed(r) => {
                    let attempt = self.history.job(job.id).unwrap().unwrap().attempt;
                    self.history.record_receipt(job.id, attempt, None, r).unwrap();
                }
                s => self.history.transition(job.id, s.clone()).unwrap(),
            }
        }
        job.id
    }
}

fn captured() -> DateTime<Local> {
    Local
        .with_ymd_and_hms(2026, 11, 14, 8, 3, 9)
        .single()
        .unwrap()
}

fn completed() -> Vec<JobStatus> {
    let receipt = UploadReceipt::new("https://i.example/f/x.png", "x.png").unwrap();
    vec![JobStatus::Running, JobStatus::Completed(receipt)]
}

fn files_under(dir: &Path) -> usize {
    if !dir.exists() {
        return 0;
    }
    fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
            if p.is_dir() { files_under(&p) } else { 1 }
        })
        .sum()
}

#[test]
fn always_save_and_no_cleanup_are_defaults() {
    let policy = ArchivePolicy::default();
    assert_eq!(policy.storage_mode, StorageMode::AlwaysSave);
    assert_eq!(policy.cleanup_age_days, None);
    assert_eq!(
        policy.archive_root, None,
        "default root resolves the Pictures known folder at use time"
    );
    policy.validate().unwrap();

    // Resolving reads the known folder; it never creates or changes it.
    let pictures = dirs::picture_dir().expect("Pictures known folder on this Windows host");
    assert_eq!(policy.resolved_root().unwrap(), pictures.join("VectrCap"));

    assert!(policy.should_auto_archive(false));
    assert!(policy.should_auto_archive(true));

    let mut bad = ArchivePolicy::default();
    for age in [0, 3651] {
        bad.cleanup_age_days = Some(age);
        assert!(matches!(bad.validate(), Err(AppError::InvalidInput(_))));
    }
    bad.cleanup_age_days = Some(3650);
    bad.validate().unwrap();
    bad.archive_root = Some(PathBuf::from("relative\\dir"));
    assert!(bad.validate().is_err(), "archive roots must be absolute");
}

#[test]
fn dated_archive_names_never_overwrite() {
    let fx = Fixture::new();
    let archive = fx.archive();
    let art = fx.screenshot();
    let month = fx.root.join("2026-11");
    fs::create_dir_all(&month).unwrap();
    let user_file = month.join(&art.display_name);
    fs::write(&user_file, b"user edited copy").unwrap();

    let first = archive.save_finalized(&art, captured()).unwrap();
    let second = archive.save_finalized(&art, captured()).unwrap();
    assert_eq!(
        first.path.parent().unwrap(),
        month.as_path(),
        "YYYY-MM from local capture time"
    );
    assert_ne!(first.path, user_file);
    assert_ne!(first.path, second.path);
    assert_eq!(fs::read(&user_file).unwrap(), b"user edited copy");
    assert_eq!(fs::read(&first.path).unwrap(), fs::read(&art.path).unwrap());

    assert_eq!(first.origin, ArchiveOrigin::Automatic);
    assert_eq!(first.artifact_id, art.id);
    assert_eq!(first.sha256, art.sha256);
    assert_eq!(first.size, art.size);
    assert_eq!(first.root, fx.root);
    let name = first.path.file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with("VectrCap_2026-11-14_08-03-09_") && name.ends_with(".png"));

    // Original files picked for upload are never archive material.
    let file = fx.store.stage_file(&user_file, ArtifactKind::File).unwrap();
    assert!(matches!(
        archive.save_finalized(&file, captured()),
        Err(AppError::InvalidInput(_))
    ));
}

#[test]
fn unwritable_archive_reports_error_and_keeps_source() {
    let fx = Fixture::new();
    fs::create_dir_all(fx.root.parent().unwrap()).unwrap();
    fs::write(&fx.root, b"a file where the archive folder should be").unwrap();
    let art = fx.screenshot();

    let err = fx.archive().save_finalized(&art, captured()).unwrap_err();
    assert!(matches!(err, AppError::Io { .. }), "{err:?}");
    fx.store.verify(&art).unwrap();
    assert_eq!(
        fs::read(&fx.root).unwrap(),
        b"a file where the archive folder should be"
    );
}

#[test]
fn pending_policy_keeps_failed_and_uncertain_jobs() {
    let fx = Fixture::new();
    let archive = fx.archive();
    let policy = fx.policy(StorageMode::PendingOrFailed, None);
    assert!(policy.should_auto_archive(true));
    assert!(
        !policy.should_auto_archive(false),
        "copy-only captures are not archived"
    );

    let failed = fx.screenshot();
    fx.job(
        &failed,
        &[
            JobStatus::Running,
            JobStatus::Failed {
                code: "http.timeout".into(),
            },
        ],
    );
    let uncertain = fx.screenshot();
    fx.job(&uncertain, &[JobStatus::Running, JobStatus::Uncertain]);
    let done = fx.screenshot();
    fx.job(&done, &completed());
    let manual_done = fx.screenshot();
    fx.job(&manual_done, &completed());

    let e_failed = archive.save_finalized(&failed, captured()).unwrap();
    let e_uncertain = archive.save_finalized(&uncertain, captured()).unwrap();
    let e_done = archive.save_finalized(&done, captured()).unwrap();
    let e_manual = archive.save_manual(&manual_done, captured()).unwrap();

    let report = archive.reconcile_uploaded(&policy).unwrap();
    assert_eq!(report.removed, 1);
    assert!(e_failed.path.exists());
    assert!(e_uncertain.path.exists());
    assert!(
        !e_done.path.exists(),
        "confirmed upload releases the automatic copy"
    );
    assert!(e_manual.path.exists(), "explicit Save is always retained");

    // Other modes never release archives on upload success.
    let again = fx.screenshot();
    fx.job(&again, &completed());
    let e_again = archive.save_finalized(&again, captured()).unwrap();
    assert_eq!(
        archive
            .reconcile_uploaded(&fx.policy(StorageMode::AlwaysSave, None))
            .unwrap()
            .removed,
        0
    );
    assert!(e_again.path.exists());
}

#[test]
fn never_automatic_preserves_retry_spool_without_archiving() {
    let fx = Fixture::new();
    let policy = fx.policy(StorageMode::NeverAutomatic, Some(1));
    assert!(!policy.should_auto_archive(true));
    assert!(!policy.should_auto_archive(false));

    let art = fx.screenshot();
    let job = fx.job(
        &art,
        &[
            JobStatus::Running,
            JobStatus::Failed {
                code: "net.offline".into(),
            },
        ],
    );
    let archive = fx.archive();
    archive.reconcile_uploaded(&policy).unwrap();
    archive
        .cleanup(&policy, Local::now() + Duration::days(400))
        .unwrap();

    assert_eq!(files_under(&fx.root), 0, "no automatic archive copy");
    fx.store.verify(&art).unwrap();
    assert!(
        fx.history
            .artifacts_needing_spool()
            .unwrap()
            .contains(&art.id)
    );
    assert!(
        fx.history
            .recover_jobs()
            .unwrap()
            .iter()
            .any(|j| j.id == job)
    );
}

#[test]
fn manual_save_ignores_automatic_policy() {
    let fx = Fixture::new();
    let archive = fx.archive();
    let art = fx.screenshot();
    let entry = archive.save_manual(&art, captured()).unwrap();
    assert_eq!(entry.origin, ArchiveOrigin::Manual);
    assert!(entry.path.exists());

    for mode in [
        StorageMode::AlwaysSave,
        StorageMode::PendingOrFailed,
        StorageMode::NeverAutomatic,
    ] {
        let policy = fx.policy(mode, Some(1));
        archive.reconcile_uploaded(&policy).unwrap();
        let report = archive
            .cleanup(&policy, Local::now() + Duration::days(4000))
            .unwrap();
        assert_eq!(report.removed, 0);
        assert!(entry.path.exists());
    }
}

#[test]
fn cleanup_skips_active_manual_and_modified_files() {
    let fx = Fixture::new();
    let archive = fx.archive();

    let idle = fx.screenshot();
    fx.job(&idle, &completed());
    let e_idle = archive.save_finalized(&idle, captured()).unwrap();
    let local_only = fx.screenshot();
    let e_local = archive.save_finalized(&local_only, captured()).unwrap();
    let active = fx.screenshot();
    fx.job(&active, &[]);
    let e_active = archive.save_finalized(&active, captured()).unwrap();
    let manual = fx.screenshot();
    let e_manual = archive.save_manual(&manual, captured()).unwrap();
    let modified = fx.screenshot();
    let e_modified = archive.save_finalized(&modified, captured()).unwrap();
    fs::write(&e_modified.path, b"user annotated this later").unwrap();
    let moved = fx.screenshot();
    let e_moved = archive.save_finalized(&moved, captured()).unwrap();
    fs::rename(&e_moved.path, fx.dir.path().join("moved.png")).unwrap();

    let disabled = fx.policy(StorageMode::AlwaysSave, None);
    assert_eq!(
        archive
            .cleanup(&disabled, Local::now() + Duration::days(4000))
            .unwrap()
            .removed,
        0
    );

    let policy = fx.policy(StorageMode::AlwaysSave, Some(30));
    let early = archive
        .cleanup(&policy, Local::now() + Duration::days(29))
        .unwrap();
    assert_eq!(early.removed, 0, "nothing is older than the threshold yet");

    let report = archive
        .cleanup(&policy, Local::now() + Duration::days(31))
        .unwrap();
    assert_eq!(report.removed, 2);
    assert_eq!(report.kept_active, 1);
    assert_eq!(report.kept_modified, 1);
    assert_eq!(report.kept_missing, 1);
    assert!(!e_idle.path.exists() && !e_local.path.exists());
    assert!(e_active.path.exists() && e_manual.path.exists() && e_modified.path.exists());
    assert!(
        fx.dir.path().join("moved.png").exists(),
        "moved user files are never chased"
    );
    assert_eq!(
        fs::read(&e_modified.path).unwrap(),
        b"user annotated this later"
    );

    // Already-removed entries are not processed twice.
    assert_eq!(
        archive
            .cleanup(&policy, Local::now() + Duration::days(31))
            .unwrap()
            .removed,
        0
    );
}

#[test]
fn changing_archive_root_never_moves_or_deletes_existing_files() {
    let fx = Fixture::new();
    let old = fx
        .archive()
        .save_finalized(&fx.screenshot(), captured())
        .unwrap();

    let new_root = fx.dir.path().join("Elsewhere");
    let moved_store = ArchiveStore::new(fx.history.clone(), new_root.clone());
    let fresh = moved_store
        .save_finalized(&fx.screenshot(), captured())
        .unwrap();
    assert!(fresh.path.starts_with(&new_root));
    assert!(old.path.exists(), "existing archives stay where they were");
    assert_eq!(
        files_under(&new_root),
        1,
        "nothing was copied or moved to the new root"
    );

    let policy = ArchivePolicy {
        storage_mode: StorageMode::AlwaysSave,
        archive_root: Some(new_root.clone()),
        cleanup_age_days: Some(1),
    };
    let report = moved_store
        .cleanup(&policy, Local::now() + Duration::days(10))
        .unwrap();
    assert!(
        old.path.exists(),
        "cleanup never deletes outside the current root"
    );
    assert!(!fresh.path.exists());
    assert_eq!(report.removed, 1);
    assert_eq!(report.kept_other_root, 1);
}
