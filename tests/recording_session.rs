use std::time::Duration;
use vectrcap::{
    capture::geometry::{MonitorGeometry, PhysicalRect},
    recording::{
        options::*,
        session::*,
        worker::{RecordingControl, RecordingEvent},
    },
};
fn monitor() -> MonitorGeometry {
    MonitorGeometry::new(
        "screen".into(),
        PhysicalRect::new(0, 0, 1920, 1080).unwrap(),
        1.0,
    )
    .unwrap()
}
fn options(mp4: bool) -> RecordingOptions {
    RecordingOptions::new(
        "screen".into(),
        PhysicalRect::new(30, 20, 640, 360).unwrap(),
        mp4,
    )
}
fn begin(mp4: bool) -> RecordingSession {
    let mut s = RecordingSession::default();
    s.handle(RecordingInput::Start {
        options: options(mp4),
        monitor: monitor(),
        now: Duration::ZERO,
    });
    s
}
fn running() -> RecordingSession {
    let mut s = begin(false);
    s.handle(RecordingInput::Tick(Duration::from_secs(3)));
    s.handle(RecordingInput::Worker {
        event: RecordingEvent::Started,
        now: Duration::from_secs(3),
    });
    s
}
fn pause(s: &mut RecordingSession) {
    s.handle(RecordingInput::PauseResume);
    s.handle(RecordingInput::Worker {
        event: RecordingEvent::Paused(Duration::from_secs(1)),
        now: Duration::from_secs(4),
    });
}
#[test]
fn rejects_cross_monitor_recording() {
    let mut s = RecordingSession::default();
    let mut o = options(false);
    o.rect = PhysicalRect::new(1800, 20, 640, 360).unwrap();
    assert!(
        s.handle(RecordingInput::Start {
            options: o,
            monitor: monitor(),
            now: Duration::ZERO
        })
        .iter()
        .any(|e| matches!(e, RecordingEffect::Error(_)))
    );
    assert_eq!(s.state(), RecordingState::Idle);
}
#[test]
fn countdown_is_cancellable() {
    let mut s = begin(false);
    assert_eq!(s.state(), RecordingState::Countdown);
    let e = s.handle(RecordingInput::Cancel);
    assert_eq!(s.state(), RecordingState::Idle);
    assert!(
        !e.iter()
            .any(|e| matches!(e, RecordingEffect::StartWorker(_) | RecordingEffect::Review))
    );
    assert!(
        s.handle(RecordingInput::Tick(Duration::from_secs(4)))
            .is_empty()
    );
}
#[test]
fn stop_waits_for_finalization() {
    let mut s = running();
    assert!(
        s.handle(RecordingInput::Stop)
            .iter()
            .any(|e| matches!(e, RecordingEffect::Control(RecordingControl::Stop)))
    );
    assert_eq!(s.state(), RecordingState::Finalizing);
    assert!(s.handle(RecordingInput::Stop).is_empty());
}
#[test]
fn confirmed_cancel_discards_partial() {
    let mut s = running();
    s.handle(RecordingInput::Cancel);
    assert_eq!(s.state(), RecordingState::Recording);
    assert!(s.confirmation_pending());
    let e = s.handle(RecordingInput::ConfirmDiscard(true));
    assert!(
        e.iter()
            .any(|e| matches!(e, RecordingEffect::Control(RecordingControl::Cancel)))
    );
    assert_eq!(s.state(), RecordingState::Finalizing);
}
#[test]
fn duration_limit_stops_cleanly() {
    let mut s = running();
    let e = s.handle(RecordingInput::Tick(Duration::from_secs(63)));
    assert!(
        e.iter()
            .any(|e| matches!(e, RecordingEffect::Control(RecordingControl::Stop)))
    );
}
#[test]
fn lock_or_display_change_never_switches_source() {
    for reason in [
        Interruption::Locked,
        Interruption::DisplayChanged,
        Interruption::Sleep,
    ] {
        let mut s = running();
        let e = s.handle(RecordingInput::Interrupt(reason));
        assert_eq!(s.state(), RecordingState::Interrupted);
        assert!(
            e.iter()
                .any(|e| matches!(e, RecordingEffect::Control(RecordingControl::Stop)))
        );
        assert_eq!(s.options().unwrap().monitor_id, "screen");
    }
}
#[test]
fn disk_exhaustion_never_uploads_partial() {
    let mut s = running();
    s.handle(RecordingInput::Failed(
        vectrcap::app::error::AppError::Encoder("disk full"),
    ));
    assert_eq!(s.state(), RecordingState::Failed);
    assert!(s.handle(RecordingInput::Upload).is_empty());
}
#[test]
fn review_upload_requires_explicit_action() {
    let mut s = running();
    assert!(s.handle(RecordingInput::Upload).is_empty());
    assert!(
        !s.handle(RecordingInput::Stop)
            .iter()
            .any(|e| matches!(e, RecordingEffect::Upload))
    );
}
#[test]
fn remapped_start_stop_respects_active_recording_format() {
    use vectrcap::app::{events::AppCommand, shortcuts::ShortcutAction};
    assert!(matches!(
        vectrcap::recording::session::shortcut_command(ShortcutAction::ToggleGifRecording),
        Some(AppCommand::ToggleRecording { mp4: false })
    ));
    let mut s = running();
    assert!(s.handle(RecordingInput::ToggleFormat(true)).is_empty());
    assert!(
        s.handle(RecordingInput::ToggleFormat(false))
            .iter()
            .any(|e| matches!(e, RecordingEffect::Control(RecordingControl::Stop)))
    );
}
#[test]
fn pause_resume_hotkey_updates_controls_and_active_time() {
    let mut s = running();
    let e = s.handle(RecordingInput::PauseResume);
    assert_eq!(s.state(), RecordingState::Recording);
    assert!(
        e.iter()
            .any(|e| matches!(e, RecordingEffect::Control(RecordingControl::Pause)))
    );
    assert!(s.handle(RecordingInput::PauseResume).is_empty());
    s.handle(RecordingInput::Worker {
        event: RecordingEvent::Paused(Duration::from_secs(1)),
        now: Duration::from_secs(4),
    });
    assert_eq!(s.state(), RecordingState::Paused);
    s.handle(RecordingInput::Tick(Duration::from_secs(44)));
    assert_eq!(s.active_elapsed(), Duration::from_secs(1));
    s.handle(RecordingInput::PauseResume);
    s.handle(RecordingInput::Worker {
        event: RecordingEvent::Resumed(Duration::from_secs(1)),
        now: Duration::from_secs(44),
    });
    s.handle(RecordingInput::Tick(Duration::from_secs(45)));
    assert_eq!(s.active_elapsed(), Duration::from_secs(2));
}
#[test]
fn stop_and_cancel_work_while_paused() {
    let mut s = running();
    pause(&mut s);
    assert!(
        s.handle(RecordingInput::Stop)
            .iter()
            .any(|e| matches!(e, RecordingEffect::Control(RecordingControl::Stop)))
    );
    let mut s = running();
    pause(&mut s);
    s.handle(RecordingInput::Cancel);
    assert!(
        s.handle(RecordingInput::ConfirmDiscard(true))
            .iter()
            .any(|e| matches!(e, RecordingEffect::Control(RecordingControl::Cancel)))
    );
}
#[test]
fn active_duration_limit_ignores_pause() {
    let mut s = running();
    pause(&mut s);
    assert!(
        s.handle(RecordingInput::Tick(Duration::from_secs(500)))
            .is_empty()
    );
    assert_eq!(s.state(), RecordingState::Paused);
}
#[test]
fn external_interruption_never_auto_resumes() {
    let mut s = running();
    pause(&mut s);
    s.handle(RecordingInput::Interrupt(Interruption::Locked));
    assert!(s.handle(RecordingInput::PauseResume).is_empty());
    assert!(
        s.handle(RecordingInput::Tick(Duration::from_secs(90)))
            .is_empty()
    );
}
#[test]
fn pause_before_first_frame_never_publishes_invalid_file() {
    let mut s = begin(false);
    s.handle(RecordingInput::Tick(Duration::from_secs(3)));
    assert!(s.handle(RecordingInput::PauseResume).is_empty());
    s.handle(RecordingInput::Failed(
        vectrcap::app::error::AppError::Encoder("no usable frame"),
    ));
    assert!(s.output().is_none());
    assert!(s.handle(RecordingInput::Upload).is_empty());
}
#[test]
fn discard_confirmation_preserves_prior_state_when_declined() {
    for paused in [false, true] {
        let mut s = running();
        if paused {
            pause(&mut s);
        }
        let state = s.state();
        s.handle(RecordingInput::Cancel);
        assert!(s.handle(RecordingInput::ConfirmDiscard(false)).is_empty());
        assert_eq!(s.state(), state);
        assert!(!s.confirmation_pending());
    }
}
#[test]
fn idle_controls_are_noops_and_gif_off_never_open_audio() {
    let mut s = RecordingSession::default();
    for i in [
        RecordingInput::Stop,
        RecordingInput::PauseResume,
        RecordingInput::Cancel,
    ] {
        assert!(s.handle(i).is_empty());
    }
    let mut s = begin(false);
    let e = s.handle(RecordingInput::Tick(Duration::from_secs(3)));
    let RecordingEffect::StartWorker(o) = e
        .into_iter()
        .find(|e| matches!(e, RecordingEffect::StartWorker(_)))
        .unwrap()
    else {
        panic!()
    };
    assert!(o.audio().is_none());
    assert_eq!(o.countdown_seconds, 0);
}

