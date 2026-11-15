use image::{Rgba, RgbaImage, imageops::crop_imm};
use std::time::Duration;
use vectrcap::{
    capture::{geometry::PhysicalRect, windows::WindowId},
    scrolling::{session::*, *},
};
fn options() -> ScrollOptions {
    ScrollOptions {
        target: WindowId::new(1).unwrap(),
        monitor_id: "screen".into(),
        rect: PhysicalRect::new(0, 0, 17, 40).unwrap(),
        maximum_duration: Duration::from_secs(600),
        maximum_segments: 1000,
    }
}
fn identity() -> TargetIdentity {
    TargetIdentity {
        target: options().target,
        monitor_id: "screen".into(),
        window_rect: PhysicalRect::new(0, 0, 100, 100).unwrap(),
        viewport: options().rect,
        focused: true,
    }
}
fn image(y: u32) -> RgbaImage {
    crop_imm(
        &RgbaImage::from_fn(17, 120, |x, y| {
            let n =
                (y.wrapping_mul(2654435761) ^ x.wrapping_mul(2246822519)).wrapping_mul(3266489917);
            Rgba([n as u8, (n >> 8) as u8, (n >> 16) as u8, 255])
        }),
        0,
        y,
        17,
        40,
    )
    .to_image()
}
fn start(o: ScrollOptions) -> ScrollingSession {
    let mut s = ScrollingSession::default();
    s.handle(ScrollInput::Select {
        options: o,
        identity: identity(),
        now: Duration::ZERO,
    });
    s.handle(ScrollInput::Tick(Duration::from_secs(3)));
    s
}
fn stable(s: &mut ScrollingSession, y: u32, ms: u64) {
    for t in [ms, ms + 200] {
        s.handle(ScrollInput::Frame {
            image: image(y),
            identity: identity(),
            now: Duration::from_millis(t),
        });
    }
}
#[test]
fn manual_capture_emits_no_input_injection() {
    let mut s = ScrollingSession::default();
    let effects = s.handle(ScrollInput::Select {
        options: options(),
        identity: identity(),
        now: Duration::ZERO,
    });
    assert!(
        effects
            .iter()
            .all(|e| matches!(e, ScrollEffect::HideSelection))
    );
    let effects = s.handle(ScrollInput::Tick(Duration::from_secs(3)));
    assert!(
        effects
            .iter()
            .all(|e| matches!(e, ScrollEffect::StartCandidates(_)))
    );
    stable(&mut s, 0, 3000);
    assert_eq!(s.accepted_segments(), 1);
}
#[test]
fn focus_or_target_change_pauses() {
    for mutate in [0, 1, 2, 3] {
        let mut s = start(options());
        stable(&mut s, 0, 3000);
        let mut id = identity();
        match mutate {
            0 => id.focused = false,
            1 => id.target = WindowId::new(2).unwrap(),
            2 => id.monitor_id = "other".into(),
            _ => id.window_rect = PhysicalRect::new(1, 0, 100, 100).unwrap(),
        };
        s.handle(ScrollInput::Observe(id));
        assert_eq!(s.state(), ScrollState::Paused);
        assert_eq!(s.accepted_segments(), 1);
    }
}
#[test]
fn resume_revalidates_identity_and_overlap() {
    let mut s = start(options());
    stable(&mut s, 0, 3000);
    let mut id = identity();
    id.focused = false;
    s.handle(ScrollInput::Observe(id));
    let mut id = identity();
    id.viewport = PhysicalRect::new(1, 0, 17, 40).unwrap();
    assert!(s.handle(ScrollInput::Resume(id)).is_empty());
    assert_eq!(s.state(), ScrollState::Paused);
    s.handle(ScrollInput::Resume(identity()));
    stable(&mut s, 60, 4000);
    assert_eq!(s.state(), ScrollState::Paused);
    assert_eq!(s.accepted_segments(), 1);
}
#[test]
fn stop_keeps_reliable_partial_prefix() {
    let mut s = start(options());
    stable(&mut s, 0, 3000);
    stable(&mut s, 60, 4000);
    s.handle(ScrollInput::Stop);
    let r = s.take_review().unwrap();
    assert_eq!(r.image, image(0));
    assert_eq!(r.accepted_segments, 1);
    assert!(r.partial_reason.is_some());
}
#[test]
fn cancel_never_saves_or_uploads() {
    let mut s = start(options());
    stable(&mut s, 0, 3000);
    let effects = s.handle(ScrollInput::Cancel);
    assert!(
        effects
            .iter()
            .all(|e| matches!(e, ScrollEffect::StopCandidates))
    );
    assert!(s.take_review().is_none());
    assert_eq!(s.state(), ScrollState::Cancelled);
    assert!(s.handle(ScrollInput::Stop).is_empty());
}
#[test]
fn single_frame_can_be_reviewed() {
    let mut s = start(options());
    stable(&mut s, 0, 3000);
    assert!(
        s.handle(ScrollInput::Stop)
            .iter()
            .any(|e| matches!(e, ScrollEffect::Review))
    );
    assert_eq!(s.take_review().unwrap().image, image(0));
}
#[test]
fn limits_stop_before_overallocation() {
    let mut o = options();
    o.maximum_segments = 1;
    let mut s = start(o);
    stable(&mut s, 0, 3000);
    assert_eq!(s.state(), ScrollState::Review);
    let r = s.take_review().unwrap();
    assert!(r.partial_reason.is_some());
    assert_eq!(r.image, image(0));
    let mut s = start(options());
    stable(&mut s, 0, 3000);
    s.handle(ScrollInput::Tick(Duration::from_secs(604)));
    assert_eq!(s.state(), ScrollState::Review);
    assert!(s.take_review().unwrap().partial_reason.is_some());
}
#[test]
fn stitched_review_requires_explicit_final_action() {
    let mut s = start(options());
    stable(&mut s, 0, 3000);
    let effects = s.handle(ScrollInput::Interrupt("Session locked"));
    assert!(
        effects
            .iter()
            .all(|e| matches!(e, ScrollEffect::StopCandidates | ScrollEffect::Review))
    );
    let r = s.take_review().unwrap();
    let mut review = vectrcap::ui::scrolling_review::ScrollingReview::new(r).unwrap();
    assert!(review.partial_reason().is_some());
    assert!(!review.accepted());
    review.accept_partial();
    assert!(review.accepted());
    assert!(review.partial_reason().is_some());
}
#[test]
fn remapped_scrolling_hotkey_stops_to_review_without_upload() {
    use vectrcap::app::{events::AppCommand, shortcuts::*};
    let mut binding = ShortcutBinding::unassigned(ShortcutAction::ToggleScrollingCapture);
    binding.chord = Some(KeyChord {
        ctrl: true,
        alt: true,
        shift: false,
        win: false,
        key: KeyCode::from_vk(0x59).unwrap(),
    });
    binding.enabled = true;
    let mut saved = ShortcutSettings::default();
    *saved
        .bindings
        .iter_mut()
        .find(|b| b.action == ShortcutAction::ToggleScrollingCapture)
        .unwrap() = binding;
    struct Registrar(std::sync::Arc<std::sync::Mutex<Vec<(RegistrationId, KeyChord)>>>);
    impl ShortcutRegistrar for Registrar {
        fn register(
            &self,
            id: RegistrationId,
            chord: KeyChord,
        ) -> Result<(), vectrcap::app::error::AppError> {
            self.0.lock().unwrap().push((id, chord));
            Ok(())
        }
        fn unregister(&self, _: RegistrationId) {}
    }
    let persisted = serde_json::to_vec(&saved).unwrap();
    let saved: ShortcutSettings = serde_json::from_slice(&persisted).unwrap();
    let registrations = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (manager, failed) = ShortcutManager::new(
        Box::new(Registrar(registrations.clone())),
        saved.effective(),
    );
    assert!(failed.is_empty());
    let id = registrations
        .lock()
        .unwrap()
        .iter()
        .find(|(_, chord)| chord.key.vk() == 0x59 && chord.ctrl && chord.alt)
        .unwrap()
        .0;
    let action = manager.action_for_id(id.0).unwrap();
    assert_eq!(action, ShortcutAction::ToggleScrollingCapture);
    let outcome = manager
        .handle(
            ShortcutEvent {
                action,
                repeat: false,
            },
            ShortcutFocus::Normal,
        )
        .unwrap();
    assert!(matches!(
        outcome,
        ShortcutOutcome::Command(AppCommand::ToggleScrollingFor(_))
    ));
    let mut s = start(options());
    stable(&mut s, 0, 3000);
    s.handle(ScrollInput::Stop);
    assert_eq!(s.state(), ScrollState::Review);
}

