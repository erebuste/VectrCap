mod task11_support;
use task11_support::controller;
use tokio_util::sync::CancellationToken;
use vectrcap::{
    app::{events::*, shortcuts::*},
    capture::{
        geometry::{MonitorGeometry, PhysicalPoint as Point, PhysicalRect as Rect},
        request::{CaptureService, CaptureSource, Countdown, SourcePurpose},
    },
    tools::{UtilitySnapshot, session::*},
};
fn snapshot() -> UtilitySnapshot {
    let b = Rect::new(0, 0, 2, 2).unwrap();
    UtilitySnapshot::new(
        [20, 40, 60, 255].repeat(4),
        b,
        vec![MonitorGeometry::new("a".into(), b, 1.0).unwrap()],
        1,
    )
    .unwrap()
}
fn start(c: &mut vectrcap::app::controller::AppController) -> uuid::Uuid {
    let effects = c.dispatch(AppCommand::StartUtility(UtilityKind::Sampler));
    match effects.as_slice() {
        [AppEffect::AcquireUtility { session, .. }] => *session,
        _ => panic!("expected one utility acquisition"),
    }
}
#[test]
fn utility_never_archives_or_uploads() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(dir.path());
    let id = start(&mut c);
    let effects = c.on_event(AppEvent::UtilityAcquired {
        session: id,
        result: Ok(snapshot()),
    });
    assert!(matches!(
        effects.as_slice(),
        [AppEffect::UtilitySurface(Some(_))]
    ));
    assert!(
        c.dispatch(AppCommand::Utility {
            session: id,
            input: UtilityInput::Select
        })
        .iter()
        .all(|e| matches!(e, AppEffect::UtilitySurface(_)))
    );
    assert_eq!(c.history_page().unwrap().records.len(), 0);
    assert_eq!(
        std::fs::read_dir(dir.path().join("spool")).unwrap().count(),
        0
    );
}
struct Source {
    purpose: Option<SourcePurpose>,
    chrome_visible: std::rc::Rc<std::cell::Cell<bool>>,
}
impl CaptureSource for Source {
    fn set_purpose(&mut self, p: SourcePurpose) -> Result<(), vectrcap::app::error::AppError> {
        self.purpose = Some(p);
        Ok(())
    }
    fn monitors(&mut self) -> Result<Vec<MonitorGeometry>, vectrcap::app::error::AppError> {
        Ok(snapshot().monitors().to_vec())
    }
    fn pointer(&mut self) -> Result<Point, vectrcap::app::error::AppError> {
        Ok(Point { x: 0, y: 0 })
    }
    fn windows(
        &mut self,
    ) -> Result<Vec<vectrcap::capture::windows::WindowCandidate>, vectrcap::app::error::AppError>
    {
        Ok(vec![])
    }
    fn prepare_window(
        &mut self,
        _: vectrcap::capture::windows::WindowCandidate,
    ) -> Result<(), vectrcap::app::error::AppError> {
        unreachable!()
    }
    fn window(
        &mut self,
        _: vectrcap::capture::windows::WindowCandidate,
        _: bool,
    ) -> Result<Vec<u8>, vectrcap::app::error::AppError> {
        unreachable!()
    }
    fn desktop(
        &mut self,
        _: Rect,
        _: &[MonitorGeometry],
        cursor: bool,
    ) -> Result<Vec<u8>, vectrcap::app::error::AppError> {
        assert!(!cursor);
        let pixel = if self.chrome_visible.get() {
            [200, 0, 0, 255]
        } else if self.purpose == Some(SourcePurpose::Utility) {
            [20, 40, 60, 255]
        } else {
            [1, 2, 3, 255]
        };
        Ok(pixel.repeat(4))
    }
}
struct Chrome {
    visible: std::rc::Rc<std::cell::Cell<bool>>,
    fail: bool,
}
impl Countdown for Chrome {
    fn wait(&mut self, _: u8, _: &CancellationToken) -> Result<(), vectrcap::app::error::AppError> {
        Ok(())
    }
    fn hide_chrome(&mut self) -> Result<(), vectrcap::app::error::AppError> {
        if self.fail {
            return Err(vectrcap::app::error::AppError::Capture(
                "synthetic chrome hiding failure",
            ));
        }
        self.visible.set(false);
        Ok(())
    }
}
#[test]
fn visible_pin_can_be_sampled_without_sampling_tool_chrome() {
    let visible = std::rc::Rc::new(std::cell::Cell::new(true));
    let mut service = CaptureService::new(
        Source {
            purpose: None,
            chrome_visible: visible.clone(),
        },
        Chrome {
            visible,
            fail: false,
        },
        CancellationToken::new(),
    );
    let s = service.utility_snapshot().unwrap();
    assert_eq!(s.pixels(), [20, 40, 60, 255].repeat(4));
    assert_eq!(s.launch_pointer(), Some(Point { x: 0, y: 0 }));
    assert_eq!(service.into_source().purpose, Some(SourcePurpose::Utility));
}
#[test]
fn other_active_acquisition_rejects_tool_start() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(dir.path());
    c.dispatch(AppCommand::StartCapture {
        mode: vectrcap::capture::request::CaptureMode::Region,
        selection: Default::default(),
    });
    assert!(matches!(
        c.dispatch(AppCommand::StartUtility(UtilityKind::Ruler))
            .as_slice(),
        [AppEffect::Error(_)]
    ));
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(dir.path());
    let _id = start(&mut c);
    assert!(matches!(
        c.dispatch(AppCommand::StartCapture {
            mode: vectrcap::capture::request::CaptureMode::Region,
            selection: Default::default()
        })
        .as_slice(),
        [AppEffect::Error(_)]
    ));
    assert!(matches!(
        c.dispatch(AppCommand::StartUtility(UtilityKind::Ruler))
            .as_slice(),
        [AppEffect::Error(_)]
    ));
}
#[test]
fn cancel_leaves_clipboard_unchanged() {
    let mut s = UtilitySession::new(UtilityKind::Sampler, snapshot(), Point { x: 0, y: 0 });
    assert_eq!(s.handle(UtilityInput::Close), vec![UtilityEffect::Close]);
    assert!(s.snapshot().is_none());
}
#[test]
fn display_change_releases_snapshot() {
    for input in [
        UtilityInput::DisplayChanged,
        UtilityInput::Locked,
        UtilityInput::Close,
    ] {
        let mut s = UtilitySession::new(UtilityKind::Ruler, snapshot(), Point { x: 0, y: 0 });
        assert_eq!(s.handle(input), vec![UtilityEffect::Close]);
        assert!(s.snapshot().is_none());
    }
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(dir.path());
    let id = start(&mut c);
    c.on_event(AppEvent::UtilityAcquired {
        session: id,
        result: Ok(snapshot()),
    });
    let surface = c.utility_surface().unwrap();
    c.dispatch(AppCommand::CloseWindow);
    assert!(c.utility_surface().is_none());
    assert!(surface.lock().unwrap().session.snapshot().is_none());
}
#[test]
fn remapped_utility_keys_respect_focus_scope() {
    let mut s = ShortcutSettings::default();
    let binding = s
        .bindings
        .iter_mut()
        .find(|b| b.action == ShortcutAction::SamplerSelect)
        .unwrap();
    let chord = KeyChord {
        ctrl: false,
        alt: false,
        shift: false,
        win: false,
        key: KeyCode::from_vk(0x50).unwrap(),
    };
    binding.chord = Some(chord);
    s.validate().unwrap();
    assert_eq!(
        s.local_action_for(
            ShortcutScope::ScreenSampler,
            chord,
            ShortcutFocus::Normal,
            false
        ),
        Some(ShortcutAction::SamplerSelect)
    );
    assert_eq!(
        s.local_action_for(ShortcutScope::Ruler, chord, ShortcutFocus::Normal, false),
        None
    );
    assert_eq!(
        s.local_action_for(
            ShortcutScope::ScreenSampler,
            chord,
            ShortcutFocus::TextEntry,
            false
        ),
        None
    );
    assert_eq!(
        s.local_action_for(
            ShortcutScope::ScreenSampler,
            chord,
            ShortcutFocus::Normal,
            true
        ),
        None
    );
}