fn encoded(root: &std::path::Path) -> vectrcap::recording::EncodedRecording {
    use vectrcap::recording::{RecordingEncoder, frame::CapturedFrame, gif::GifEncoder};
    let mut o = options(false);
    o.rect = PhysicalRect::new(0, 0, 16, 16).unwrap();
    let mut e = GifEncoder::new(root, o).unwrap();
    e.push_video(&CapturedFrame::new(vec![255; 16 * 16 * 4], 16, 16, Duration::ZERO).unwrap())
        .unwrap();
    e.end_at(Duration::from_secs(1)).unwrap();
    Box::new(e).finish().unwrap()
}
fn stores(
    root: &std::path::Path,
) -> (
    std::sync::Arc<vectrcap::persistence::history::History>,
    vectrcap::persistence::artifacts::ArtifactStore,
    vectrcap::persistence::settings::ArchivePolicy,
) {
    use vectrcap::persistence::{
        artifacts::ArtifactStore, history::History, settings::ArchivePolicy,
    };
    (
        std::sync::Arc::new(History::open(&root.join("history.db")).unwrap()),
        ArtifactStore::new(root.join("spool")).unwrap(),
        ArchivePolicy {
            archive_root: Some(root.join("archive")),
            ..Default::default()
        },
    )
}
#[test]
fn recording_stop_archives_by_default() {
    use vectrcap::recording::review::RecordingReview;
    let root = tempfile::tempdir().unwrap();
    let (h, a, p) = stores(root.path());
    let mut r = RecordingReview::new(encoded(root.path()));
    r.persist(&a, h.clone(), &p, &Default::default(), false, false)
        .unwrap();
    assert!(r.archive_path().unwrap().exists());
    assert_eq!(h.browse(&Default::default()).unwrap().records.len(), 1);
    assert!(r.source_path().exists());
    assert_eq!(r.metadata().fps, 15);
}
#[test]
fn discard_removes_only_owned_recording() {
    use vectrcap::recording::review::RecordingReview;
    let root = tempfile::tempdir().unwrap();
    let (h, a, p) = stores(root.path());
    let foreign = root.path().join("foreign.gif");
    std::fs::write(&foreign, b"keep").unwrap();
    let mut r = RecordingReview::new(encoded(root.path()));
    r.persist(&a, h.clone(), &p, &Default::default(), false, false)
        .unwrap();
    let archive = r.archive_path().unwrap().to_owned();
    r.discard(&a, &h).unwrap();
    assert!(!archive.exists());
    assert!(foreign.exists());
}
#[test]
fn recording_archive_failure_keeps_finalized_temp() {
    use vectrcap::recording::review::RecordingReview;
    let root = tempfile::tempdir().unwrap();
    let (h, a, mut p) = stores(root.path());
    let bad = root.path().join("not-a-directory");
    std::fs::write(&bad, b"blocked").unwrap();
    p.archive_root = Some(bad);
    let mut r = RecordingReview::new(encoded(root.path()));
    assert!(
        r.persist(&a, h, &p, &Default::default(), false, false)
            .is_err()
    );
    assert!(r.source_path().exists());
    assert!(r.archive_path().is_none());
}
#[test]
fn pending_and_never_archive_only_when_policy_requires() {
    use vectrcap::{persistence::settings::StorageMode, recording::review::RecordingReview};
    for mode in [StorageMode::PendingOrFailed, StorageMode::NeverAutomatic] {
        let root = tempfile::tempdir().unwrap();
        let (h, a, mut p) = stores(root.path());
        p.storage_mode = mode;
        let mut r = RecordingReview::new(encoded(root.path()));
        r.persist(&a, h.clone(), &p, &Default::default(), false, false)
            .unwrap();
        assert!(r.archive_path().is_none());
        r.persist(&a, h, &p, &Default::default(), true, false)
            .unwrap();
        assert_eq!(
            r.archive_path().is_some(),
            mode == StorageMode::PendingOrFailed
        );
    }
}
#[test]
fn finalized_review_upload_is_explicit_and_interrupted_reason_is_retained() {
    let root = tempfile::tempdir().unwrap();
    let mut s = running();
    s.handle(RecordingInput::Interrupt(Interruption::Locked));
    let e = s.handle(RecordingInput::Finalized(Ok(encoded(root.path()))));
    assert_eq!(s.state(), RecordingState::Review);
    assert_eq!(s.interruption(), Some(Interruption::Locked));
    assert!(!e.iter().any(|e| matches!(e, RecordingEffect::Upload)));
    assert!(
        s.handle(RecordingInput::Upload)
            .iter()
            .any(|e| matches!(e, RecordingEffect::Upload))
    );
}

