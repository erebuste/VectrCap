use image::{Rgba, RgbaImage};
use std::{io::Write, sync::Arc};
use vectrcap::{
    app::types::{Artifact, ArtifactId, ArtifactKind},
    capture::geometry::{PhysicalPoint, PhysicalRect},
    pin::{
        PinEffect, PinManager, PinnedImage,
        source::{load_pin_dib, load_pin_file, load_pin_history},
    },
};

#[test]
fn file_and_clipboard_dimensions_checked_before_allocation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image.bmp");
    let mut dib = vec![0u8; 40];
    dib[0..4].copy_from_slice(&40u32.to_le_bytes());
    dib[4..8].copy_from_slice(&8001i32.to_le_bytes());
    dib[8..12].copy_from_slice(&8000i32.to_le_bytes());
    dib[12..14].copy_from_slice(&1u16.to_le_bytes());
    dib[14..16].copy_from_slice(&32u16.to_le_bytes());
    assert!(load_pin_dib(&dib).is_err());
    let mut bmp = b"BM".to_vec();
    bmp.extend_from_slice(&[0; 8]);
    bmp.extend_from_slice(&54u32.to_le_bytes());
    bmp.extend_from_slice(&dib);
    std::fs::write(&path, bmp).unwrap();
    assert!(load_pin_file(&path).is_err());
    for bytes in [
        b"not an image".as_slice(),
        b"<svg/>".as_slice(),
        b"\0\0\0\x18ftypmp42".as_slice(),
        b"\x89PNG\r\n\x1a\n".as_slice(),
    ] {
        std::fs::write(&path, bytes).unwrap();
        assert!(load_pin_file(&path).is_err());
        assert!(load_pin_dib(bytes).is_err());
    }
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(vectrcap::editor::export::MAX_ENCODED_BYTES as u64 + 1)
        .unwrap();
    drop(file);
    assert!(load_pin_file(&path).is_err());
    assert!(load_pin_file(std::path::Path::new(r"\\server\share\image.png")).is_err());
}
#[test]
fn gif_pin_uses_labeled_first_frame() {
    let mut file = tempfile::Builder::new().suffix(".gif").tempfile().unwrap();
    {
        let mut encoder = gif::Encoder::new(&mut file, 2, 1, &[255, 0, 0, 0, 0, 255]).unwrap();
        for index in [0, 1] {
            let frame = gif::Frame {
                width: 2,
                height: 1,
                buffer: vec![index; 2].into(),
                ..Default::default()
            };
            encoder.write_frame(&frame).unwrap();
        }
    }
    file.flush().unwrap();
    // File handles used by ingress deliberately exclude writers.
    let (_file, path) = file.keep().unwrap();
    drop(_file);
    let image = load_pin_file(&path).unwrap();
    assert_eq!(image.pixels().get_pixel(0, 0).0, [255, 0, 0, 255]);
    assert!(image.source_label().unwrap().contains("first frame"));
    std::fs::remove_file(path).unwrap();
}
fn artifact(path: std::path::PathBuf, bytes: &[u8]) -> Artifact {
    use sha2::{Digest, Sha256};
    Artifact {
        id: ArtifactId::new_random(),
        path,
        display_name: "fixture.png".into(),
        kind: ArtifactKind::Screenshot,
        size: bytes.len() as u64,
        sha256: Sha256::digest(bytes).into(),
    }
}
#[test]
fn missing_history_image_never_fetches_url() {
    let dir = tempfile::tempdir().unwrap();
    let item = artifact(dir.path().join("missing.png"), b"missing");
    assert!(load_pin_history(&item).is_err());
    // Ingress accepts only a local Artifact, no URL or network client.
    std::fs::write(&item.path, b"changed").unwrap();
    assert!(load_pin_history(&item).is_err());
}
fn manager() -> PinManager {
    let mut m = PinManager::default();
    m.update_work_areas(
        vec![PhysicalRect::new(0, 0, 1920, 1080).unwrap()],
        PhysicalPoint { x: 200, y: 100 },
    )
    .unwrap();
    m
}
#[test]
fn pinning_existing_image_creates_no_archive_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("existing.png");
    RgbaImage::from_pixel(20, 10, Rgba([10, 20, 30, 40]))
        .save(&path)
        .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let item = artifact(path.clone(), &bytes);
    let mut m = manager();
    m.open(load_pin_history(&item).unwrap(), None).unwrap();
    m.open(load_pin_file(&path).unwrap(), None).unwrap();
    assert_eq!(m.close_all().len(), 2);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}
