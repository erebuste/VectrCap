use std::collections::HashSet;
use std::fs;
use std::path::Path;

use tempfile::TempDir;
use uuid::Uuid;
use vectrcap::app::destinations::{
    DestinationSelection, DestinationSnapshot, Destinations, resolve_destination,
};
use vectrcap::app::error::AppError;
use vectrcap::app::types::{ArtifactKind, UploadReceipt};
use vectrcap::persistence::artifacts::ArtifactStore;
use vectrcap::persistence::history::{History, JobStatus};
use vectrcap::persistence::settings::{LinkOrigin, RecordingLimits, ServerOrigin, Settings};

fn snapshot() -> DestinationSnapshot {
    let mut d = Destinations::default();
    let id = d
        .add(
            "Phone",
            ServerOrigin::parse("https://vectr.example").unwrap(),
            None,
        )
        .unwrap();
    d.bind_credential(id, &HashSet::new()).unwrap();
    resolve_destination(
        DestinationSelection::Inherit,
        DestinationSelection::Inherit,
        &d,
    )
    .unwrap()
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = dir.join(name);
    fs::write(&p, bytes).unwrap();
    p
}

#[test]
fn rejects_unsafe_origins() {
    let bad = [
        "",
        "http://vectr.example",
        "ftp://vectr.example",
        "vectr.example",
        "https://",
        "https://user:pw@vectr.example",
        "https://user@vectr.example",
        "https://@vectr.example",
        "https://vectr.example?x=1",
        "https://vectr.example/?",
        "https://vectr.example#frag",
        "https://vectr.example/#",
        "https://vectr.example/path",
        "https://vectr.example//",
        "https://vectr.example\\",
        "https://vectr.example/\n",
        "https://vec\ttr.example",
        " https://vectr.example",
        "https://vectr.example ",
        "https://vectr .example",
        "https://vectr.example:0",
        "https://vectr.example:99999",
    ];
    for s in bad {
        assert!(
            matches!(ServerOrigin::parse(s), Err(AppError::InvalidInput(_))),
            "{s:?} must be rejected"
        );
    }
    let long = format!("https://{}.example", "a".repeat(3000));
    assert!(ServerOrigin::parse(&long).is_err());

    assert_eq!(
        ServerOrigin::parse("https://vectr.example")
            .unwrap()
            .as_str(),
        "https://vectr.example"
    );
    assert_eq!(
        ServerOrigin::parse("HTTPS://Vectr.EXAMPLE/")
            .unwrap()
            .as_str(),
        "https://vectr.example"
    );
    assert_eq!(
        ServerOrigin::parse("https://vectr.example:443/")
            .unwrap()
            .as_str(),
        "https://vectr.example"
    );
    assert_eq!(
        ServerOrigin::parse("https://vectr.example:8443")
            .unwrap()
            .as_str(),
        "https://vectr.example:8443"
    );
    assert_eq!(
        ServerOrigin::parse("https://vectr.example").unwrap(),
        ServerOrigin::parse("https://VECTR.example/").unwrap()
    );
}

#[test]
fn dot_segment_origins_are_rejected_before_normalization() {
    // WHATWG parsing reduces each of these paths to "/", so only a raw check catches them.
    let dotted = [
        "https://vectr.example/.",
        "https://vectr.example/..",
        "https://vectr.example/./",
        "https://vectr.example/a/..",
        "https://vectr.example:8443/a/../",
        "https://vectr.example/%2e",
    ];
    for s in dotted {
        assert!(
            matches!(ServerOrigin::parse(s), Err(AppError::InvalidInput(_))),
            "ServerOrigin {s:?}"
        );
        assert!(
            matches!(LinkOrigin::parse(s), Err(AppError::InvalidInput(_))),
            "LinkOrigin {s:?}"
        );
        let json = serde_json::to_string(s).unwrap();
        assert!(serde_json::from_str::<ServerOrigin>(&json).is_err(), "serde {s:?}");
        assert!(serde_json::from_str::<LinkOrigin>(&json).is_err(), "serde {s:?}");
    }

    // The same strings are refused inside a persisted settings document.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("settings.json");
    let mut s = Settings::default();
    s.destinations
        .add(
            "Phone",
            ServerOrigin::parse("https://vectr.example").unwrap(),
            Some(LinkOrigin::parse("https://share.example").unwrap()),
        )
        .unwrap();
    s.save(&path).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    for field in ["origin", "link_domain_override"] {
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        v["destinations"]["items"][0][field] = "https://vectr.example/a/..".into();
        fs::write(&path, v.to_string()).unwrap();
        assert!(Settings::load(&path).is_err(), "settings {field}");
    }
}