#[test]
fn canceled_acquisition_keeps_reservation_until_worker_settles() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(dir.path());
    let id = start(&mut c);
    c.dispatch(AppCommand::CloseWindow);
    assert!(matches!(
        c.dispatch(AppCommand::StartUtility(UtilityKind::Ruler))
            .as_slice(),
        [AppEffect::Error(_)]
    ));
    let out = c.on_event(AppEvent::UtilityAcquired {
        session: id,
        result: Ok(snapshot()),
    });
    assert!(matches!(out.as_slice(), [AppEffect::UtilitySurface(None)]));
    assert!(c.utility_surface().is_none());
    start(&mut c);
}

#[test]
fn pick_again_reacquires_and_rejects_stale_viewport_input() {
    use vectrcap::tools::color_result::ColorResultInput;
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(dir.path());
    let id = start(&mut c);
    c.on_event(AppEvent::UtilityAcquired {
        session: id,
        result: Ok(snapshot()),
    });
    c.dispatch(AppCommand::Utility {
        session: id,
        input: UtilityInput::Select,
    });
    let old = c.utility_surface().unwrap();
    let out = c.dispatch(AppCommand::Utility {
        session: id,
        input: UtilityInput::Result(ColorResultInput::PickAgain),
    });
    let fresh = out
        .iter()
        .find_map(|e| {
            if let AppEffect::AcquireUtility { session } = e {
                Some(*session)
            } else {
                None
            }
        })
        .unwrap();
    assert_ne!(id, fresh);
    assert!(old.lock().unwrap().session.snapshot().is_none());
    assert!(
        c.dispatch(AppCommand::Utility {
            session: id,
            input: UtilityInput::Close
        })
        .is_empty()
    );
    c.on_event(AppEvent::UtilityAcquired {
        session: fresh,
        result: Ok(snapshot()),
    });
    assert!(
        c.utility_surface()
            .unwrap()
            .lock()
            .unwrap()
            .session
            .snapshot()
            .is_some()
    );
}