#[test]
fn native_interruption_messages_have_no_resume_mapping() {
    use vectrcap::platform::windows::recording_events::interruption_for_message;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    assert_eq!(
        interruption_for_message(WM_DISPLAYCHANGE, 0),
        Some(Interruption::DisplayChanged)
    );
    assert_eq!(
        interruption_for_message(WM_POWERBROADCAST, PBT_APMSUSPEND as usize),
        Some(Interruption::Sleep)
    );
    assert_eq!(
        interruption_for_message(WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK as usize),
        Some(Interruption::Locked)
    );
    assert_eq!(
        interruption_for_message(WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK as usize),
        None
    );
}
#[test]
fn recording_controls_never_overlap_selected_pixels() {
    use vectrcap::ui::recording_overlay::control_bounds;
    let m = PhysicalRect::new(-1920, 0, 1920, 1080).unwrap();
    let r = PhysicalRect::new(-1850, 200, 640, 360).unwrap();
    let c = control_bounds(r, m, (450, 160)).unwrap();
    assert!(m.contains_rect(c));
    assert!(c.intersection(r).is_none());
    assert!(control_bounds(m, m, (450, 160)).is_err());
    // Docked like the capture tools: top centre of the monitor when that is free.
    let dock = control_bounds(r, m, (560, 44)).unwrap();
    assert_eq!((dock.left(), dock.top()), (-1920 + (1920 - 560) / 2, 8));
    // A selection reaching the top pushes it to the bottom centre.
    let tall = PhysicalRect::new(-1920, 0, 1920, 600).unwrap();
    let low = control_bounds(tall, m, (560, 44)).unwrap();
    assert_eq!(low.top(), 1080 - 44 - 8);
    assert!(low.intersection(tall).is_none());
}

