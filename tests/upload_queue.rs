mod support;
use std::{collections::HashSet, sync::Arc, time::Duration};
use support::*;
use vectrcap::{
    app::{
        destinations::{DestinationSelection, Destinations, resolve_destination},
        error::AppError,
        types::{ArtifactKind, UploadReceipt},
    },
    persistence::{
        artifacts::ArtifactStore,
        history::{History, JobStatus, UploadJob},
        settings::ServerOrigin,
    },
    vectr::queue::{
        QueueCommand as C, QueueController, QueueEffect as E, TransferStats, command_channel,
    },
};
fn setup() -> (
    tempfile::TempDir,
    Arc<History>,
    Arc<ArtifactStore>,
    UploadJob,
) {
    setup_origin(ServerOrigin::parse("https://a.example").unwrap())
}
fn setup_origin(
    origin: ServerOrigin,
) -> (
    tempfile::TempDir,
    Arc<History>,
    Arc<ArtifactStore>,
    UploadJob,
) {
    let t = tempfile::tempdir().unwrap();
    let h = Arc::new(History::open(&t.path().join("h.db")).unwrap());
    let store = Arc::new(ArtifactStore::new(t.path().join("spool")).unwrap());
    let p = t.path().join("original.zip");
    std::fs::write(&p, b"abc").unwrap();
    let a = store.stage_file(&p, ArtifactKind::File).unwrap();
    let mut d = Destinations::default();
    let id = d.add("A", origin, None).unwrap();
    d.bind_credential(id, &HashSet::new()).unwrap();
    let snap = resolve_destination(
        DestinationSelection::Inherit,
        DestinationSelection::Inherit,
        &d,
    )
    .unwrap();
    let j = h.create_job(&a, &snap).unwrap();
    (t, h, store, j)
}
fn start(q: &mut QueueController, id: vectrcap::app::types::JobId) -> UploadJob {
    q.handle(C::Admit(id))
        .into_iter()
        .find_map(|e| if let E::Start(j) = e { Some(j) } else { None })
        .expect("one start")
        .as_ref()
        .clone()
}
#[test]
fn pause_keeps_source_and_session() {
    let (_t, h, s, j) = setup();
    let mut q = QueueController::new(h.clone()).unwrap();
    let j = start(&mut q, j.id);
    let session = uuid::Uuid::new_v4();
    h.set_session(j.id, session).unwrap();
    assert!(
        q.handle(C::PauseJob(j.id))
            .iter()
            .any(|e| matches!(e,E::Pausing(id) if *id==j.id))
    );
    q.handle(C::Settled {
        job: j.id,
        attempt: j.attempt,
        result: Err(AppError::Paused),
    });
    let saved = h.job(j.id).unwrap().unwrap();
    assert!(saved.paused);
    assert_eq!(saved.session, Some(session));
    s.verify(&saved.artifact).unwrap();
}
#[test]
fn pause_all_does_not_block_capture() {
    let (t, h, s, j) = setup();
    let mut q = QueueController::new(h.clone()).unwrap();
    q.handle(C::PauseAll);
    assert!(h.queue_paused().unwrap());
    assert!(
        !q.handle(C::Admit(j.id))
            .iter()
            .any(|e| matches!(e, E::Start(_)))
    );
    let p = t.path().join("another.png");
    std::fs::write(&p, b"synthetic").unwrap();
    let a = s.stage_file(&p, ArtifactKind::Screenshot).unwrap();
    h.create_job(&a, &j.destination).unwrap();
    assert_eq!(std::fs::read(p).unwrap(), b"synthetic");
}
#[test]
fn resume_all_respects_individual_pause() {
    let (_t, h, _s, j) = setup();
    let mut q = QueueController::new(h.clone()).unwrap();
    q.handle(C::PauseAll);
    q.handle(C::Admit(j.id));
    q.handle(C::PauseJob(j.id));
    assert!(h.job(j.id).unwrap().unwrap().paused);
    assert!(
        !q.handle(C::ResumeAll)
            .iter()
            .any(|e| matches!(e, E::Start(_)))
    );
    assert!(
        q.handle(C::ResumeJob(j.id))
            .iter()
            .any(|e| matches!(e, E::Start(_)))
    );
}
#[test]
fn paused_intents_survive_restart() {
    let (t, h, _s, j) = setup();
    let mut q = QueueController::new(h.clone()).unwrap();
    q.handle(C::PauseJob(j.id));
    q.handle(C::PauseAll);
    drop(q);
    let reopened = Arc::new(History::open(&t.path().join("h.db")).unwrap());
    let mut q = QueueController::new(reopened.clone()).unwrap();
    assert!(reopened.queue_paused().unwrap());
    assert!(reopened.job(j.id).unwrap().unwrap().paused);
    assert!(
        !q.handle(C::ResumeAll)
            .iter()
            .any(|e| matches!(e, E::Start(_)))
    );
}
#[tokio::test]
async fn acknowledged_active_cancel_survives_reopen_without_settlement() {
    let f = Fixture::new(vec![]).await;
    let (t, h, s, j) = setup_origin(f.origin.clone());
    let mut q = QueueController::new(h.clone()).unwrap();
    let running = start(&mut q, j.id);
    h.set_session(j.id, ID.parse().unwrap()).unwrap();
    assert!(
        q.handle(C::Cancel(j.id))
            .iter()
            .any(|e| matches!(e, E::CancelActive(_)))
    );
    // Process loss before the bounded in-flight request settles.
    drop(q);
    let reopened = Arc::new(History::open(&t.path().join("h.db")).unwrap());
    let mut recovered = QueueController::new(reopened.clone()).unwrap();
    assert!(
        !recovered
            .handle(C::ResumeAll)
            .iter()
            .any(|e| matches!(e, E::Start(_)))
    );
    assert_eq!(
        reopened.job(j.id).unwrap().unwrap().status,
        JobStatus::Cancelled
    );
    let u = net_uploader(&f, reopened, s);
    let (tx, _rx) = tokio::sync::mpsc::channel(32);
    assert_eq!(
        u.run(running, tokio_util::sync::CancellationToken::new(), tx)
            .await,
        Err(AppError::Cancelled)
    );
    assert!(
        f.log().is_empty(),
        "cancelled work must send no create/status/piece/complete"
    );
}
#[test]
fn completion_racing_pause_keeps_receipt() {
    let (_t, h, _s, j) = setup();
    let mut q = QueueController::new(h.clone()).unwrap();
    let j = start(&mut q, j.id);
    let sid = uuid::Uuid::new_v4();
    h.set_session(j.id, sid).unwrap();
    q.handle(C::PauseJob(j.id));
    let receipt = UploadReceipt::new("https://links.example/a", "a").unwrap();
    q.handle(C::Settled {
        job: j.id,
        attempt: j.attempt,
        result: Ok(receipt.clone()),
    });
    assert_eq!(
        h.job(j.id).unwrap().unwrap().status,
        JobStatus::Completed(receipt)
    );
}
#[test]
fn resume_queries_existing_session() {
    let (_t, h, _s, j) = setup();
    h.transition(j.id, JobStatus::Running).unwrap();
    let sid = uuid::Uuid::new_v4();
    h.set_session(j.id, sid).unwrap();
    h.transition(j.id, JobStatus::Interrupted).unwrap();
    h.set_job_paused(j.id, true).unwrap();
    let mut q = QueueController::new(h.clone()).unwrap();
    let e = q.handle(C::ResumeJob(j.id));
    let resumed = e
        .into_iter()
        .find_map(|e| if let E::Start(j) = e { Some(j) } else { None })
        .unwrap();
    assert_eq!(resumed.session, Some(sid));
    assert_eq!(resumed.attempt, 2);
}
#[test]
fn rapid_commands_never_duplicate_worker() {
    let (_t, h, _s, j) = setup();
    let mut q = QueueController::new(h).unwrap();
    let first = start(&mut q, j.id);
    for _ in 0..20 {
        q.handle(C::PauseJob(j.id));
        assert!(
            !q.handle(C::ResumeJob(j.id))
                .iter()
                .any(|e| matches!(e, E::Start(_)))
        );
        assert!(
            !q.handle(C::Admit(j.id))
                .iter()
                .any(|e| matches!(e, E::Start(_)))
        );
    }
    q.handle(C::Cancel(j.id));
    let effects = q.handle(C::Settled {
        job: j.id,
        attempt: first.attempt - 1,
        result: Err(AppError::Paused),
    });
    assert!(!effects.iter().any(|e| matches!(e, E::Start(_))));
}
#[test]
fn retry_failed_respects_gate_and_uncertainty() {
    let (_t, h, _s, j) = setup();
    h.transition(j.id, JobStatus::Running).unwrap();
    h.transition(
        j.id,
        JobStatus::Failed {
            code: "transient".into(),
        },
    )
    .unwrap();
    let u = h.create_job(&j.artifact, &j.destination).unwrap();
    h.transition(u.id, JobStatus::Running).unwrap();
    h.transition(u.id, JobStatus::Uncertain).unwrap();
    h.set_upload_retry(j.id, "piece.0", 4, 123456).unwrap();
    let mut q = QueueController::new(h.clone()).unwrap();
    q.handle(C::PauseAll);
    assert!(
        !q.handle(C::RetryEligible)
            .iter()
            .any(|e| matches!(e, E::Start(_)))
    );
    assert_eq!(h.job(j.id).unwrap().unwrap().status, JobStatus::Pending);
    assert_eq!(h.job(u.id).unwrap().unwrap().status, JobStatus::Uncertain);
    assert_eq!(h.upload_retry(j.id, "piece.0").unwrap(), Some((0, 123456)));
}
#[test]
fn speed_excludes_paused_time_and_progress_deduplicates_pieces() {
    let mut s = TransferStats::default();
    s.acknowledge(0, 3).unwrap();
    s.acknowledge(0, 3).unwrap();
    s.acknowledge(1, 2).unwrap();
    assert_eq!(s.acknowledged(), 5);
    s.payload(Duration::from_secs(1), 100, Duration::from_secs(1))
        .unwrap();
    assert_eq!(s.speed(Duration::from_secs(1)), Some(100.0));
    s.pause();
    assert_eq!(s.speed(Duration::from_secs(100)), None);
    s.payload(Duration::from_secs(102), 200, Duration::from_secs(2))
        .unwrap();
    assert_eq!(s.speed(Duration::from_secs(102)), Some(100.0));
    assert_eq!(s.speed(Duration::from_secs(108)), None);
}
#[test]
fn speed_accounts_for_stalled_body_intervals_and_rejects_old_bulk_samples() {
    let mut stats = TransferStats::default();
    stats
        .payload(Duration::from_secs(1), 100, Duration::from_secs(1))
        .unwrap();
    stats
        .payload(Duration::from_secs(2), 0, Duration::from_secs(1))
        .unwrap();
    assert_eq!(stats.speed(Duration::from_secs(2)), Some(50.0));
    stats
        .payload(Duration::from_secs(3), 200, Duration::from_secs(1))
        .unwrap();
    assert_eq!(stats.speed(Duration::from_secs(3)), Some(100.0));
    assert_eq!(stats.speed(Duration::from_secs(9)), None);
    stats
        .payload(Duration::from_secs(20), 1000, Duration::from_secs(10))
        .unwrap();
    assert_eq!(
        stats.speed(Duration::from_secs(20)),
        None,
        "a missed long interval cannot be relabelled recent"
    );
}
#[test]
fn commands_are_bounded_at_32() {
    let (tx, _rx) = command_channel();
    for _ in 0..32 {
        tx.try_send(C::PauseAll).unwrap();
    }
    assert!(tx.try_send(C::PauseAll).is_err());
}
#[test]
fn durable_admission_of_100_jobs_never_strands_accepted_work() {
    let (_t, h, _s, j) = setup();
    let mut q = QueueController::new(h.clone()).unwrap();
    q.handle(C::PauseAll);
    let mut ids = HashSet::from([j.id]);
    for _ in 1..100 {
        let next = h.create_job(&j.artifact, &j.destination).unwrap();
        ids.insert(next.id);
        assert!(
            !q.handle(C::Admit(next.id))
                .iter()
                .any(|e| matches!(e, E::Error(_)))
        );
    }
    let mut effects = q.handle(C::ResumeAll);
    let mut seen = HashSet::new();
    while let Some(job) = effects.into_iter().find_map(|e| match e {
        E::Start(j) => Some(j),
        _ => None,
    }) {
        assert!(seen.insert(job.id));
        effects = q.handle(C::Settled {
            job: job.id,
            attempt: job.attempt,
            result: Err(AppError::Cancelled),
        });
    }
    assert_eq!(seen, ids);
    assert!(h.recover_jobs().unwrap().is_empty());
}