#[test]
fn validates_recording_duration_bounds() {
    let defaults = RecordingLimits::default();
    assert_eq!(
        (defaults.gif_max_seconds, defaults.mp4_max_minutes),
        (60, 10)
    );
    let check = |gif, mp4| {
        let s = Settings {
            recording: RecordingLimits {
                gif_max_seconds: gif,
                mp4_max_minutes: mp4,
            },
            ..Settings::default()
        };
        s.validate()
    };
    check(1, 10).unwrap();
    check(300, 10).unwrap();
    assert!(matches!(check(301, 10), Err(AppError::InvalidInput(_))));
    assert!(check(0, 10).is_err());
    check(60, 1).unwrap();
    check(60, 60).unwrap();
    assert!(matches!(check(60, 61), Err(AppError::InvalidInput(_))));
    assert!(check(60, 0).is_err());
}

#[test]
fn staged_file_is_immutable() {
    let dir = TempDir::new().unwrap();
    let spool = dir.path().join("spool");
    let store = ArtifactStore::new(spool.clone()).unwrap();
    let src = write(dir.path(), "report.zip", b"v1");

    let artifact = store.stage_file(&src, ArtifactKind::File).unwrap();
    assert_eq!(artifact.size, 2);
    assert_eq!(artifact.display_name, "report.zip");
    assert_eq!(artifact.kind, ArtifactKind::File);
    assert_eq!(artifact.path.parent().unwrap(), spool.as_path());
    let generated = artifact.path.file_name().unwrap().to_str().unwrap();
    assert!(
        !generated.contains("report"),
        "staged names never reuse untrusted components"
    );

    fs::write(&src, b"v2 - edited after staging").unwrap();
    assert_eq!(fs::read(&artifact.path).unwrap(), b"v1");
    store.verify(&artifact).unwrap();
    assert!(
        fs::metadata(&artifact.path)
            .unwrap()
            .permissions()
            .readonly()
    );
    assert!(
        fs::OpenOptions::new()
            .write(true)
            .open(&artifact.path)
            .is_err()
    );

    // If the staged bytes are ever altered, verification refuses them.
    let mut perms = fs::metadata(&artifact.path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(&artifact.path, perms).unwrap();
    fs::write(&artifact.path, b"v9").unwrap();
    assert!(matches!(
        store.verify(&artifact),
        Err(AppError::ArtifactChanged)
    ));

    // Directories and missing sources are refused; originals are untouched.
    assert!(matches!(
        store.stage_file(dir.path(), ArtifactKind::File),
        Err(AppError::InvalidInput(_))
    ));
    assert!(
        store
            .stage_file(&dir.path().join("missing.bin"), ArtifactKind::File)
            .is_err()
    );
    assert_eq!(fs::read(&src).unwrap(), b"v2 - edited after staging");
}

#[test]
fn reopens_pending_jobs() {
    let dir = TempDir::new().unwrap();
    let store = ArtifactStore::new(dir.path().join("spool")).unwrap();
    let db = dir.path().join("history.sqlite3");
    let dest = snapshot();
    let session = Uuid::new_v4();

    let (resumable, uncertain, paused, done) = {
        let history = History::open(&db).unwrap();
        let job = |name: &str| {
            let art = store
                .stage_file(
                    &write(dir.path(), name, name.as_bytes()),
                    ArtifactKind::File,
                )
                .unwrap();
            history.create_job(&art, &dest).unwrap()
        };
        let (resumable, uncertain, paused, done) =
            (job("a.bin"), job("b.bin"), job("c.bin"), job("d.bin"));

        history
            .transition(resumable.id, JobStatus::Running)
            .unwrap();
        history.set_session(resumable.id, session).unwrap();
        history
            .transition(uncertain.id, JobStatus::Running)
            .unwrap();
        history.set_job_paused(paused.id, true).unwrap();
        history.transition(done.id, JobStatus::Running).unwrap();
        let receipt = UploadReceipt::new("https://i.example/f/d.bin", "d.bin").unwrap();
        history
            .transition(done.id, JobStatus::Completed(receipt))
            .unwrap();
        (resumable, uncertain, paused, done)
    }; // process "exits" here

    let history = History::open(&db).unwrap();
    let jobs = history.recover_jobs().unwrap();
    let find = |id| jobs.iter().find(|j| j.id == id);

    let r = find(resumable.id).unwrap();
    assert_eq!(
        r.status,
        JobStatus::Interrupted,
        "known session resumes by status query"
    );
    assert_eq!(r.session, Some(session));
    assert_eq!(r.destination, dest);
    assert_eq!(r.artifact, resumable.artifact);

    let u = find(uncertain.id).unwrap();
    assert_eq!(
        u.status,
        JobStatus::Uncertain,
        "creation may have reached the server"
    );
    assert_eq!(u.session, None);

    let p = find(paused.id).unwrap();
    assert_eq!(p.status, JobStatus::Pending);
    assert!(p.paused);
    assert!(find(done.id).is_none(), "completed jobs are not recovered");

    // Recovery is idempotent and leaves the completed receipt intact.
    assert_eq!(history.recover_jobs().unwrap().len(), 3);
    assert!(matches!(
        history.job(done.id).unwrap().unwrap().status,
        JobStatus::Completed(_)
    ));
}

#[test]
fn job_transitions_are_validated_and_sanitized() {
    let dir = TempDir::new().unwrap();
    let store = ArtifactStore::new(dir.path().join("spool")).unwrap();
    let history = History::open(&dir.path().join("history.sqlite3")).unwrap();
    let art = store
        .stage_file(&write(dir.path(), "a.bin", b"a"), ArtifactKind::File)
        .unwrap();
    let job = history.create_job(&art, &snapshot()).unwrap();
    assert_eq!(job.status, JobStatus::Pending);
    assert!(!job.paused);

    // Failure codes are short typed identifiers, never raw text that could hold secrets/URLs.
    let too_long = "x".repeat(65);
    for raw in [
        "Bearer abc123",
        "https://x.example/a",
        "",
        too_long.as_str(),
    ] {
        let failed = JobStatus::Failed {
            code: raw.to_string(),
        };
        assert!(history.transition(job.id, failed).is_err(), "{raw:?}");
    }
    history.transition(job.id, JobStatus::Running).unwrap();
    history
        .transition(
            job.id,
            JobStatus::Failed {
                code: "http.server_error".into(),
            },
        )
        .unwrap();
    history.transition(job.id, JobStatus::Pending).unwrap();
    history.transition(job.id, JobStatus::Running).unwrap();
    let receipt = UploadReceipt::new("https://i.example/f/a.bin", "a.bin").unwrap();
    history
        .transition(job.id, JobStatus::Completed(receipt))
        .unwrap();

    // A confirmed receipt is terminal and never downgraded.
    for next in [
        JobStatus::Pending,
        JobStatus::Running,
        JobStatus::Cancelled,
        JobStatus::Uncertain,
    ] {
        assert!(matches!(
            history.transition(job.id, next),
            Err(AppError::InvalidInput(_))
        ));
    }
    assert!(UploadReceipt::new("http://i.example/f/a.bin", "a.bin").is_err());
    assert!(history.set_session(job.id, Uuid::new_v4()).is_err());
}

#[test]
fn pause_and_favorite_intents_default_false_and_persist() {
    let dir = TempDir::new().unwrap();
    let store = ArtifactStore::new(dir.path().join("spool")).unwrap();
    let db = dir.path().join("history.sqlite3");
    let job_id = {
        let history = History::open(&db).unwrap();
        assert!(!history.queue_paused().unwrap());
        let art = store
            .stage_file(&write(dir.path(), "a.bin", b"a"), ArtifactKind::File)
            .unwrap();
        let job = history.create_job(&art, &snapshot()).unwrap();
        assert!(!job.paused);
        history.set_queue_paused(true).unwrap();
        history.set_job_paused(job.id, true).unwrap();
        job.id
    };
    let history = History::open(&db).unwrap();
    assert!(history.queue_paused().unwrap());
    assert!(history.job(job_id).unwrap().unwrap().paused);

    // The versioned schema stores one history item per artifact with favorite off.
    let conn = rusqlite::Connection::open(&db).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);
    let favorite: i64 = conn
        .query_row("SELECT favorite FROM history_items", [], |r| r.get(0))
        .unwrap();
    assert_eq!(favorite, 0);
}

