mod support;
use std::{fs, sync::Arc};
use support::*;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use vectrcap::{
    app::{
        destinations::{
            DestinationSelection, DestinationSnapshot, Destinations, resolve_destination,
        },
        error::AppError,
        types::ArtifactKind,
    },
    persistence::{
        artifacts::ArtifactStore,
        history::{History, JobStatus, UploadJob},
    },
    vectr::{
        client::VectrClient,
        uploader::{ClientFactory, Uploader},
    },
};
fn destination(f: &Fixture) -> DestinationSnapshot {
    let mut d = Destinations::default();
    let id = d.add("Original", f.origin.clone(), None).unwrap();
    d.bind_credential(id, &std::collections::HashSet::new())
        .unwrap();
    resolve_destination(
        DestinationSelection::Inherit,
        DestinationSelection::Inherit,
        &d,
    )
    .unwrap()
}
fn setup(
    f: &Fixture,
    bytes: &[u8],
) -> (
    tempfile::TempDir,
    Arc<History>,
    Arc<ArtifactStore>,
    UploadJob,
    Uploader,
) {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("original.zip");
    fs::write(&src, bytes).unwrap();
    let store = Arc::new(ArtifactStore::new(t.path().join("spool")).unwrap());
    let a = store.stage_file(&src, ArtifactKind::File).unwrap();
    let h = Arc::new(History::open(&t.path().join("history.db")).unwrap());
    let d = destination(f);
    let j = h.create_job(&a, &d).unwrap();
    let root = f.root.clone();
    let origin = f.origin.clone();
    let key = d.credential.clone();
    let factory: Arc<dyn ClientFactory> = Arc::new(move |d: &DestinationSnapshot| {
        assert_eq!(d.credential, key);
        assert_eq!(d.origin, origin);
        VectrClient::with_root_certificate(
            d.origin.clone(),
            vectrcap::vectr::client::UploadToken::new("synthetic-token".into()).unwrap(),
            root.clone(),
        )
    });
    let u = Uploader::new(h.clone(), store.clone(), factory);
    (t, h, store, j, u)
}
async fn run(u: &Uploader, j: UploadJob) -> Result<vectrcap::app::types::UploadReceipt, AppError> {
    let (tx, _rx) = mpsc::channel(32);
    u.run(j, CancellationToken::new(), tx).await
}
fn known(h: &History, j: &UploadJob) {
    h.transition(j.id, JobStatus::Running).unwrap();
    h.set_session(j.id, ID.parse().unwrap()).unwrap();
    h.transition(j.id, JobStatus::Interrupted).unwrap();
}
#[tokio::test]
async fn crash_seeded_exhausted_budgets_remain_explicit_retry_eligible() {
    use vectrcap::vectr::queue::{QueueCommand as C, QueueController, QueueEffect as E};
    for operation in ["piece.0", "complete", "status"] {
        let f = Fixture::new(vec![]).await;
        let (t, h, s, j, _u) = setup(&f, b"abc");
        h.transition(j.id, JobStatus::Running).unwrap();
        h.set_session(j.id, ID.parse().unwrap()).unwrap();
        h.set_upload_retry(j.id, operation, 5, 0).unwrap();
        let reopened = Arc::new(History::open(&t.path().join("history.db")).unwrap());
        let mut q = QueueController::new(reopened.clone()).unwrap();
        let job = q
            .handle(C::Admit(j.id))
            .into_iter()
            .find_map(|e| match e {
                E::Start(j) => Some(*j),
                _ => None,
            })
            .unwrap();
        let root = f.root.clone();
        let u = Uploader::new(
            reopened.clone(),
            s,
            Arc::new(move |d: &DestinationSnapshot| {
                VectrClient::with_root_certificate(
                    d.origin.clone(),
                    vectrcap::vectr::client::UploadToken::new("synthetic".into()).unwrap(),
                    root.clone(),
                )
            }),
        );
        let error = run(&u, job.clone()).await.unwrap_err();
        assert_eq!(
            reopened.job(j.id).unwrap().unwrap().status,
            JobStatus::Failed {
                code: "retry_exhausted".into()
            },
            "{operation}: {error:?}"
        );
        assert!(
            f.log().is_empty(),
            "no recovery request before explicit Retry"
        );
        q.handle(C::Settled {
            job: job.id,
            attempt: job.attempt,
            result: Err(error),
        });
        q.handle(C::PauseAll);
        let deadline = i64::MAX / 2;
        reopened
            .set_upload_retry(j.id, operation, 5, deadline)
            .unwrap();
        q.handle(C::RetryEligible);
        assert_eq!(
            reopened.job(j.id).unwrap().unwrap().status,
            JobStatus::Pending
        );
        assert_eq!(
            reopened.upload_retry(j.id, operation).unwrap(),
            Some((0, deadline))
        );
    }
}
#[tokio::test(start_paused = true)]
async fn recovered_exhaustion_offers_explicit_retry_before_cooldown_expires() {
    use vectrcap::vectr::queue::{QueueCommand as C, QueueController, QueueEffect as E};
    for operation in ["piece.0", "complete", "status"] {
        let f = Fixture::new(vec![]).await;
        let (t, h, s, j, _u) = setup(&f, b"abc");
        h.transition(j.id, JobStatus::Running).unwrap();
        h.set_session(j.id, ID.parse().unwrap()).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let deadline = now + 120_000;
        h.set_upload_retry(j.id, operation, 5, deadline).unwrap();
        let reopened = Arc::new(History::open(&t.path().join("history.db")).unwrap());
        let mut q = QueueController::new(reopened.clone()).unwrap();
        let job = q
            .handle(C::Admit(j.id))
            .into_iter()
            .find_map(|e| match e {
                E::Start(j) => Some(*j),
                _ => None,
            })
            .unwrap();
        let root = f.root.clone();
        let u = Uploader::new(
            reopened.clone(),
            s,
            Arc::new(move |d: &DestinationSnapshot| {
                VectrClient::with_root_certificate(
                    d.origin.clone(),
                    vectrcap::vectr::client::UploadToken::new("synthetic".into()).unwrap(),
                    root.clone(),
                )
            }),
        );
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(1), run(&u, job.clone())).await;
        assert!(
            result.is_ok(),
            "{operation}: exhaustion must offer Retry without waiting out the persisted cooldown"
        );
        let error = result.unwrap().unwrap_err();
        assert_eq!(
            reopened.job(j.id).unwrap().unwrap().status,
            JobStatus::Failed {
                code: "retry_exhausted".into()
            }
        );
        assert!(f.log().is_empty());
        q.handle(C::Settled {
            job: job.id,
            attempt: job.attempt,
            result: Err(error),
        });
        q.handle(C::PauseAll);
        q.handle(C::RetryEligible);
        assert_eq!(
            reopened.job(j.id).unwrap().unwrap().status,
            JobStatus::Pending
        );
        assert_eq!(
            reopened.upload_retry(j.id, operation).unwrap(),
            Some((0, deadline))
        );
    }
}
#[tokio::test]
async fn payload_is_sampled_before_delayed_response_without_response_wait_time() {
    let (reply, arrived, release) = Reply::json(204, "").blocked();
    let f = Fixture::new(vec![
        Reply::json(200, r#"{"chunkBytes":3,"received":[]}"#),
        reply,
        Reply::json(200, RECEIPT),
    ])
    .await;
    let (_t, h, _s, j, u) = setup(&f, b"abc");
    known(&h, &j);
    let (tx, mut rx) = mpsc::channel(32);
    let task = tokio::spawn(async move { u.run(j, CancellationToken::new(), tx).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), arrived.notified())
        .await
        .unwrap();
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(10)).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    let mut samples = vec![];
    while let Ok(event) = rx.try_recv() {
        if let vectrcap::vectr::uploader::UploadEvent::Payload { bytes, elapsed, .. } = event {
            samples.push((bytes, elapsed));
        }
    }
    assert_eq!(
        samples.iter().map(|(bytes, _)| *bytes).sum::<u64>(),
        3,
        "outgoing body must be observed while response is blocked"
    );
    assert!(
        samples
            .iter()
            .all(|(_, elapsed)| *elapsed < std::time::Duration::from_secs(1)),
        "response waiting is not body transfer time"
    );
    tokio::time::resume();
    release.add_permits(1);
    task.await.unwrap().unwrap();
}
#[tokio::test]
async fn resumes_only_missing_pieces() {
    let f = Fixture::new(vec![
        Reply::json(200, r#"{"chunkBytes":3,"received":[0,2]}"#),
        Reply::json(204, ""),
        Reply::json(200, RECEIPT),
    ])
    .await;
    let (_t, h, _s, j, u) = setup(&f, b"abcdefg");
    known(&h, &j);
    run(&u, j.clone()).await.unwrap();
    let l = f.log();
    assert_eq!(l.len(), 3);
    assert_eq!(l[0].method, "GET");
    assert_eq!(l[1].path, format!("/api/uploads/{ID}/1"));
    assert_eq!(l[1].body, b"def");
    assert!(matches!(
        h.job(j.id).unwrap().unwrap().status,
        JobStatus::Completed(_)
    ));
}
#[tokio::test]
async fn recovers_completed_session() {
    let f = Fixture::new(vec![Reply::json(
        200,
        format!(r#"{{"chunkBytes":3,"received":[],"done":{RECEIPT}}}"#),
    )])
    .await;
    let (_t, h, _s, j, u) = setup(&f, b"abc");
    known(&h, &j);
    let r = run(&u, j.clone()).await.unwrap();
    assert_eq!(r.name(), "remote.webp");
    assert_eq!(f.log().len(), 1);
    assert!(matches!(
        h.job(j.id).unwrap().unwrap().status,
        JobStatus::Completed(_)
    ));
}
#[tokio::test]
async fn does_not_retry_uncertain_creation() {
    for bytes in [&b"abc"[..], &b""[..]] {
        let f = Fixture::new(vec![Reply::lost()]).await;
        let (_t, h, _s, j, u) = setup(&f, bytes);
        assert!(matches!(
            run(&u, j.clone()).await,
            Err(AppError::Uncertain(_))
        ));
        assert_eq!(h.job(j.id).unwrap().unwrap().status, JobStatus::Uncertain);
        assert!(matches!(run(&u, j).await, Err(AppError::Uncertain(_))));
        assert_eq!(f.log().len(), 1);
    }
}
#[tokio::test]
async fn resume_uses_staged_bytes() {
    let f = Fixture::new(vec![
        Reply::json(200, r#"{"chunkBytes":3,"received":[]}"#),
        Reply::json(204, ""),
        Reply::json(200, RECEIPT),
    ])
    .await;
    let (t, h, _s, j, u) = setup(&f, b"abc");
    known(&h, &j);
    fs::write(t.path().join("original.zip"), b"changed").unwrap();
    run(&u, j).await.unwrap();
    assert_eq!(f.log()[1].body, b"abc");
    assert_eq!(fs::read(t.path().join("original.zip")).unwrap(), b"changed");
}
#[tokio::test]
async fn resume_uses_original_destination_not_default() {
    let f = Fixture::new(vec![Reply::json(
        200,
        format!(r#"{{"chunkBytes":3,"received":[],"done":{RECEIPT}}}"#),
    )])
    .await;
    let (_t, h, _s, j, u) = setup(&f, b"a");
    known(&h, &j);
    let other = Fixture::new(vec![Reply::json(401, "")]).await;
    let mut defaults = Destinations::default();
    defaults
        .add("New default", other.origin.clone(), None)
        .unwrap();
    run(&u, j).await.unwrap();
    assert_eq!(f.log().len(), 1);
    assert!(other.log().is_empty());
}
#[tokio::test]
async fn same_origin_uploader_tokens_never_cross() {
    let f = Fixture::new(vec![Reply::json(200, RECEIPT), Reply::json(200, RECEIPT)]).await;
    let (t, h, s, j, _u) = setup(&f, b"");
    let mut d = Destinations::default();
    let id = d.add("Other uploader", f.origin.clone(), None).unwrap();
    d.bind_credential(id, &std::collections::HashSet::new())
        .unwrap();
    let snap = resolve_destination(
        DestinationSelection::Inherit,
        DestinationSelection::Inherit,
        &d,
    )
    .unwrap();
    let j2 = h.create_job(&j.artifact, &snap).unwrap();
    let first = j.destination.credential.clone();
    let second = snap.credential.clone();
    let root = f.root.clone();
    let factory = Arc::new(move |d: &DestinationSnapshot| {
        let token = if d.credential == first {
            "first-synthetic"
        } else {
            assert_eq!(d.credential, second);
            "second-synthetic"
        };
        VectrClient::with_root_certificate(
            d.origin.clone(),
            vectrcap::vectr::client::UploadToken::new(token.into()).unwrap(),
            root.clone(),
        )
    });
    let u = Uploader::new(h, s, factory);
    run(&u, j).await.unwrap();
    run(&u, j2).await.unwrap();
    let l = f.log();
    assert!(l[0].headers.contains("authorization: first-synthetic"));
    assert!(l[1].headers.contains("authorization: second-synthetic"));
    drop(t);
}
#[tokio::test]
async fn ambiguous_completion_queries_status_before_retry() {
    let f = Fixture::new(vec![
        Reply::json(200, r#"{"chunkBytes":3,"received":[0]}"#),
        Reply::lost(),
        Reply::json(
            200,
            format!(r#"{{"chunkBytes":3,"received":[],"done":{RECEIPT}}}"#),
        ),
    ])
    .await;
    let (_t, h, _s, j, u) = setup(&f, b"abc");
    known(&h, &j);
    run(&u, j).await.unwrap();
    let l = f.log();
    assert_eq!(l.len(), 3);
    assert_eq!(l[1].method, "POST");
    assert_eq!(l[2].method, "GET");
}
#[tokio::test]
async fn expired_session_requires_explicit_restart() {
    let f = Fixture::new(vec![Reply::json(404, "")]).await;
    let (_t, h, _s, j, u) = setup(&f, b"abc");
    known(&h, &j);
    assert!(matches!(
        run(&u, j.clone()).await,
        Err(AppError::HttpStatus { code: 404, .. })
    ));
    assert!(
        matches!(h.job(j.id).unwrap().unwrap().status,JobStatus::Failed{code} if code=="expired")
    );
    assert_eq!(f.log().len(), 1);
}
#[tokio::test(start_paused = true)]
async fn honors_retry_after() {
    use std::time::{Duration, SystemTime};
    use vectrcap::vectr::{client::retry_after, retry};
    assert_eq!(
        retry_after("120", SystemTime::UNIX_EPOCH),
        Some(Duration::from_secs(120))
    );
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    let task = tokio::spawn(async move { retry::wait(Duration::from_secs(120), &c).await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(119)).await;
    assert!(!task.is_finished());
    cancel.cancel();
    assert_eq!(task.await.unwrap(), Err(AppError::Cancelled));
}
#[tokio::test]
async fn retry_budget_and_deadline_survive_database_restart() {
    let f = Fixture::new(vec![]).await;
    let (t, h, _s, j, _u) = setup(&f, b"abc");
    h.set_upload_retry(j.id, "piece.0", 4, 123456).unwrap();
    let reopened = History::open(&t.path().join("history.db")).unwrap();
    assert_eq!(
        reopened.upload_retry(j.id, "piece.0").unwrap(),
        Some((4, 123456))
    );
    reopened.clear_upload_retry(j.id).unwrap();
    assert_eq!(h.upload_retry(j.id, "piece.0").unwrap(), None);
}
#[tokio::test]
async fn recovery_status_retry_cannot_erase_pending_piece_budget() {
    let f = Fixture::new(vec![]).await;
    let (_t, h, _s, j, _u) = setup(&f, b"abc");
    h.set_upload_retry(j.id, "piece.0", 4, 123456).unwrap();
    h.set_upload_retry(j.id, "status", 1, 789012).unwrap();
    assert_eq!(h.upload_retry(j.id, "piece.0").unwrap(), Some((4, 123456)));
    assert_eq!(h.upload_retry(j.id, "status").unwrap(), Some((1, 789012)));
}
#[tokio::test]
async fn verified_guard_prevents_mutation_throughout_request() {
    let (reply, arrived, release) = Reply::json(204, "").blocked();
    let f = Fixture::new(vec![
        Reply::json(200, r#"{"chunkBytes":3,"received":[]}"#),
        reply,
        Reply::json(200, RECEIPT),
    ])
    .await;
    let (t, h, _s, j, u) = setup(&f, b"abc");
    known(&h, &j);
    let path = j.artifact.path.clone();
    let work = tokio::spawn(async move { run(&u, j).await });
    arrived.notified().await;
    assert!(fs::write(&path, b"bad").is_err());
    assert!(fs::rename(&path, t.path().join("replacement")).is_err());
    release.add_permits(1);
    work.await.unwrap().unwrap();
}
#[tokio::test]
async fn rate_limit_retries_are_bounded_and_count_payload_not_progress() {
    let mut replies = vec![Reply::json(200, r#"{"chunkBytes":3,"received":[]}"#)];
    for _ in 0..5 {
        replies.push(Reply::json(429, "").header("Retry-After", "0"));
    }
    let f = Fixture::new(replies).await;
    let (_t, h, _s, j, u) = setup(&f, b"abc");
    known(&h, &j);
    let (tx, mut rx) = mpsc::channel(32);
    assert!(matches!(
        u.run(j.clone(), CancellationToken::new(), tx).await,
        Err(AppError::HttpStatus { code: 429, .. })
    ));
    assert_eq!(f.log().len(), 6);
    assert_eq!(h.upload_retry(j.id, "piece.0").unwrap().unwrap().0, 5);
    let mut payload = 0;
    let mut accepted = 0;
    while let Some(e) = rx.recv().await {
        match e {
            vectrcap::vectr::uploader::UploadEvent::Payload { bytes, .. } => payload += bytes,
            vectrcap::vectr::uploader::UploadEvent::Acknowledged { bytes, .. } => accepted += bytes,
            _ => {}
        }
    }
    assert_eq!(payload, 15);
    assert_eq!(accepted, 0);
}
#[tokio::test]
async fn confirmed_receipt_survives_retry_cleanup_failure() {
    let f = Fixture::new(vec![
        Reply::json(200, r#"{"chunkBytes":3,"received":[0]}"#),
        Reply::json(200, RECEIPT),
    ])
    .await;
    let (t, h, _s, j, u) = setup(&f, b"abc");
    known(&h, &j);
    h.set_upload_retry(j.id, "complete", 1, 0).unwrap();
    let c = rusqlite::Connection::open(t.path().join("history.db")).unwrap();
    c.execute_batch("CREATE TRIGGER reject_retry_cleanup BEFORE DELETE ON upload_retry BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
    run(&u, j.clone()).await.unwrap();
    assert!(matches!(
        h.job(j.id).unwrap().unwrap().status,
        JobStatus::Completed(_)
    ));
}
#[tokio::test]
async fn many_recovered_pieces_cannot_overflow_progress_channel() {
    let status = serde_json::json!({"chunkBytes":1,"received":(0..65).collect::<Vec<u32>>()});
    let f = Fixture::new(vec![
        Reply::json(200, status.to_string()),
        Reply::json(204, ""),
        Reply::json(200, RECEIPT),
    ])
    .await;
    let (_t, h, _s, j, u) = setup(&f, &[42; 66]);
    known(&h, &j);
    let (tx, mut rx) = mpsc::channel(32);
    u.run(j, CancellationToken::new(), tx).await.unwrap();
    let mut accepted = 0;
    while let Some(e) = rx.recv().await {
        match e {
            vectrcap::vectr::uploader::UploadEvent::Acknowledged { bytes, .. } => accepted += bytes,
            vectrcap::vectr::uploader::UploadEvent::Recovered { acknowledged, .. } => {
                accepted = acknowledged
            }
            _ => {}
        }
    }
    assert_eq!(accepted, 66);
}
#[tokio::test]
async fn approved_schema_v1_upgrades_additively_without_retargeting_jobs() {
    let f = Fixture::new(vec![]).await;
    let (t, h, _s, j, _u) = setup(&f, b"abc");
    known(&h, &j);
    h.set_job_paused(j.id, true).unwrap();
    let before = h.job(j.id).unwrap().unwrap();
    let c = rusqlite::Connection::open(t.path().join("history.db")).unwrap();
    remove_schema_v3(&c);
    c.execute_batch("DROP TABLE upload_retry; PRAGMA user_version=1;")
        .unwrap();
    drop(c);
    let reopened = History::open(&t.path().join("history.db")).unwrap();
    assert_eq!(reopened.job(j.id).unwrap().unwrap(), before);
    reopened.set_upload_retry(j.id, "piece.0", 1, 123).unwrap();
    assert_eq!(
        reopened.upload_retry(j.id, "piece.0").unwrap(),
        Some((1, 123))
    );
}
#[tokio::test]
async fn approved_schema_v2_upgrades_queue_order_and_retries_without_archive_changes() {
    let f = Fixture::new(vec![]).await;
    let (t, h, _s, j, _u) = setup(&f, b"abc");
    known(&h, &j);
    h.set_job_paused(j.id, true).unwrap();
    h.set_queue_paused(true).unwrap();
    h.set_upload_retry(j.id, "piece.0", 5, 123456).unwrap();
    let second = h.create_job(&j.artifact, &j.destination).unwrap();
    let before = h.job(j.id).unwrap().unwrap();
    let c = rusqlite::Connection::open(t.path().join("history.db")).unwrap();
    let archive_schema = |c: &rusqlite::Connection| {
        c.prepare("SELECT sql FROM sqlite_master WHERE name LIKE 'archive%' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let archive_before = archive_schema(&c);
    remove_schema_v3(&c);
    c.execute_batch("PRAGMA user_version=2; UPDATE jobs SET created_ms=1;")
        .unwrap();
    let reopened = History::open(&t.path().join("history.db")).unwrap();
    assert_eq!(reopened.job(j.id).unwrap().unwrap(), before);
    assert!(reopened.queue_paused().unwrap());
    assert_eq!(
        reopened.upload_retry(j.id, "piece.0").unwrap(),
        Some((5, 123456))
    );
    assert_eq!(archive_schema(&c), archive_before);
    assert!(!reopened.upload_cancel_requested(j.id).unwrap());
    reopened.set_job_paused(j.id, false).unwrap();
    assert_eq!(reopened.next_upload().unwrap().unwrap().id, j.id);
    reopened.transition(j.id, JobStatus::Cancelled).unwrap();
    let third = reopened.create_job(&j.artifact, &j.destination).unwrap();
    assert_eq!(reopened.next_upload().unwrap().unwrap().id, second.id);
    reopened
        .transition(second.id, JobStatus::Cancelled)
        .unwrap();
    assert_eq!(reopened.next_upload().unwrap().unwrap().id, third.id);
}
#[tokio::test]
async fn ambiguous_completion_status_query_uses_bounded_rate_limit_recovery() {
    let f = Fixture::new(vec![
        Reply::json(200, r#"{"chunkBytes":3,"received":[0]}"#),
        Reply::lost(),
        Reply::json(429, "").header("Retry-After", "0"),
        Reply::json(
            200,
            format!(r#"{{"chunkBytes":3,"received":[],"done":{RECEIPT}}}"#),
        ),
    ])
    .await;
    let (_t, h, _s, j, u) = setup(&f, b"abc");
    known(&h, &j);
    run(&u, j.clone()).await.unwrap();
    assert_eq!(f.log().len(), 4);
    assert!(matches!(
        h.job(j.id).unwrap().unwrap().status,
        JobStatus::Completed(_)
    ));
}
#[tokio::test(start_paused = true)]
async fn extreme_retry_after_wait_is_cancellable_without_timer_overflow() {
    let token = CancellationToken::new();
    let cancel = token.clone();
    let task = tokio::spawn(async move {
        vectrcap::vectr::retry::wait(std::time::Duration::MAX, &cancel).await
    });
    tokio::task::yield_now().await;
    token.cancel();
    assert_eq!(task.await.unwrap(), Err(AppError::Cancelled));
}