#[test]
fn held_global_utility_hotkey_does_not_repeat() {
    struct Registrar;
    impl ShortcutRegistrar for Registrar {
        fn register(
            &self,
            _: RegistrationId,
            _: KeyChord,
        ) -> Result<(), vectrcap::app::error::AppError> {
            Ok(())
        }
        fn unregister(&self, _: RegistrationId) {}
    }
    let mut binding = ShortcutBinding::unassigned(ShortcutAction::ScreenColorPicker);
    binding.enabled = true;
    binding.chord = Some(KeyChord {
        ctrl: true,
        alt: true,
        shift: false,
        win: false,
        key: KeyCode::from_vk(0x50).unwrap(),
    });
    let (manager, failures) = ShortcutManager::new(Box::new(Registrar), vec![binding]);
    assert!(failures.is_empty());
    assert!(matches!(
        manager.handle(
            ShortcutEvent {
                action: ShortcutAction::ScreenColorPicker,
                repeat: false
            },
            ShortcutFocus::Normal
        ),
        Some(ShortcutOutcome::Command(AppCommand::StartUtility(
            UtilityKind::Sampler
        )))
    ));
    assert!(
        manager
            .handle(
                ShortcutEvent {
                    action: ShortcutAction::ScreenColorPicker,
                    repeat: true
                },
                ShortcutFocus::Normal
            )
            .is_none()
    );
}

#[test]
fn canceled_screenshot_still_owns_acquisition_until_worker_settles() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(dir.path());
    let out = c.dispatch(AppCommand::StartCapture {
        mode: vectrcap::capture::request::CaptureMode::Region,
        selection: Default::default(),
    });
    let id = out
        .iter()
        .find_map(|e| {
            if let AppEffect::AcquireCapture { session, .. } = e {
                Some(*session)
            } else {
                None
            }
        })
        .unwrap();
    c.dispatch(AppCommand::CancelActiveCapture);
    assert!(matches!(
        c.dispatch(AppCommand::StartUtility(UtilityKind::Sampler))
            .as_slice(),
        [AppEffect::Error(_)]
    ));
    c.on_event(AppEvent::CaptureAcquired {
        session: id,
        result: Err(vectrcap::app::error::AppError::Cancelled),
    });
    start(&mut c);
}

#[test]
fn utility_settings_reject_destination_routing() {
    let mut settings = vectrcap::persistence::settings::Settings::default();
    let id = settings
        .destinations
        .add(
            "fixture",
            vectrcap::persistence::settings::ServerOrigin::parse("https://utility.test").unwrap(),
            None,
        )
        .unwrap();
    settings
        .shortcuts
        .bindings
        .iter_mut()
        .find(|b| b.action == ShortcutAction::Ruler)
        .unwrap()
        .destination = vectrcap::app::destinations::DestinationSelection::Specific(id);
    assert!(settings.validate().is_err());
}