#[test]
fn newer_schema_is_refused() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("history.sqlite3");
    History::open(&db).unwrap();
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("PRAGMA user_version = 99")
        .unwrap();
    assert!(matches!(History::open(&db), Err(AppError::Persistence(_))));
}

#[test]
fn startup_recovery_removes_only_orphaned_partials() {
    let dir = TempDir::new().unwrap();
    let spool = dir.path().join("spool");
    let store = ArtifactStore::new(spool.clone()).unwrap();
    let history = History::open(&dir.path().join("history.sqlite3")).unwrap();

    let queued = store
        .stage_file(&write(dir.path(), "q.bin", b"queued"), ArtifactKind::File)
        .unwrap();
    history.create_job(&queued, &snapshot()).unwrap();
    let orphan = store
        .stage_file(&write(dir.path(), "o.bin", b"orphan"), ArtifactKind::File)
        .unwrap();
    let partial = write(&spool, ".vectrcap-partial-abc123", b"half");
    let foreign = write(&spool, "notes.txt", b"not ours");

    let needed = history.artifacts_needing_spool().unwrap();
    assert!(needed.contains(&queued.id) && !needed.contains(&orphan.id));
    let removed = store.recover(&needed).unwrap();

    assert_eq!(removed, 2);
    assert!(queued.path.exists(), "queued job source stays protected");
    assert!(!orphan.path.exists());
    assert!(!partial.exists());
    assert!(
        foreign.exists(),
        "files the app does not own are never removed"
    );
}