#[test]
fn worker_pause_ack_is_authoritative_for_displayed_time() {
    let mut s = running();
    s.handle(RecordingInput::PauseResume);
    s.handle(RecordingInput::Worker {
        event: RecordingEvent::Paused(Duration::from_millis(900)),
        now: Duration::from_secs(4),
    });
    s.handle(RecordingInput::Tick(Duration::from_secs(40)));
    assert_eq!(s.active_elapsed(), Duration::from_millis(900));
}
#[test]
fn cancel_from_review_uses_owned_discard_path() {
    let root = tempfile::tempdir().unwrap();
    let mut s = running();
    s.handle(RecordingInput::Stop);
    s.handle(RecordingInput::Finalized(Ok(encoded(root.path()))));
    assert!(
        s.handle(RecordingInput::Cancel)
            .iter()
            .any(|e| matches!(e, RecordingEffect::Discard))
    );
}
#[test]
fn gif_review_decodes_actual_finalized_pixels() {
    let root = tempfile::tempdir().unwrap();
    let o = encoded(root.path());
    let p = vectrcap::ui::recording_review::GifPlayback::open(o.path.clone()).unwrap();
    let f = p
        .frames
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert_eq!(f.image.size, [16, 16]);
    assert!(
        f.image
            .pixels
            .iter()
            .all(|p| *p == eframe::egui::Color32::WHITE)
    );
    assert_eq!(f.delay, Duration::from_secs(1));
}