#[test]
fn durable_recovery_controls_33_and_100_jobs_with_retained_failures() {
    for count in [33, 100] {
        let (t, h, _s, j) = setup();
        let mut ids = vec![j.id];
        for _ in 1..count {
            ids.push(h.create_job(&j.artifact, &j.destination).unwrap().id);
        }
        for (index, id) in ids.iter().take(32).enumerate() {
            h.transition(*id, JobStatus::Running).unwrap();
            h.transition(
                *id,
                if index % 2 == 0 {
                    JobStatus::Failed {
                        code: "transient".into(),
                    }
                } else {
                    JobStatus::Uncertain
                },
            )
            .unwrap();
        }
        h.set_queue_paused(true).unwrap();
        let reopened = Arc::new(History::open(&t.path().join("h.db")).unwrap());
        let mut q = QueueController::new(reopened.clone())
            .expect("durable depth is independent of channel size");
        q.handle(C::RetryEligible);
        for id in ids.iter().take(32).skip(1).step_by(2) {
            assert_eq!(
                reopened.job(*id).unwrap().unwrap().status,
                JobStatus::Uncertain
            );
            assert!(
                !q.handle(C::Cancel(*id))
                    .iter()
                    .any(|e| matches!(e, E::Error(_)))
            );
        }
        let last = *ids.last().unwrap();
        q.handle(C::PauseJob(last));
        let mut effects = q.handle(C::ResumeAll);
        let mut seen = HashSet::new();
        while let Some(job) = effects.into_iter().find_map(|e| match e {
            E::Start(j) => Some(j),
            _ => None,
        }) {
            assert_ne!(job.id, last);
            assert!(seen.insert(job.id));
            effects = q.handle(C::Settled {
                job: job.id,
                attempt: job.attempt,
                result: Err(AppError::Cancelled),
            });
        }
        assert_eq!(seen.len(), count - 17);
        assert!(
            q.handle(C::ResumeJob(last))
                .iter()
                .any(|e| matches!(e, E::Start(j) if j.id == last))
        );
    }
}