#[test]
fn successful_segments_match_exact_original_prefix() {
    let mut s = start(options());
    stable(&mut s, 0, 3000);
    stable(&mut s, 13, 4000);
    stable(&mut s, 26, 5000);
    s.handle(ScrollInput::Stop);
    let r = s.take_review().unwrap();
    assert_eq!(r.accepted_segments, 3);
    let mut bytes = image(0).into_raw();
    for y in [13, 26] {
        let next = image(y).into_raw();
        bytes.extend_from_slice(&next[27 * 17 * 4..]);
    }
    assert_eq!(r.image.as_raw(), &bytes);
    assert!(r.partial_reason.is_none());
}
#[test]
fn invalid_options_and_countdown_cancel_never_start_capture() {
    for invalid in [0, 1, 2] {
        let mut o = options();
        match invalid {
            0 => o.maximum_segments = 1001,
            1 => o.maximum_duration = Duration::from_secs(601),
            _ => o.maximum_segments = 0,
        };
        let mut s = ScrollingSession::default();
        assert!(
            s.handle(ScrollInput::Select {
                options: o,
                identity: identity(),
                now: Duration::ZERO
            })
            .iter()
            .any(|e| matches!(e, ScrollEffect::Error(_)))
        );
    }
    let mut s = ScrollingSession::default();
    s.handle(ScrollInput::Select {
        options: options(),
        identity: identity(),
        now: Duration::ZERO,
    });
    s.handle(ScrollInput::Cancel);
    assert!(
        s.handle(ScrollInput::Tick(Duration::from_secs(3)))
            .is_empty()
    );
}
mod controller_integration {
    use super::*;
    use std::{
        path::Path,
        sync::{Arc, Mutex},
    };
    use vectrcap::{
        app::{
            controller::AppController, destination_manager::FileDestinationRepository,
            destinations::CredentialKey, error::AppError, events::*,
        },
        persistence::{artifacts::ArtifactStore, history::History, settings::Settings},
        platform::{
            CredentialStore,
            windows::{clipboard::ClipboardWriter, shell_actions::ShellActions},
        },
        scrolling::runtime::{RuntimeEvent, ScrollUpdate},
        vectr::client::UploadToken,
    };
    #[derive(Default)]
    struct Boundary(Mutex<Vec<RgbaImage>>);
    impl CredentialStore for Boundary {
        fn read(&self, _: &CredentialKey) -> Result<UploadToken, AppError> {
            Err(AppError::NeedsSetup("fixture has no destination"))
        }
        fn write(&self, _: &CredentialKey, _: &UploadToken) -> Result<(), AppError> {
            Ok(())
        }
        fn delete(&self, _: &CredentialKey) -> Result<(), AppError> {
            Ok(())
        }
    }
    impl ShellActions for Boundary {
        fn open_path(&self, _: &Path) -> Result<(), AppError> {
            panic!("unexpected shell action")
        }
        fn open_url(&self, _: &str) -> Result<(), AppError> {
            panic!("unexpected shell action")
        }
        fn reveal_path(&self, _: &Path) -> Result<(), AppError> {
            panic!("unexpected shell action")
        }
    }
    impl ClipboardWriter for Boundary {
        fn write_text(&self, _: &str) -> Result<(), AppError> {
            panic!("unexpected clipboard text")
        }
        fn write_image(&self, i: &RgbaImage) -> Result<(), AppError> {
            self.0.lock().unwrap().push(i.clone());
            Ok(())
        }
    }
    fn app(root: &Path) -> (AppController, Arc<Boundary>) {
        let mut settings = Settings::default();
        settings.archive.archive_root = Some(root.join("archive"));
        settings.save(&root.join("settings.json")).unwrap();
        let boundary = Arc::new(Boundary::default());
        (
            AppController::open(
                Box::new(FileDestinationRepository::open(&root.join("settings.json")).unwrap()),
                Arc::new(History::open(&root.join("history.db")).unwrap()),
                boundary.clone(),
                Arc::new(ArtifactStore::new(root.join("spool")).unwrap()),
                boundary.clone(),
                boundary.clone(),
            )
            .unwrap(),
            boundary,
        )
    }
    fn review(
        app: &mut AppController,
        image: RgbaImage,
        partial: Option<&'static str>,
    ) -> Arc<Mutex<vectrcap::app::controller::CaptureSurface>> {
        app.dispatch(AppCommand::ToggleScrolling);
        let id = app.scrolling_view().unwrap().id;
        let effects = app.on_event(AppEvent::Scrolling(RuntimeEvent {
            session: id,
            update: ScrollUpdate {
                state: ScrollState::Review,
                options: options(),
                accepted_segments: 1,
                dimensions: Some(image.dimensions()),
                partial_reason: partial,
                countdown: Duration::ZERO,
            },
            review: Some(ScrollReview {
                image,
                accepted_segments: 1,
                partial_reason: partial,
            }),
            error: None,
        }));
        effects
            .into_iter()
            .find_map(|e| {
                if let AppEffect::CaptureSurface(Some(s)) = e {
                    Some(s)
                } else {
                    None
                }
            })
            .unwrap()
    }
    fn finish(
        surface: &Arc<Mutex<vectrcap::app::controller::CaptureSurface>>,
        action: vectrcap::ui::capture_overlay::OverlayAction,
    ) -> vectrcap::ui::image_export::ImageFinish {
        let (tx, rx) = std::sync::mpsc::channel();
        surface.lock().unwrap().overlay.prepare_output(move || {
            let _ = tx.send(());
        });
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
            Err(error) => panic!("output worker timeout: {error}"),
        };
        let mut s = surface.lock().unwrap();
        s.overlay.prepare_output(|| {});
        s.overlay.confirm_finish(action).unwrap()
    }
    #[test]
    fn partial_review_blocks_output_until_accepted_then_copies_edited_pixels() {
        use vectrcap::{
            editor::{Annotation, EditorCommand},
            ui::capture_overlay::OverlayAction,
        };
        let dir = tempfile::tempdir().unwrap();
        let (mut app, boundary) = app(dir.path());
        let surface = review(&mut app, image(0), Some("Focus changed"));
        let id = surface.lock().unwrap().session;
        assert!(!dir.path().join("archive").exists());
        let prepared = finish(&surface, OverlayAction::Copy);
        let effects = app.dispatch(AppCommand::FinishCapture {
            session: id,
            finish: prepared,
            save_path: None,
        });
        assert!(effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
        assert!(boundary.0.lock().unwrap().is_empty());
        {
            let mut s = surface.lock().unwrap();
            s.scrolling_review.as_mut().unwrap().accepted = true;
            s.overlay
                .apply_command(EditorCommand::Insert(
                    Annotation::redaction(PhysicalRect::new(2, 3, 4, 5).unwrap(), [9, 8, 7])
                        .unwrap(),
                ))
                .unwrap();
        }
        let prepared = finish(&surface, OverlayAction::Copy);
        let effects = app.dispatch(AppCommand::FinishCapture {
            session: id,
            finish: prepared,
            save_path: None,
        });
        assert!(!effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
        assert_eq!(
            boundary.0.lock().unwrap()[0].get_pixel(3, 4).0,
            [9, 8, 7, 255]
        );
    }
    #[test]
    fn failed_pin_preserves_review_and_other_finish_actions() {
        use vectrcap::ui::capture_overlay::OverlayAction;
        let dir = tempfile::tempdir().unwrap();
        let (mut app, boundary) = app(dir.path());
        let pixels = RgbaImage::from_pixel(3, 32761, Rgba([20, 40, 60, 255]));
        let surface = review(&mut app, pixels.clone(), None);
        let id = surface.lock().unwrap().session;
        let prepared = finish(&surface, OverlayAction::Pin);
        let effects = app.dispatch(AppCommand::FinishCapture {
            session: id,
            finish: prepared,
            save_path: None,
        });
        assert!(effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
        assert!(!dir.path().join("archive").exists());
        assert_eq!(
            surface.lock().unwrap().overlay.document().source().as_ref(),
            &pixels
        );
        let prepared = finish(&surface, OverlayAction::Copy);
        let effects = app.dispatch(AppCommand::FinishCapture {
            session: id,
            finish: prepared,
            save_path: None,
        });
        assert!(!effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
        assert_eq!(boundary.0.lock().unwrap()[0], pixels);
    }
    #[test]
    fn pin_display_failure_preserves_review_without_archiving() {
        use vectrcap::ui::capture_overlay::OverlayAction;
        let dir = tempfile::tempdir().unwrap();
        let (mut app, _) = app(dir.path());
        let surface = review(&mut app, image(0), None);
        let id = surface.lock().unwrap().session;
        let prepared = finish(&surface, OverlayAction::Pin);
        let effects = app.dispatch(AppCommand::FinishCapture {
            session: id,
            finish: prepared,
            save_path: None,
        });
        assert!(effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
        assert!(
            !dir.path().join("archive").exists(),
            "display validation must precede archive persistence"
        );
        assert_eq!(
            surface.lock().unwrap().overlay.document().source().as_ref(),
            &image(0)
        );
    }
    #[test]
    fn stop_during_selection_cancels_and_quit_drains_without_output() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, boundary) = app(dir.path());
        app.dispatch(AppCommand::ToggleScrolling);
        let effects = app.dispatch(AppCommand::ToggleScrolling);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, AppEffect::CancelScrolling))
        );
        assert!(app.scrolling_view().is_none());
        app.dispatch(AppCommand::ToggleScrolling);
        let effects = app.dispatch(AppCommand::Quit);
        assert!(effects.iter().any(|e| matches!(e, AppEffect::DrainWorker)));
        assert!(boundary.0.lock().unwrap().is_empty());
    }
}
#[test]
fn native_target_event_filter_catches_focus_and_geometry_without_input_injection() {
    use vectrcap::scrolling::runtime::target_event_invalidates;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EVENT_OBJECT_DESTROY, EVENT_OBJECT_LOCATIONCHANGE, EVENT_SYSTEM_FOREGROUND, OBJID_WINDOW,
    };
    assert!(target_event_invalidates(EVENT_SYSTEM_FOREGROUND, 44, 0, 55));
    assert!(target_event_invalidates(
        EVENT_OBJECT_LOCATIONCHANGE,
        55,
        OBJID_WINDOW,
        55
    ));
    assert!(target_event_invalidates(
        EVENT_OBJECT_DESTROY,
        55,
        OBJID_WINDOW,
        55
    ));
    assert!(!target_event_invalidates(
        EVENT_OBJECT_LOCATIONCHANGE,
        44,
        OBJID_WINDOW,
        55
    ));
    assert!(!target_event_invalidates(
        EVENT_OBJECT_LOCATIONCHANGE,
        55,
        1,
        55
    ));
    assert!(!target_event_invalidates(EVENT_SYSTEM_FOREGROUND, 44, 0, 0));
}
#[test]
fn terminal_runtime_event_requires_final_pixels_or_cancel() {
    use vectrcap::scrolling::runtime::{RuntimeEvent, ScrollUpdate};
    let mut event = RuntimeEvent {
        session: uuid::Uuid::new_v4(),
        update: ScrollUpdate {
            state: ScrollState::Review,
            options: options(),
            accepted_segments: 1,
            dimensions: Some((17, 40)),
            partial_reason: None,
            countdown: Duration::ZERO,
        },
        review: None,
        error: None,
    };
    assert!(!event.is_terminal());
    event.review = Some(ScrollReview {
        image: image(0),
        accepted_segments: 1,
        partial_reason: None,
    });
    assert!(event.is_terminal());
    event.review = None;
    event.update.state = ScrollState::Cancelled;
    assert!(event.is_terminal());
}
#[test]
fn malformed_frame_layout_pauses_without_accepting_pixels() {
    let mut s = start(options());
    for t in [3000, 3200] {
        s.handle(ScrollInput::Frame {
            image: RgbaImage::from_raw(17, 40, vec![0; 17 * 40 * 4 + 4]).unwrap(),
            identity: identity(),
            now: Duration::from_millis(t),
        });
    }
    assert_eq!(s.state(), ScrollState::Paused);
    assert_eq!(s.accepted_segments(), 0);
    assert!(s.partial_reason().is_some());
}
#[test]
fn resume_after_countdown_focus_loss_cannot_bypass_three_second_delay() {
    let mut s = ScrollingSession::default();
    s.handle(ScrollInput::Select {
        options: options(),
        identity: identity(),
        now: Duration::ZERO,
    });
    let mut lost = identity();
    lost.focused = false;
    s.handle(ScrollInput::Observe(lost));
    s.handle(ScrollInput::Resume(identity()));
    let effects = s.handle(ScrollInput::Tick(Duration::from_secs(2)));
    assert!(effects.is_empty());
    assert_eq!(s.state(), ScrollState::Countdown);
    assert!(
        s.handle(ScrollInput::Tick(Duration::from_secs(3)))
            .iter()
            .any(|e| matches!(e, ScrollEffect::StartCandidates(_)))
    );
}