mod controller_tests {
    use super::*;
    use std::{path::Path, sync::Arc};
    use vectrcap::{
        app::{
            controller::AppController,
            destination_manager::{DestinationEdit, FileDestinationRepository},
            destinations::CredentialKey,
            error::AppError,
            events::*,
        },
        persistence::{
            artifacts::ArtifactStore,
            history::History,
            settings::{ServerOrigin, Settings},
        },
        platform::{
            CredentialStore,
            windows::{clipboard::ClipboardWriter, shell_actions::ShellActions},
        },
        vectr::client::UploadToken,
    };
    #[derive(Default)]
    struct Boundary(std::sync::Mutex<std::collections::HashSet<CredentialKey>>);
    impl CredentialStore for Boundary {
        fn read(&self, k: &CredentialKey) -> Result<UploadToken, AppError> {
            if self.0.lock().unwrap().contains(k) {
                UploadToken::new("test-token".into())
            } else {
                Err(AppError::NeedsSetup("no test credential"))
            }
        }
        fn write(&self, k: &CredentialKey, _: &UploadToken) -> Result<(), AppError> {
            self.0.lock().unwrap().insert(k.clone());
            Ok(())
        }
        fn delete(&self, k: &CredentialKey) -> Result<(), AppError> {
            self.0.lock().unwrap().remove(k);
            Ok(())
        }
    }
    impl ClipboardWriter for Boundary {
        fn write_text(&self, _: &str) -> Result<(), AppError> {
            Ok(())
        }
    }
    impl ShellActions for Boundary {
        fn open_path(&self, _: &Path) -> Result<(), AppError> {
            Ok(())
        }
        fn open_url(&self, _: &str) -> Result<(), AppError> {
            Ok(())
        }
        fn reveal_path(&self, _: &Path) -> Result<(), AppError> {
            Ok(())
        }
    }
    fn app(root: &Path) -> (AppController, Arc<History>) {
        let mut settings = Settings::default();
        settings.archive.archive_root = Some(root.join("archive"));
        settings.save(&root.join("settings.json")).unwrap();
        let h = Arc::new(History::open(&root.join("history.db")).unwrap());
        let mut c = AppController::open(
            Box::new(FileDestinationRepository::open(&root.join("settings.json")).unwrap()),
            h.clone(),
            Arc::new(Boundary::default()),
            Arc::new(ArtifactStore::new(root.join("spool")).unwrap()),
            Arc::new(Boundary::default()),
            Arc::new(Boundary::default()),
        )
        .unwrap();
        let mut d = c.settings().destinations.clone();
        let id = d
            .add(
                "Test",
                ServerOrigin::parse("https://example.invalid").unwrap(),
                None,
            )
            .unwrap();
        let e = c.dispatch(AppCommand::ApplyDestinations {
            edit: DestinationEdit {
                destinations: d,
                tokens: vec![(id, UploadToken::new("test-token".into()).unwrap())],
            },
            revision: c.settings().revision,
        });
        assert!(!e.iter().any(|e| matches!(e, AppEffect::Error(_))));
        (c, h)
    }
    fn start(c: &mut AppController) -> uuid::Uuid {
        c.dispatch(AppCommand::ToggleRecording { mp4: false });
        let id = c.recording_view().unwrap().id;
        let snapshot = vectrcap::capture::request::CaptureSnapshot::new(
            vec![0; 1920 * 1080 * 4],
            PhysicalRect::new(0, 0, 1920, 1080).unwrap(),
            vec![monitor()],
            vec![],
        )
        .unwrap();
        c.on_event(AppEvent::RecordingAcquired {
            session: id,
            result: Ok(snapshot),
        });
        c.dispatch(AppCommand::RecordingSelection {
            session: id,
            rect: PhysicalRect::new(30, 20, 640, 360).unwrap(),
        });
        c.dispatch(AppCommand::RecordingSelectionClosed(id));
        c.dispatch(AppCommand::Recording(RecordingInput::Tick(
            Duration::from_secs(4),
        )));
        c.on_event(AppEvent::RecordingWorker {
            session: id,
            input: RecordingInput::Worker {
                event: RecordingEvent::Started,
                now: Duration::from_secs(4),
            },
        });
        id
    }
    #[test]
    fn paused_recording_blocks_screenshot_acquisition() {
        let root = tempfile::tempdir().unwrap();
        let (mut c, _) = app(root.path());
        let id = start(&mut c);
        c.dispatch(AppCommand::Recording(RecordingInput::PauseResume));
        c.on_event(AppEvent::RecordingWorker {
            session: id,
            input: RecordingInput::Worker {
                event: RecordingEvent::Paused(Duration::from_secs(1)),
                now: Duration::from_secs(5),
            },
        });
        let e = c.dispatch(AppCommand::StartCapture {
            mode: vectrcap::capture::request::CaptureMode::Region,
            selection: Default::default(),
        });
        assert!(e.iter().any(|e| matches!(e, AppEffect::Error(_))));
        assert!(
            !e.iter()
                .any(|e| matches!(e, AppEffect::AcquireCapture { .. }))
        );
    }
    #[test]
    fn finalized_recording_reuses_upload_queue_and_duplicate_upload_is_harmless() {
        let root = tempfile::tempdir().unwrap();
        let (mut c, h) = app(root.path());
        let id = start(&mut c);
        c.dispatch(AppCommand::Recording(RecordingInput::Stop));
        let e = c.on_event(AppEvent::RecordingWorker {
            session: id,
            input: RecordingInput::Finalized(Ok(encoded(root.path()))),
        });
        assert!(!e.iter().any(|e| matches!(e, AppEffect::Queue(_))));
        assert_eq!(h.shell_jobs().unwrap().len(), 0);
        assert!(c.recording_view().unwrap().archive_path.is_some());
        let e = c.dispatch(AppCommand::Recording(RecordingInput::Upload));
        assert!(e.iter().any(|e| matches!(
            e,
            AppEffect::Queue(vectrcap::vectr::queue::QueueCommand::Admit(_))
        )));
        assert_eq!(h.shell_jobs().unwrap().len(), 1);
        c.dispatch(AppCommand::Recording(RecordingInput::Upload));
        assert_eq!(h.shell_jobs().unwrap().len(), 1);
        let e = c.dispatch(AppCommand::Recording(RecordingInput::Discard));
        assert!(e.iter().any(|e| matches!(e, AppEffect::Error(_))));
        assert!(c.recording_view().unwrap().archive_path.unwrap().exists());
    }
    #[test]
    fn countdown_waits_until_selection_surface_is_hidden() {
        let root = tempfile::tempdir().unwrap();
        let (mut c, _) = app(root.path());
        c.dispatch(AppCommand::ToggleRecording { mp4: false });
        let id = c.recording_view().unwrap().id;
        c.on_event(AppEvent::RecordingAcquired {
            session: id,
            result: Ok(vectrcap::capture::request::CaptureSnapshot::new(
                vec![0; 1920 * 1080 * 4],
                PhysicalRect::new(0, 0, 1920, 1080).unwrap(),
                vec![monitor()],
                vec![],
            )
            .unwrap()),
        });
        c.dispatch(AppCommand::RecordingSelection {
            session: id,
            rect: PhysicalRect::new(30, 20, 640, 360).unwrap(),
        });
        assert!(
            !c.dispatch(AppCommand::Recording(RecordingInput::Tick(
                Duration::from_secs(4)
            )))
            .iter()
            .any(|e| matches!(e, AppEffect::StartRecordingWorker { .. }))
        );
        c.dispatch(AppCommand::RecordingSelectionClosed(id));
        assert!(
            c.dispatch(AppCommand::Recording(RecordingInput::Tick(
                Duration::from_secs(4)
            )))
            .iter()
            .any(|e| matches!(e, AppEffect::StartRecordingWorker { .. }))
        );
    }
    #[test]
    fn cancelled_upload_allows_owned_local_discard() {
        let root = tempfile::tempdir().unwrap();
        let (mut c, h) = app(root.path());
        let id = start(&mut c);
        c.dispatch(AppCommand::Recording(RecordingInput::Stop));
        c.on_event(AppEvent::RecordingWorker {
            session: id,
            input: RecordingInput::Finalized(Ok(encoded(root.path()))),
        });
        c.dispatch(AppCommand::Recording(RecordingInput::Upload));
        let job = h.shell_jobs().unwrap().remove(0);
        h.transition(job.id, vectrcap::persistence::history::JobStatus::Cancelled)
            .unwrap();
        let archive = c.recording_view().unwrap().archive_path.unwrap();
        let e = c.dispatch(AppCommand::Recording(RecordingInput::Discard));
        assert!(!e.iter().any(|e| matches!(e, AppEffect::Error(_))));
        assert!(!archive.exists());
        assert!(c.recording_view().is_none());
    }
    #[test]
    fn selected_recording_upload_honors_explicit_archive_skip() {
        use vectrcap::workflow::profile::CaptureTask;
        let root = tempfile::tempdir().unwrap();
        let (mut c, _) = app(root.path());
        let mut tasks = c.settings().tasks.clone();
        tasks.global.capture.insert(CaptureTask::Upload);
        c.dispatch(AppCommand::SetTaskProfiles {
            tasks,
            archive: c.settings().archive.clone(),
            revision: c.settings().revision,
        });
        let path = c.settings().archive.resolved_root().unwrap();
        std::fs::write(path, b"blocked").unwrap();
        let id = start(&mut c);
        c.dispatch(AppCommand::Recording(RecordingInput::Stop));
        c.on_event(AppEvent::RecordingWorker {
            session: id,
            input: RecordingInput::Finalized(Ok(encoded(root.path()))),
        });
        assert!(c.recording_view().unwrap().archive_failed);
        let effects = c.dispatch(AppCommand::RetryRecordingArchive {
            continue_without: true,
        });
        assert!(!effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
        let effects = c.dispatch(AppCommand::RunRecordingTasks);
        assert!(!effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
        assert_eq!(
            effects
                .iter()
                .filter(|e| matches!(e, AppEffect::JobAccepted(_)))
                .count(),
            1
        );
    }

    #[test]
    fn selected_recording_save_as_can_finish_without_upload() {
        use vectrcap::workflow::{profile::CaptureTask, runner::TaskCommand};
        let root = tempfile::tempdir().unwrap();
        let (mut c, _) = app(root.path());
        let mut tasks = c.settings().tasks.clone();
        tasks.global.capture.insert(CaptureTask::SaveAs);
        let mut archive = c.settings().archive.clone();
        archive.storage_mode = vectrcap::persistence::settings::StorageMode::NeverAutomatic;
        c.dispatch(AppCommand::SetTaskProfiles {
            tasks,
            archive,
            revision: c.settings().revision,
        });
        let id = start(&mut c);
        c.dispatch(AppCommand::Recording(RecordingInput::Stop));
        c.on_event(AppEvent::RecordingWorker {
            session: id,
            input: RecordingInput::Finalized(Ok(encoded(root.path()))),
        });
        c.dispatch(AppCommand::RunRecordingTasks);
        let task = c.task_views().into_iter().find(|r| !r.complete()).unwrap();
        let path = root.path().join("manual.gif");
        let effects = c.dispatch(AppCommand::Task {
            id: task.context().id,
            command: TaskCommand::SaveAs(path.clone()),
        });
        assert!(!effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
        assert!(path.exists());
        let effects = c.dispatch(AppCommand::CloseRecordingReview);
        assert!(!effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
        assert!(c.recording_view().is_none());
        assert!(path.exists());
    }
    #[test]
    fn selected_recording_archive_is_reused_by_explicit_save() {
        use vectrcap::workflow::profile::CaptureTask;
        let root = tempfile::tempdir().unwrap();
        let (mut c, h) = app(root.path());
        let mut tasks = c.settings().tasks.clone();
        tasks.global.capture.insert(CaptureTask::Upload);
        let mut archive = c.settings().archive.clone();
        archive.storage_mode = vectrcap::persistence::settings::StorageMode::PendingOrFailed;
        c.dispatch(AppCommand::SetTaskProfiles {
            tasks,
            archive,
            revision: c.settings().revision,
        });
        let id = start(&mut c);
        c.dispatch(AppCommand::Recording(RecordingInput::Stop));
        c.on_event(AppEvent::RecordingWorker {
            session: id,
            input: RecordingInput::Finalized(Ok(encoded(root.path()))),
        });
        assert!(c.recording_view().unwrap().archive_path.is_none());
        let effects = c.dispatch(AppCommand::RunRecordingTasks);
        assert!(!effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
        let original = c
            .recording_view()
            .unwrap()
            .archive_path
            .expect("selected tasks archive must belong to the recording review");
        let effects = c.dispatch(AppCommand::Recording(RecordingInput::Save));
        assert!(!effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
        assert_eq!(c.recording_view().unwrap().archive_path, Some(original));
        let item = h.browse(&Default::default()).unwrap().records.remove(0);
        assert!(item.local_available);
    }

    #[test]
    fn saved_review_can_close_without_deleting_archive() {
        let root = tempfile::tempdir().unwrap();
        let (mut c, _) = app(root.path());
        let id = start(&mut c);
        c.dispatch(AppCommand::Recording(RecordingInput::Stop));
        c.on_event(AppEvent::RecordingWorker {
            session: id,
            input: RecordingInput::Finalized(Ok(encoded(root.path()))),
        });
        let archive = c.recording_view().unwrap().archive_path.unwrap();
        c.dispatch(AppCommand::CloseRecordingReview);
        assert!(c.recording_view().is_none());
        assert!(archive.exists());
    }
    #[test]
    fn quitting_selection_cancels_acquisition_and_drains_once() {
        let root = tempfile::tempdir().unwrap();
        let (mut c, _) = app(root.path());
        c.dispatch(AppCommand::ToggleRecording { mp4: false });
        let effects = c.dispatch(AppCommand::Quit);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, AppEffect::CancelRecordingAcquisition))
        );
        assert_eq!(
            effects
                .iter()
                .filter(|e| matches!(e, AppEffect::DrainWorker))
                .count(),
            1
        );
    }
    #[test]
    fn audio_test_stops_when_recording_selection_closes_settings() {
        let root = tempfile::tempdir().unwrap();
        let (mut c, _) = app(root.path());
        let e = c.dispatch(AppCommand::ToggleRecording { mp4: false });
        assert!(e.iter().any(|e| matches!(e, AppEffect::StopAudioTest)));
    }
    #[test]
    fn active_recording_protects_destination_credential_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let (mut c, _) = app(root.path());
        start(&mut c);
        let id = c.settings().destinations.default_id().unwrap();
        let e = c.dispatch(AppCommand::ApplyDestinations {
            edit: DestinationEdit {
                destinations: c.settings().destinations.clone(),
                tokens: vec![(id, UploadToken::new("replacement-token".into()).unwrap())],
            },
            revision: c.settings().revision,
        });
        assert!(e.iter().any(|e| matches!(e, AppEffect::Error(_))));
    }
    #[test]
    fn declined_quit_preserves_paused_recording_and_clears_quit_intent() {
        let root = tempfile::tempdir().unwrap();
        let (mut c, _) = app(root.path());
        let id = start(&mut c);
        c.dispatch(AppCommand::Recording(RecordingInput::PauseResume));
        c.on_event(AppEvent::RecordingWorker {
            session: id,
            input: RecordingInput::Worker {
                event: RecordingEvent::Paused(Duration::from_secs(1)),
                now: Duration::from_secs(5),
            },
        });
        c.dispatch(AppCommand::Quit);
        assert_eq!(c.lifecycle, Lifecycle::Running);
        assert_eq!(c.recording_view().unwrap().state, RecordingState::Paused);
        c.dispatch(AppCommand::Recording(RecordingInput::ConfirmDiscard(false)));
        c.dispatch(AppCommand::Recording(RecordingInput::Cancel));
        c.dispatch(AppCommand::Recording(RecordingInput::ConfirmDiscard(true)));
        c.on_event(AppEvent::RecordingWorker {
            session: id,
            input: RecordingInput::Finalized(Err(AppError::Cancelled)),
        });
        assert_eq!(c.lifecycle, Lifecycle::Running);
        assert!(c.recording_view().is_none());
    }
}

#[test]
fn video_queue_waits_for_confirmed_audio_prefix() {
    use vectrcap::recording::{frame::CapturedFrame, worker::FrameQueue};
    let mut q = FrameQueue::default();
    q.push(CapturedFrame::new(vec![0; 16], 2, 2, Duration::from_millis(100)).unwrap());
    assert!(q.pop_before(Some(Duration::from_millis(100))).is_none());
    assert_eq!(q.len(), 1);
    assert!(q.pop_before(Some(Duration::from_millis(101))).is_some());
}

#[test]
fn explicit_save_protects_an_existing_automatic_archive_from_age_cleanup() {
    use vectrcap::{persistence::archive::ArchiveStore, recording::review::RecordingReview};
    let root = tempfile::tempdir().unwrap();
    let (h, a, mut p) = stores(root.path());
    p.cleanup_age_days = Some(1);
    let mut r = RecordingReview::new(encoded(root.path()));
    r.persist(&a, h.clone(), &p, &Default::default(), false, false)
        .unwrap();
    r.persist(&a, h.clone(), &p, &Default::default(), false, true)
        .unwrap();
    let path = r.archive_path().unwrap().to_owned();
    ArchiveStore::new(h, p.archive_root.clone().unwrap(), Default::default())
        .cleanup(&p, chrono::Local::now() + chrono::TimeDelta::days(2))
        .unwrap();
    assert!(path.exists());
}

#[test]
fn clock_failure_stops_worker_before_reporting_failure() {
    let mut s = running();
    let effects = s.handle(RecordingInput::Tick(Duration::from_secs(1)));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, RecordingEffect::Control(RecordingControl::Stop)))
    );
    assert_eq!(s.state(), RecordingState::Interrupted);
}

#[test]
fn recording_selection_cannot_enable_annotation_tools() {
    use vectrcap::{
        capture::{geometry::PhysicalPoint, request::CaptureSnapshot},
        ui::{annotation_toolbar::AnnotationTool, capture_overlay::CaptureOverlay},
    };
    let snapshot = CaptureSnapshot::new(
        vec![0; 64 * 64 * 4],
        PhysicalRect::new(0, 0, 64, 64).unwrap(),
        vec![
            MonitorGeometry::new(
                "small".into(),
                PhysicalRect::new(0, 0, 64, 64).unwrap(),
                1.0,
            )
            .unwrap(),
        ],
        vec![],
    )
    .unwrap();
    let mut overlay =
        CaptureOverlay::from_snapshot(snapshot, PhysicalPoint { x: 2, y: 2 }, (4, 4)).unwrap();
    overlay.set_recording_selection();
    assert!(overlay.set_tool(AnnotationTool::Freehand).is_err());
    assert_eq!(overlay.tool(), AnnotationTool::Region);
}