#[test]
fn durable_acceptance_failure_is_transactional_before_acknowledgment() {
    let (t, h, _s, j) = setup();
    let c = rusqlite::Connection::open(t.path().join("h.db")).unwrap();
    c.execute_batch("CREATE TRIGGER reject_job BEFORE INSERT ON jobs BEGIN SELECT RAISE(ABORT,'synthetic capacity failure'); END;").unwrap();
    assert!(h.create_job(&j.artifact, &j.destination).is_err());
    assert_eq!(h.recover_jobs().unwrap().len(), 1);
}
#[test]
fn durable_fifo_ignores_equal_timestamps_uuid_order_and_clock_rollback() {
    for backward in [false, true] {
        let (t, h, _s, first) = setup();
        let second = h.create_job(&first.artifact, &first.destination).unwrap();
        let third = h.create_job(&first.artifact, &first.destination).unwrap();
        let fourth = h.create_job(&first.artifact, &first.destination).unwrap();
        let c = rusqlite::Connection::open(t.path().join("h.db")).unwrap();
        let a = "ffffffff-ffff-4fff-afff-ffffffffffff";
        let b = "00000001-0000-4000-a000-000000000001";
        c.execute(
            "UPDATE jobs SET id=?2,created_ms=1000 WHERE id=?1",
            rusqlite::params![first.id.0.to_string(), a],
        )
        .unwrap();
        c.execute(
            "UPDATE jobs SET id=?2,created_ms=?3 WHERE id=?1",
            rusqlite::params![second.id.0.to_string(), b, if backward { 1 } else { 1000 }],
        )
        .unwrap();
        h.set_job_paused(third.id, true).unwrap();
        h.transition(fourth.id, JobStatus::Running).unwrap();
        h.transition(fourth.id, JobStatus::Uncertain).unwrap();
        let reopened = Arc::new(History::open(&t.path().join("h.db")).unwrap());
        let mut q = QueueController::new(reopened.clone()).unwrap();
        let take_start = |effects: Vec<E>| {
            effects
                .into_iter()
                .find_map(|e| match e {
                    E::Start(j) => Some(j),
                    _ => None,
                })
                .unwrap()
        };
        let running = take_start(q.handle(C::ResumeAll));
        assert_eq!(running.id.0.to_string(), a, "backward={backward}");
        let running = take_start(q.handle(C::Settled {
            job: running.id,
            attempt: running.attempt,
            result: Err(AppError::Cancelled),
        }));
        assert_eq!(running.id.0.to_string(), b);
        assert!(
            !q.handle(C::Settled {
                job: running.id,
                attempt: running.attempt,
                result: Err(AppError::Cancelled)
            })
            .iter()
            .any(|e| matches!(e, E::Start(_)))
        );
        assert_eq!(take_start(q.handle(C::ResumeJob(third.id))).id, third.id);
        assert_eq!(
            reopened.job(fourth.id).unwrap().unwrap().status,
            JobStatus::Uncertain
        );
    }
}
#[test]
fn restart_is_explicit_and_preserves_original_source() {
    let (_t, h, s, j) = setup();
    h.transition(j.id, JobStatus::Running).unwrap();
    h.transition(j.id, JobStatus::Uncertain).unwrap();
    let mut q = QueueController::new(h.clone()).unwrap();
    let effects = q.handle(C::Restart(j.id));
    let replacement = effects
        .iter()
        .find_map(|e| {
            if let E::Restarted { replacement, .. } = e {
                Some(*replacement)
            } else {
                None
            }
        })
        .unwrap();
    let new = h.job(replacement).unwrap().unwrap();
    assert_eq!(new.artifact, j.artifact);
    assert_eq!(new.destination, j.destination);
    assert_eq!(new.session, None);
    s.verify(&new.artifact).unwrap();
    assert_eq!(h.job(j.id).unwrap().unwrap().status, JobStatus::Cancelled);
}
async fn until(rx: &mut tokio::sync::mpsc::Receiver<E>, predicate: impl Fn(&E) -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let e = rx.recv().await.expect("queue effect");
            if let E::Error(error) = &e {
                panic!("queue: {error:?}");
            }
            if predicate(&e) {
                break;
            }
        }
    })
    .await
    .expect("bounded fixture synchronization");
}
fn net_uploader(
    f: &Fixture,
    h: Arc<History>,
    s: Arc<ArtifactStore>,
) -> Arc<vectrcap::vectr::uploader::Uploader> {
    let root = f.root.clone();
    let origin = f.origin.clone();
    Arc::new(vectrcap::vectr::uploader::Uploader::new(
        h,
        s,
        Arc::new(
            move |d: &vectrcap::app::destinations::DestinationSnapshot| {
                assert_eq!(d.origin, origin);
                assert_eq!(d.credential.destination(), d.id);
                vectrcap::vectr::client::VectrClient::with_root_certificate(
                    d.origin.clone(),
                    vectrcap::vectr::client::UploadToken::new("synthetic".into()).unwrap(),
                    root.clone(),
                )
            },
        ),
    ))
}
#[tokio::test]
async fn actual_pause_settles_piece_then_resume_queries_status() {
    let (reply, arrived, release) = Reply::json(204, "").blocked();
    let f = Fixture::new(vec![
        Reply::json(200, r#"{"chunkBytes":1,"received":[]}"#),
        reply,
        Reply::json(200, r#"{"chunkBytes":1,"received":[0]}"#),
        Reply::json(204, ""),
        Reply::json(204, ""),
        Reply::json(200, RECEIPT),
    ])
    .await;
    let (_t, h, s, j) = setup_origin(f.origin.clone());
    h.transition(j.id, JobStatus::Running).unwrap();
    h.set_session(j.id, ID.parse().unwrap()).unwrap();
    h.transition(j.id, JobStatus::Interrupted).unwrap();
    let q = QueueController::new(h.clone()).unwrap();
    let u = net_uploader(&f, h.clone(), s.clone());
    let (tx, rx) = command_channel();
    let (nt, mut nr) = tokio::sync::mpsc::channel(32);
    let task = tokio::spawn(q.serve(u, rx, nt));
    tokio::time::timeout(Duration::from_secs(5), arrived.notified())
        .await
        .unwrap();
    tx.send(C::PauseAll).await.unwrap();
    until(&mut nr, |e| matches!(e, E::Pausing(_))).await;
    release.add_permits(1);
    until(&mut nr, |e| matches!(e, E::Paused(_))).await;
    assert_eq!(f.log().len(), 2);
    assert!(h.queue_paused().unwrap());
    assert_eq!(
        h.job(j.id).unwrap().unwrap().session,
        Some(ID.parse().unwrap())
    );
    s.verify(&j.artifact).unwrap();
    tx.send(C::ResumeAll).await.unwrap();
    until(&mut nr, |e| matches!(e, E::Changed(_))).await;
    assert!(matches!(
        h.job(j.id).unwrap().unwrap().status,
        JobStatus::Completed(_)
    ));
    assert_eq!(f.log()[2].method, "GET");
    assert_eq!(f.log().len(), 6);
    drop(tx);
    task.await.unwrap().unwrap();
}
#[tokio::test]
async fn actual_completion_racing_cancel_preserves_receipt() {
    let (reply, arrived, release) = Reply::json(200, RECEIPT).blocked();
    let f = Fixture::new(vec![
        Reply::json(200, r#"{"chunkBytes":3,"received":[0]}"#),
        reply,
    ])
    .await;
    let (_t, h, s, j) = setup_origin(f.origin.clone());
    h.transition(j.id, JobStatus::Running).unwrap();
    h.set_session(j.id, ID.parse().unwrap()).unwrap();
    h.transition(j.id, JobStatus::Interrupted).unwrap();
    let q = QueueController::new(h.clone()).unwrap();
    let u = net_uploader(&f, h.clone(), s);
    let (tx, rx) = command_channel();
    let (nt, mut nr) = tokio::sync::mpsc::channel(32);
    let task = tokio::spawn(q.serve(u, rx, nt));
    tokio::time::timeout(Duration::from_secs(5), arrived.notified())
        .await
        .unwrap();
    tx.send(C::Cancel(j.id)).await.unwrap();
    until(&mut nr, |e| matches!(e, E::CancelActive(_))).await;
    release.add_permits(1);
    until(&mut nr, |e| matches!(e, E::Changed(_))).await;
    assert!(matches!(
        h.job(j.id).unwrap().unwrap().status,
        JobStatus::Completed(_)
    ));
    assert_eq!(f.log().len(), 2);
    drop(tx);
    task.await.unwrap().unwrap();
}
#[tokio::test]
async fn known_session_cancel_reopen_before_piece_settlement_never_resumes() {
    let (reply, arrived, _release) = Reply::json(204, "").blocked();
    let f = Fixture::new(vec![
        Reply::json(200, r#"{"chunkBytes":1,"received":[]}"#),
        reply,
    ])
    .await;
    let (t, h, s, j) = setup_origin(f.origin.clone());
    h.transition(j.id, JobStatus::Running).unwrap();
    h.set_session(j.id, ID.parse().unwrap()).unwrap();
    let q = QueueController::new(h.clone()).unwrap();
    let u = net_uploader(&f, h.clone(), s.clone());
    let (tx, rx) = command_channel();
    let (nt, mut nr) = tokio::sync::mpsc::channel(32);
    let task = tokio::spawn(q.serve(u, rx, nt));
    tokio::time::timeout(Duration::from_secs(5), arrived.notified())
        .await
        .unwrap();
    tx.send(C::Cancel(j.id)).await.unwrap();
    until(&mut nr, |e| matches!(e, E::CancelActive(_))).await;
    assert!(
        h.upload_cancel_requested(j.id).unwrap(),
        "ack follows durable intent"
    );
    assert_eq!(
        h.job(j.id).unwrap().unwrap().status,
        JobStatus::Running,
        "in-flight result can still win"
    );
    task.abort(); // simulate process loss, without settling the piece
    assert!(task.await.unwrap_err().is_cancelled());
    let reopened = Arc::new(History::open(&t.path().join("h.db")).unwrap());
    let mut q = QueueController::new(reopened.clone()).unwrap();
    assert_eq!(
        reopened.job(j.id).unwrap().unwrap().status,
        JobStatus::Cancelled
    );
    assert!(
        !q.handle(C::ResumeAll)
            .iter()
            .any(|e| matches!(e, E::Start(_)))
    );
    let (events, _rx) = tokio::sync::mpsc::channel(32);
    let saved = reopened.job(j.id).unwrap().unwrap();
    assert_eq!(
        net_uploader(&f, reopened, s.clone())
            .run(saved, tokio_util::sync::CancellationToken::new(), events)
            .await,
        Err(AppError::Cancelled)
    );
    assert_eq!(
        f.log().len(),
        2,
        "no additional create/status/piece/complete"
    );
    s.verify(&j.artifact).unwrap();
}

#[test]
fn cancel_persistence_failure_cannot_acknowledge_active_cancellation() {
    let (t, h, _s, j) = setup();
    let mut q = QueueController::new(h.clone()).unwrap();
    start(&mut q, j.id);
    let c = rusqlite::Connection::open(t.path().join("h.db")).unwrap();
    c.execute_batch("CREATE TRIGGER reject_cancel_intent BEFORE UPDATE OF cancel_requested ON jobs BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
    let effects = q.handle(C::Cancel(j.id));
    assert!(effects.iter().any(|e| matches!(e, E::Error(_))));
    assert!(!effects.iter().any(|e| matches!(e, E::CancelActive(_))));
    assert!(!h.upload_cancel_requested(j.id).unwrap());
}

#[tokio::test]
async fn full_ui_channel_cannot_block_cancel_or_confirmed_completion() {
    let (reply, arrived, release) = Reply::json(200, RECEIPT).blocked();
    let f = Fixture::new(vec![
        Reply::json(200, r#"{"chunkBytes":3,"received":[0]}"#),
        reply,
    ])
    .await;
    let (_t, h, s, j) = setup_origin(f.origin.clone());
    h.transition(j.id, JobStatus::Running).unwrap();
    h.set_session(j.id, ID.parse().unwrap()).unwrap();
    let q = QueueController::new(h.clone()).unwrap();
    let u = net_uploader(&f, h.clone(), s);
    let (tx, rx) = command_channel();
    let (nt, nr) = tokio::sync::mpsc::channel(1);
    let task = tokio::spawn(q.serve(u, rx, nt));
    tokio::time::timeout(Duration::from_secs(5), arrived.notified())
        .await
        .unwrap();
    assert_eq!(nr.len(), 1);
    tx.send(C::Cancel(j.id)).await.unwrap();
    for _ in 0..100 {
        if h.upload_cancel_requested(j.id).unwrap() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(h.upload_cancel_requested(j.id).unwrap());
    release.add_permits(1);
    drop(tx);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        h.job(j.id).unwrap().unwrap().status,
        JobStatus::Completed(_)
    ));
    assert_eq!(f.log().len(), 2);
}
#[tokio::test]
async fn active_speed_retires_without_body_or_response_events() {
    let (reply, arrived, release) = Reply::json(204, "").blocked();
    let f = Fixture::new(vec![
        Reply::json(200, r#"{"chunkBytes":262144,"received":[]}"#),
        reply,
        Reply::json(200, RECEIPT),
    ])
    .await;
    let (t, h, s, original) = setup_origin(f.origin.clone());
    h.transition(original.id, JobStatus::Cancelled).unwrap();
    let path = t.path().join("large.zip");
    std::fs::write(&path, vec![7; 262144]).unwrap();
    let artifact = s.stage_file(&path, ArtifactKind::File).unwrap();
    let j = h.create_job(&artifact, &original.destination).unwrap();
    h.transition(j.id, JobStatus::Running).unwrap();
    h.set_session(j.id, ID.parse().unwrap()).unwrap();
    h.transition(j.id, JobStatus::Interrupted).unwrap();
    let q = QueueController::new(h.clone()).unwrap();
    let u = net_uploader(&f, h.clone(), s);
    let (tx, rx) = command_channel();
    let (nt, mut nr) = tokio::sync::mpsc::channel(32);
    let slow_ui = nt.clone();
    let task = tokio::spawn(q.serve(u, rx, nt));
    tokio::time::timeout(Duration::from_secs(5), arrived.notified())
        .await
        .unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_millis(250)).await;
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }
    let mut speed_seen = false;
    while let Ok(e) = nr.try_recv() {
        if matches!(
            e,
            E::Progress {
                speed: Some(_),
                acknowledged: 0,
                ..
            }
        ) {
            speed_seen = true;
        }
    }
    assert!(
        speed_seen,
        "speed must update before response/accepted progress"
    );
    for _ in 0..32 {
        slow_ui.try_send(E::Changed(original.id)).unwrap();
    }
    tokio::time::advance(Duration::from_secs(6)).await;
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }
    let mut waiting_seen = false;
    while let Ok(e) = nr.try_recv() {
        if matches!(
            e,
            E::Progress {
                speed: None,
                acknowledged: 0,
                ..
            }
        ) {
            waiting_seen = true;
        }
    }
    // The stale update occurred while notifications were full. Its latest
    // coalesced value must be delivered once the consumer has room again.
    tokio::time::advance(Duration::from_millis(250)).await;
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }
    while let Ok(e) = nr.try_recv() {
        assert!(
            !matches!(e, E::Progress { speed: Some(_), .. }),
            "do not replay a stale coalesced speed"
        );
        if matches!(
            e,
            E::Progress {
                speed: None,
                acknowledged: 0,
                ..
            }
        ) {
            waiting_seen = true;
        }
    }
    assert!(
        waiting_seen,
        "published rate must retire with no new body/response events"
    );
    tokio::time::resume();
    release.add_permits(1);
    until(&mut nr, |e| matches!(e, E::Changed(id) if *id == j.id)).await;
    drop(tx);
    task.await.unwrap().unwrap();
}
#[test]
fn restart_failure_cannot_publish_an_extra_job() {
    let (t, h, _s, j) = setup();
    h.transition(j.id, JobStatus::Running).unwrap();
    h.transition(j.id, JobStatus::Uncertain).unwrap();
    let c = rusqlite::Connection::open(t.path().join("h.db")).unwrap();
    c.execute_batch("CREATE TRIGGER reject_cancel BEFORE UPDATE OF status ON jobs WHEN NEW.status='cancelled' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
    let mut q = QueueController::new(h.clone()).unwrap();
    assert!(
        q.handle(C::Restart(j.id))
            .iter()
            .any(|e| matches!(e, E::Error(_)))
    );
    let recovered = h.recover_jobs().unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].id, j.id);
    assert_eq!(recovered[0].status, JobStatus::Uncertain);
}
#[tokio::test]
async fn restart_replay_under_pause_and_reopen_has_one_replacement() {
    let f = Fixture::new(vec![
        Reply::json(201, format!(r#"{{"id":"{ID}","chunkBytes":3}}"#)),
        Reply::json(204, ""),
        Reply::json(200, RECEIPT),
    ])
    .await;
    let (t, h, s, j) = setup_origin(f.origin.clone());
    h.transition(j.id, JobStatus::Running).unwrap();
    h.transition(j.id, JobStatus::Uncertain).unwrap();
    let mut q = QueueController::new(h.clone()).unwrap();
    q.handle(C::PauseAll);
    let replacement = |effects: Vec<E>| {
        effects
            .into_iter()
            .find_map(|e| match e {
                E::Restarted { replacement, .. } => Some(replacement),
                _ => None,
            })
            .unwrap()
    };
    let first = replacement(q.handle(C::Restart(j.id)));
    assert_eq!(replacement(q.handle(C::Restart(j.id))), first);
    drop(q);
    let reopened = Arc::new(History::open(&t.path().join("h.db")).unwrap());
    let mut q = QueueController::new(reopened.clone()).unwrap();
    assert_eq!(replacement(q.handle(C::Restart(j.id))), first);
    let remaining = reopened.recover_jobs().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, first);
    let u = net_uploader(&f, reopened.clone(), s);
    let (tx, rx) = command_channel();
    let (nt, mut nr) = tokio::sync::mpsc::channel(32);
    let task = tokio::spawn(q.serve(u, rx, nt));
    tx.send(C::ResumeAll).await.unwrap();
    until(&mut nr, |e| matches!(e, E::Changed(id) if *id == first)).await;
    drop(tx);
    task.await.unwrap().unwrap();
    assert!(matches!(
        reopened.job(first).unwrap().unwrap().status,
        JobStatus::Completed(_)
    ));
    assert_eq!(
        f.log().iter().filter(|r| r.path == "/api/uploads").count(),
        1
    );
    assert!(
        reopened.restart_upload(first).is_err(),
        "completed receipt cannot be restarted"
    );
}
#[test]
fn payload_finishing_after_pause_cannot_publish_a_speed() {
    let (_t, h, _s, j) = setup();
    let mut q = QueueController::new(h).unwrap();
    let j = start(&mut q, j.id);
    let began = tokio::time::Instant::now();
    q.handle(C::PauseAll);
    let effects = q.transfer(
        vectrcap::vectr::uploader::UploadEvent::Payload {
            job: j.id,
            attempt: j.attempt,
            bytes: 100,
            elapsed: Duration::from_secs(1),
            started: began,
            at: began,
            epoch: 0,
        },
        Duration::from_secs(1),
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, E::Progress { speed: None, .. }))
    );
}
#[tokio::test(start_paused = true)]
async fn same_tick_pause_resume_discards_old_payload_epoch() {
    let (_t, h, _s, j) = setup();
    let mut q = QueueController::new(h).unwrap();
    let running = start(&mut q, j.id);
    let began = tokio::time::Instant::now();
    q.handle(C::PauseAll);
    q.handle(C::ResumeAll);
    let effects = q.transfer(
        vectrcap::vectr::uploader::UploadEvent::Payload {
            job: j.id,
            attempt: running.attempt,
            bytes: 100,
            elapsed: Duration::from_secs(1),
            started: began,
            at: began,
            epoch: 0,
        },
        Duration::from_secs(1),
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, E::Progress { speed: None, .. })),
        "equal timestamps cannot identify a pause epoch"
    );
}