#[test]
fn new_capture_pin_follows_archive_policy() {
    use vectrcap::{
        app::image_finish::ImageFinishTransaction,
        editor::{Annotation, EditorCommand, EditorDocument},
        persistence::{
            artifacts::ArtifactStore,
            history::History,
            settings::{ArchivePolicy, StorageMode},
        },
        ui::{capture_overlay::OverlayAction, image_export::ImageExportPane},
    };
    for mode in [
        StorageMode::AlwaysSave,
        StorageMode::PendingOrFailed,
        StorageMode::NeverAutomatic,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(&dir.path().join("history.db")).unwrap());
        let artifacts = ArtifactStore::new(dir.path().join("spool")).unwrap();
        let mut doc = EditorDocument::from_image(Arc::new(RgbaImage::from_pixel(
            20,
            10,
            Rgba([200, 30, 40, 255]),
        )))
        .unwrap();
        doc.apply(EditorCommand::Insert(
            Annotation::redaction(PhysicalRect::new(0, 0, 10, 10).unwrap(), [1, 2, 3]).unwrap(),
        ))
        .unwrap();
        let mut pane = ImageExportPane::default();
        let (tx, rx) = std::sync::mpsc::channel();
        pane.update(&doc, move || {
            tx.send(()).unwrap();
        });
        rx.recv_timeout(std::time::Duration::from_secs(15)).unwrap();
        pane.update(&doc, || {});
        let finish = pane.confirm(&doc, OverlayAction::Pin).unwrap();
        let pixels = finish.prepared.pixels().clone();
        let mut transaction = ImageFinishTransaction::new(finish).unwrap();
        let policy = ArchivePolicy {
            storage_mode: mode,
            archive_root: Some(dir.path().join("archive")),
            ..Default::default()
        };
        transaction
            .persist(
                &artifacts,
                history.clone(),
                &policy,
                &Default::default(),
                None,
                Default::default(),
            )
            .unwrap();
        let image = PinnedImage::new(pixels.clone(), None).unwrap();
        let mut m = manager();
        let id = m.open(image, None).unwrap();
        assert_eq!(
            m.get(id).unwrap().image.pixels().get_pixel(0, 0).0,
            [1, 2, 3, 255]
        );
        assert!(Arc::ptr_eq(m.get(id).unwrap().image.pixels(), &pixels));
        assert_eq!(
            transaction.archive().is_some(),
            mode == StorageMode::AlwaysSave
        );
        transaction.discard_unadmitted(&artifacts).unwrap();
        assert!(history.shell_jobs().unwrap().is_empty());
        assert_eq!(
            history.browse(&Default::default()).unwrap().records.len(),
            usize::from(mode == StorageMode::AlwaysSave)
        );
    }
}
#[test]
fn pin_windows_excluded_from_hover_and_capture() {
    use vectrcap::capture::windows::{WindowCandidate, WindowId, WindowState, eligible_windows};
    let candidate = WindowCandidate {
        id: WindowId::new(1).unwrap(),
        bounds: PhysicalRect::new(0, 0, 20, 10).unwrap(),
    };
    assert!(
        eligible_windows([WindowState {
            candidate,
            visible: true,
            minimized: false,
            cloaked: false,
            owned_by_app: true
        }])
        .is_empty()
    );
    let mut m = manager();
    let id = m
        .open(
            PinnedImage::new(Arc::new(RgbaImage::new(20, 10)), None).unwrap(),
            None,
        )
        .unwrap();
    assert!(
        matches!(m.suspend_for_capture().as_slice(), [PinEffect::UpdateViewport(pin)] if !pin.visible)
    );
    assert!(!m.get(id).unwrap().visible);
    m.suspend_for_capture();
    m.restore_after_capture();
    assert!(!m.get(id).unwrap().visible);
    assert!(
        matches!(m.restore_after_capture().as_slice(), [PinEffect::UpdateViewport(pin)] if pin.visible)
    );
}