#[test]
fn settings_roundtrip_is_validated_and_versioned() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("settings.json");
    let mut s = Settings::default();
    s.destinations
        .add(
            "Phone",
            ServerOrigin::parse("https://vectr.example").unwrap(),
            None,
        )
        .unwrap();
    s.save(&path).unwrap();
    assert_eq!(Settings::load(&path).unwrap(), s);

    let text = fs::read_to_string(&path).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v["version"] = 2.into();
    fs::write(&path, v.to_string()).unwrap();
    assert!(matches!(
        Settings::load(&path),
        Err(AppError::InvalidInput(_))
    ));

    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v["token"] = "secret".into();
    fs::write(&path, v.to_string()).unwrap();
    assert!(Settings::load(&path).is_err(), "unknown fields are refused");

    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v["recording"]["gif_max_seconds"] = 301.into();
    fs::write(&path, v.to_string()).unwrap();
    assert!(
        Settings::load(&path).is_err(),
        "loaded values are validated"
    );

    fs::write(&path, vec![b' '; 2 * 1024 * 1024]).unwrap();
    assert!(
        Settings::load(&path).is_err(),
        "oversized documents are refused"
    );

    // An invalid save never replaces the working file.
    s.save(&path).unwrap();
    let mut bad = s.clone();
    bad.recording.gif_max_seconds = 0;
    assert!(bad.save(&path).is_err());
    assert_eq!(Settings::load(&path).unwrap(), s);
}