#[test]
fn copy_failure_lock_and_close_to_tray_preserve_pending_upload_and_pin_ownership() {
    use std::sync::{Arc, Mutex};
    use vectrcap::{
        app::{controller::AppController, destinations::*, error::AppError},
        persistence::{artifacts::ArtifactStore, history::History, settings::ServerOrigin},
        pin::{
            PinnedImage,
            source::{LoadedPin, PinSource},
        },
        platform::windows::clipboard::ClipboardWriter,
        tools::color_result::{ColorResultInput, CopyFeedback},
    };
    #[derive(Default)]
    struct Clipboard {
        fail: Mutex<bool>,
        values: Mutex<Vec<String>>,
    }
    impl ClipboardWriter for Clipboard {
        fn write_text(&self, text: &str) -> Result<(), AppError> {
            if *self.fail.lock().unwrap() {
                return Err(AppError::Persistence("synthetic clipboard busy"));
            }
            self.values.lock().unwrap().push(text.into());
            Ok(())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let history = Arc::new(History::open(&dir.path().join("history.db")).unwrap());
    let artifacts = Arc::new(ArtifactStore::new(dir.path().join("spool")).unwrap());
    let repo = Arc::new(task11_support::Repo::default());
    {
        let mut settings = repo.settings.lock().unwrap();
        let id = settings
            .destinations
            .add(
                "synthetic",
                ServerOrigin::parse("https://utility.test").unwrap(),
                None,
            )
            .unwrap();
        settings
            .destinations
            .bind_credential(id, &Default::default())
            .unwrap();
    }
    let dest = resolve_destination(
        Default::default(),
        Default::default(),
        &repo.settings.lock().unwrap().destinations,
    )
    .unwrap();
    let input = dir.path().join("pending.txt");
    std::fs::write(&input, b"unchanged pending upload").unwrap();
    let artifact = artifacts
        .stage_file(&input, vectrcap::app::types::ArtifactKind::File)
        .unwrap();
    let job = history.create_job(&artifact, &dest).unwrap();
    let before = history.job(job.id).unwrap().unwrap();
    let clipboard = Arc::new(Clipboard::default());
    let mut c = AppController::open(
        Box::new(task11_support::RepoAdapter(repo)),
        history.clone(),
        Arc::new(task11_support::Store::default()),
        artifacts,
        Arc::new(task11_support::NoShell),
        clipboard.clone(),
    )
    .unwrap();
    let image = Arc::new(image::RgbaImage::from_pixel(
        2,
        2,
        image::Rgba([20, 40, 60, 255]),
    ));
    let load = c
        .dispatch(AppCommand::PinSource(PinSource::Clipboard))
        .iter()
        .find_map(|e| {
            if let AppEffect::LoadPin { request, .. } = e {
                Some(*request)
            } else {
                None
            }
        })
        .unwrap();
    c.on_event(AppEvent::PinLoaded {
        request: load,
        result: Ok(LoadedPin {
            image: PinnedImage::new(image.clone(), None).unwrap(),
            work_areas: vec![Rect::new(0, 0, 1000, 1000).unwrap()],
            pointer: Point { x: 0, y: 0 },
        }),
    });
    let pin = c.pins().snapshots().pop().unwrap();
    let id = start(&mut c);
    c.on_event(AppEvent::UtilityAcquired {
        session: id,
        result: Ok(snapshot()),
    });
    assert!(
        c.dispatch(AppCommand::Utility {
            session: id,
            input: UtilityInput::Select
        })
        .iter()
        .all(|e| matches!(e, AppEffect::UtilitySurface(_)))
    );
    assert!(clipboard.values.lock().unwrap().is_empty());
    *clipboard.fail.lock().unwrap() = true;
    c.dispatch(AppCommand::Utility {
        session: id,
        input: UtilityInput::Result(ColorResultInput::CopyHex),
    });
    assert!(matches!(
        c.utility_surface()
            .unwrap()
            .lock()
            .unwrap()
            .session
            .result
            .as_ref()
            .unwrap()
            .feedback(),
        CopyFeedback::Error(_)
    ));
    *clipboard.fail.lock().unwrap() = false;
    c.dispatch(AppCommand::Utility {
        session: id,
        input: UtilityInput::Result(ColorResultInput::CopyHex),
    });
    assert_eq!(*clipboard.values.lock().unwrap(), ["#14283C"]);
    c.dispatch(AppCommand::InterruptUtility);
    let id = start(&mut c);
    c.on_event(AppEvent::UtilityAcquired {
        session: id,
        result: Ok(snapshot()),
    });
    c.dispatch(AppCommand::CloseWindow);
    let after = history.job(job.id).unwrap().unwrap();
    assert_eq!(after.status, before.status);
    assert_eq!(after.artifact, before.artifact);
    assert_eq!(after.destination, before.destination);
    let after_pin = c.pins().snapshots().pop().unwrap();
    assert_eq!(after_pin.id, pin.id);
    assert_eq!(after_pin.visible, pin.visible);
    assert!(Arc::ptr_eq(after_pin.image.pixels(), &image));
    assert_eq!(*clipboard.values.lock().unwrap(), ["#14283C"]);
}

#[test]
fn chrome_hide_failure_and_cancellation_never_publish_a_snapshot() {
    let visible = std::rc::Rc::new(std::cell::Cell::new(true));
    let mut service = CaptureService::new(
        Source {
            purpose: None,
            chrome_visible: visible.clone(),
        },
        Chrome {
            visible: visible.clone(),
            fail: true,
        },
        CancellationToken::new(),
    );
    assert!(matches!(
        service.utility_snapshot(),
        Err(vectrcap::app::error::AppError::Capture(
            "synthetic chrome hiding failure"
        ))
    ));
    assert!(visible.get());
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut service = CaptureService::new(
        Source {
            purpose: None,
            chrome_visible: visible.clone(),
        },
        Chrome {
            visible: visible.clone(),
            fail: false,
        },
        cancel,
    );
    assert!(matches!(
        service.utility_snapshot(),
        Err(vectrcap::app::error::AppError::Cancelled)
    ));
    assert!(visible.get());
}