struct NoCredentials;
impl vectrcap::platform::CredentialStore for NoCredentials {
    fn read(
        &self,
        _: &vectrcap::app::destinations::CredentialKey,
    ) -> Result<vectrcap::vectr::client::UploadToken, vectrcap::app::error::AppError> {
        panic!("pins never read credentials")
    }
    fn write(
        &self,
        _: &vectrcap::app::destinations::CredentialKey,
        _: &vectrcap::vectr::client::UploadToken,
    ) -> Result<(), vectrcap::app::error::AppError> {
        panic!("pins never write credentials")
    }
    fn delete(
        &self,
        _: &vectrcap::app::destinations::CredentialKey,
    ) -> Result<(), vectrcap::app::error::AppError> {
        panic!("pins never delete credentials")
    }
}
struct NoShell;
impl vectrcap::platform::windows::shell_actions::ShellActions for NoShell {
    fn open_path(&self, _: &std::path::Path) -> Result<(), vectrcap::app::error::AppError> {
        panic!("unexpected shell action")
    }
    fn reveal_path(&self, _: &std::path::Path) -> Result<(), vectrcap::app::error::AppError> {
        panic!("unexpected shell action")
    }
    fn open_url(&self, _: &str) -> Result<(), vectrcap::app::error::AppError> {
        panic!("pins never open URLs")
    }
}
#[derive(Default)]
struct Clipboard(std::sync::Mutex<Vec<image::RgbaImage>>);
impl vectrcap::platform::windows::clipboard::ClipboardWriter for Clipboard {
    fn write_text(&self, _: &str) -> Result<(), vectrcap::app::error::AppError> {
        panic!("image pins never copy text")
    }
    fn write_image(&self, image: &image::RgbaImage) -> Result<(), vectrcap::app::error::AppError> {
        self.0.lock().unwrap().push(image.clone());
        Ok(())
    }
}
fn controller(
    clipboard: Arc<Clipboard>,
) -> (tempfile::TempDir, vectrcap::app::controller::AppController) {
    use vectrcap::{
        app::{controller::AppController, destination_manager::FileDestinationRepository},
        persistence::{artifacts::ArtifactStore, history::History},
    };
    let dir = tempfile::tempdir().unwrap();
    let app = AppController::open(
        Box::new(FileDestinationRepository::open(&dir.path().join("settings.json")).unwrap()),
        Arc::new(History::open(&dir.path().join("history.db")).unwrap()),
        Arc::new(NoCredentials),
        Arc::new(ArtifactStore::new(dir.path().join("spool")).unwrap()),
        Arc::new(NoShell),
        clipboard,
    )
    .unwrap();
    (dir, app)
}
#[test]
fn controller_pin_lifecycle_is_typed_bounded_and_rejects_stale_loads() {
    use vectrcap::{
        app::events::{AppCommand, AppEffect, AppEvent},
        pin::{
            PinInput,
            source::{LoadedPin, PinSource},
        },
    };
    let clipboard = Arc::new(Clipboard::default());
    let (_dir, mut app) = controller(clipboard.clone());
    let image = PinnedImage::new(
        Arc::new(RgbaImage::from_pixel(20, 10, Rgba([1, 2, 3, 40]))),
        None,
    )
    .unwrap();
    for _ in 0..16 {
        let effects = app.dispatch(AppCommand::PinSource(PinSource::Clipboard));
        let request = effects
            .iter()
            .find_map(|e| {
                if let AppEffect::LoadPin { request, .. } = e {
                    Some(*request)
                } else {
                    None
                }
            })
            .unwrap();
        assert!(
            app.dispatch(AppCommand::PinSource(PinSource::Clipboard))
                .iter()
                .any(|e| matches!(e, AppEffect::Error(_)))
        );
        let effects = app.on_event(AppEvent::PinLoaded {
            request,
            result: Ok(LoadedPin {
                image: image.clone(),
                work_areas: vec![PhysicalRect::new(-100, -100, 1000, 800).unwrap()],
                pointer: PhysicalPoint { x: 0, y: 0 },
            }),
        });
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, AppEffect::Pin(PinEffect::UpdateViewport(_))))
        );
    }
    let pins = app.pins().snapshots();
    let id = pins[0].id;
    let effects = app.dispatch(AppCommand::Pin {
        id,
        input: PinInput::Copy,
    });
    assert!(!effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
    assert_eq!(
        clipboard.0.lock().unwrap().as_slice(),
        &[image.pixels().as_ref().clone()]
    );
    let effects = app.dispatch(AppCommand::PinSource(PinSource::Clipboard));
    assert!(effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
    assert_eq!(app.pins().snapshots().len(), 16);
    assert_eq!(
        app.dispatch(AppCommand::CloseAllPins)
            .iter()
            .filter(|e| matches!(e, AppEffect::Pin(PinEffect::CloseViewport(_))))
            .count(),
        16
    );
    let request = app
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
    app.dispatch(AppCommand::CloseAllPins);
    assert!(
        app.on_event(AppEvent::PinLoaded {
            request,
            result: Ok(LoadedPin {
                image,
                work_areas: vec![PhysicalRect::new(0, 0, 1000, 800).unwrap()],
                pointer: PhysicalPoint { x: 0, y: 0 }
            })
        })
        .is_empty()
    );
    assert!(app.pins().snapshots().is_empty());
}
#[test]
fn clipboard_dib_roundtrip_retains_alpha_and_orientation() {
    let pixels = RgbaImage::from_fn(3, 2, |x, y| Rgba([x as u8 * 30, y as u8 * 90, 20, 40]));
    let bytes = vectrcap::platform::windows::clipboard::dibv5_bytes(&pixels).unwrap();
    assert_eq!(load_pin_dib(&bytes).unwrap().pixels().as_ref(), &pixels);
}

#[test]
fn pin_shortcuts_are_available_without_reserved_defaults() {
    use vectrcap::app::shortcuts::{ShortcutAction, ShortcutBinding};
    for action in [
        ShortcutAction::PinSourceChooser,
        ShortcutAction::PinFromScreen,
        ShortcutAction::PinFromClipboard,
        ShortcutAction::PinFromFile,
        ShortcutAction::CloseAllPins,
    ] {
        assert!(action.is_available_now());
        assert!(ShortcutBinding::default_for(action).chord.is_none());
    }
}

#[test]
fn native_capture_hiding_is_synchronous_nested_and_rolls_back_failure() {
    use vectrcap::{
        app::error::AppError,
        pin::manager::{PinWindowRegistry, PinWindowSystem},
    };
    #[derive(Default)]
    struct Fake {
        visible: std::collections::HashMap<usize, bool>,
        fail_hide: Option<usize>,
        flushes: usize,
    }
    struct Adapter(Arc<std::sync::Mutex<Fake>>);
    impl PinWindowSystem for Adapter {
        fn is_owned(&self, _: vectrcap::pin::PinId, handle: usize) -> bool {
            self.0.lock().unwrap().visible.contains_key(&handle)
        }
        fn exclude_from_capture(&mut self, _: usize) -> bool {
            false
        }
        fn is_visible(&self, handle: usize) -> bool {
            self.0.lock().unwrap().visible[&handle]
        }
        fn set_visible(&mut self, handle: usize, visible: bool) -> Result<(), AppError> {
            let mut s = self.0.lock().unwrap();
            if !visible && s.fail_hide == Some(handle) {
                return Err(AppError::Capture("injected hide failure"));
            }
            s.visible.insert(handle, visible);
            Ok(())
        }
        fn flush(&mut self) -> Result<(), AppError> {
            self.0.lock().unwrap().flushes += 1;
            Ok(())
        }
    }
    let os = Arc::new(std::sync::Mutex::new(Fake::default()));
    os.lock().unwrap().visible.insert(1, true);
    os.lock().unwrap().visible.insert(2, true);
    let mut m = manager();
    let image = PinnedImage::new(Arc::new(RgbaImage::new(10, 10)), None).unwrap();
    let a = m.open(image.clone(), None).unwrap();
    let b = m.open(image, None).unwrap();
    let mut registry = PinWindowRegistry::new(Adapter(os.clone()));
    assert!(!registry.register(a, 1).unwrap()); // unsupported affinity uses hiding
    registry.register(b, 2).unwrap();
    registry.suspend_for_capture().unwrap();
    assert!(os.lock().unwrap().visible.values().all(|v| !*v));
    assert_eq!(os.lock().unwrap().flushes, 1);
    registry.suspend_for_capture().unwrap();
    registry.set_visible(a, true).unwrap();
    registry.restore_after_capture().unwrap();
    assert!(os.lock().unwrap().visible.values().all(|v| !*v));
    registry.restore_after_capture().unwrap();
    assert!(os.lock().unwrap().visible.values().all(|v| *v));
    os.lock().unwrap().fail_hide = Some(2);
    assert!(registry.suspend_for_capture().is_err());
    assert!(os.lock().unwrap().visible.values().all(|v| *v));
    assert!(!registry.suspended());
    registry.unregister(a);
    registry.unregister(b);
    assert!(!registry.register(a, 999).is_ok());
}

#[test]
fn controller_capture_suspends_and_restores_pins_on_failure_or_cancel() {
    use vectrcap::{
        app::{
            error::AppError,
            events::{AppCommand, AppEffect, AppEvent},
        },
        pin::source::{LoadedPin, PinSource},
    };
    let (_dir, mut app) = controller(Arc::new(Clipboard::default()));
    let request = app
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
    app.on_event(AppEvent::PinLoaded {
        request,
        result: Ok(LoadedPin {
            image: PinnedImage::new(Arc::new(RgbaImage::new(10, 10)), None).unwrap(),
            work_areas: vec![PhysicalRect::new(0, 0, 1000, 800).unwrap()],
            pointer: PhysicalPoint { x: 0, y: 0 },
        }),
    });
    for cancel in [false, true] {
        let effects = app.dispatch(AppCommand::StartCapture {
            mode: vectrcap::capture::request::CaptureMode::Region,
            selection: Default::default(),
        });
        let session = effects
            .iter()
            .find_map(|e| {
                if let AppEffect::AcquireCapture { session, .. } = e {
                    Some(*session)
                } else {
                    None
                }
            })
            .unwrap();
        assert!(!app.pins().snapshots()[0].visible);
        if cancel {
            app.dispatch(AppCommand::CancelCapture(session));
        } else {
            app.on_event(AppEvent::CaptureAcquired {
                session,
                result: Err(AppError::Capture("injected acquisition failure")),
            });
        }
        assert!(app.pins().snapshots()[0].visible);
    }
    let remaining = PhysicalRect::new(-2000, -100, 1000, 800).unwrap();
    app.on_event(AppEvent::PinWorkAreas {
        result: Ok(vectrcap::pin::source::PinWorkAreas {
            work_areas: vec![remaining],
            pointer: PhysicalPoint { x: -1500, y: 100 },
        }),
    });
    assert!(remaining.contains(app.pins().snapshots()[0].state.position()));
}

#[test]
fn pin_source_errors_offer_visible_retry_without_destination_setup() {
    use vectrcap::{
        app::{
            error::AppError,
            events::{AppCommand, AppEffect, AppEvent},
        },
        pin::source::PinSource,
    };
    let (_dir, mut app) = controller(Arc::new(Clipboard::default()));
    let request = app
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
    let effects = app.on_event(AppEvent::PinLoaded {
        request,
        result: Err(AppError::InvalidInput(
            "clipboard contains text, not an image",
        )),
    });
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, AppEffect::PinSourceChooser(true)))
    );
    assert!(effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
}
