use image::{Rgba, RgbaImage};
use std::sync::Arc;
use vectrcap::{
    capture::{
        geometry::{MonitorGeometry, PhysicalPoint, PhysicalRect},
        request::CaptureSnapshot,
    },
    editor::{Annotation, EditorCommand, EditorDocument, Style, render_export},
};

fn rect(x: i32, y: i32, w: u32, h: u32) -> PhysicalRect {
    PhysicalRect::new(x, y, w, h).unwrap()
}
fn document() -> EditorDocument {
    let bounds = rect(-20, -10, 100, 80);
    let pixels = RgbaImage::from_fn(100, 80, |x, y| Rgba([x as u8, y as u8, 100, 255]));
    EditorDocument::new(
        CaptureSnapshot::new(
            pixels.into_raw(),
            bounds,
            vec![MonitorGeometry::new("fixture".into(), bounds, 1.0).unwrap()],
            vec![],
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn image_local_annotations_export_at_correct_offsets() {
    let mut doc = EditorDocument::from_image(Arc::new(RgbaImage::from_pixel(
        80,
        400,
        Rgba([200, 150, 100, 255]),
    )))
    .unwrap();
    doc.apply(EditorCommand::Insert(
        Annotation::spotlight(rect(8, 290, 25, 40), 0.6).unwrap(),
    ))
    .unwrap();
    doc.apply(EditorCommand::Insert(
        Annotation::highlight(rect(10, 302, 10, 10), [255, 255, 0], 0.35).unwrap(),
    ))
    .unwrap();
    doc.apply(EditorCommand::Insert(
        Annotation::redaction(rect(15, 310, 7, 9), [4, 5, 6]).unwrap(),
    ))
    .unwrap();
    doc.apply(EditorCommand::Crop(Some(rect(10, 300, 30, 40))))
        .unwrap();
    assert_eq!(doc.screen_location(), None);
    let full = vectrcap::editor::render::render_preview(&doc).unwrap();
    let exported = render_export(&doc).unwrap();
    assert_eq!(exported.dimensions(), (30, 40));
    assert_eq!(exported.get_pixel(5, 10).0, [4, 5, 6, 255]);
    for y in 0..40 {
        for x in 0..30 {
            assert_eq!(full.get_pixel(x + 10, y + 300), exported.get_pixel(x, y));
        }
    }
    assert_eq!(full.get_pixel(0, 0).0, [80, 60, 40, 255]);
    assert_eq!(full.get_pixel(9, 299).0, [200, 150, 100, 255]);
}

#[test]
fn long_image_review_uses_bounded_tiles() {
    use eframe::egui::{Context, Pos2, Rect as UiRect, Vec2};
    use vectrcap::ui::image_review::TiledImage;
    let source = Arc::new(RgbaImage::from_pixel(37, 200_000, Rgba([20, 40, 60, 255])));
    let mut tiled = TiledImage::new(source).unwrap();
    let ctx = Context::default();
    let mut output = ctx.run_ui(Default::default(), |ui| {
        tiled
            .paint(
                ui.painter(),
                UiRect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 500.0)),
                (0.0, 0.0),
                0.0025,
            )
            .unwrap();
    });
    let sizes: Vec<_> = output
        .textures_delta
        .set
        .iter()
        .filter(|(id, _)| **id != eframe::egui::TextureId::Managed(0))
        .flat_map(|(_, deltas)| deltas.iter().map(|d| d.image.size()))
        .collect();
    output.textures_delta.clear();
    assert!(tiled.resident_tile_count() > 0 && tiled.resident_tile_count() <= 16);
    assert!(!sizes.is_empty());
    assert!(
        sizes.iter().all(|s| s.iter().all(|&axis| axis <= 1024)),
        "oversized tile {sizes:?}"
    );
    let mut output = ctx.run_ui(Default::default(), |ui| {
        tiled
            .paint(
                ui.painter(),
                UiRect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 500.0)),
                (0.0, 199_500.0),
                1.0,
            )
            .unwrap();
    });
    let sizes: Vec<_> = output
        .textures_delta
        .set
        .iter()
        .filter(|(id, _)| **id != eframe::egui::TextureId::Managed(0))
        .flat_map(|(_, deltas)| deltas.iter().map(|d| d.image.size()))
        .collect();
    output.textures_delta.clear();
    assert!(tiled.resident_tile_count() > 0 && tiled.resident_tile_count() <= 16);
    assert!(!sizes.is_empty());
    assert!(sizes.iter().all(|s| s.iter().all(|&axis| axis <= 1024)));
}
fn redaction() -> Annotation {
    Annotation::redaction(rect(0, 0, 12, 12), [7, 8, 9]).unwrap()
}

#[test]
fn annotate_before_select_equals_select_before_annotate() {
    let mut a = document();
    let mut b = document();
    assert!(a.crop().is_none());
    let mark = Annotation::rectangle(
        rect(-5, -5, 40, 30),
        Style::new([200, 0, 0, 255], 2.0, Some([0, 150, 0, 255])).unwrap(),
    )
    .unwrap();
    a.apply(EditorCommand::Insert(mark.clone())).unwrap();
    a.apply(EditorCommand::Crop(Some(rect(0, 0, 40, 30))))
        .unwrap();
    b.apply(EditorCommand::Crop(Some(rect(0, 0, 40, 30))))
        .unwrap();
    b.apply(EditorCommand::Insert(mark)).unwrap();
    assert_eq!(render_export(&a).unwrap(), render_export(&b).unwrap());
}
#[test]
fn crop_clips_without_moving_annotations() {
    let mut d = document();
    let mark = redaction();
    let id = mark.id();
    d.apply(EditorCommand::Insert(mark)).unwrap();
    d.apply(EditorCommand::Crop(Some(rect(5, 5, 25, 20))))
        .unwrap();
    let out = render_export(&d).unwrap();
    assert_eq!(out.dimensions(), (25, 20));
    assert_eq!(out.get_pixel(0, 0).0, [7, 8, 9, 255]);
    assert_eq!(out.get_pixel(7, 7).0, [32, 22, 100, 255]);
    assert_eq!(d.annotations()[0].id(), id);
    d.apply(EditorCommand::Crop(Some(rect(-10, -10, 40, 30))))
        .unwrap();
    assert_eq!(
        render_export(&d).unwrap().get_pixel(10, 10).0,
        [7, 8, 9, 255]
    );
}
#[test]
fn redaction_replaces_every_underlying_pixel() {
    let mut d = EditorDocument::from_image(Arc::new(RgbaImage::from_pixel(
        20,
        20,
        Rgba([210, 150, 70, 0]),
    )))
    .unwrap();
    d.apply(EditorCommand::Insert(redaction())).unwrap();
    let out = render_export(&d).unwrap();
    for y in 0..12 {
        for x in 0..12 {
            assert_eq!(out.get_pixel(x, y).0, [7, 8, 9, 255]);
        }
    }
}
#[test]
fn unicode_text_and_freehand_export() {
    let mut d =
        EditorDocument::from_image(Arc::new(RgbaImage::from_pixel(200, 80, Rgba([255; 4]))))
            .unwrap();
    d.apply(EditorCommand::Insert(
        Annotation::text(
            PhysicalPoint { x: 4, y: 3 },
            "HéλЖ!".into(),
            24.0,
            [0, 0, 0, 255],
        )
        .unwrap(),
    ))
    .unwrap();
    d.apply(EditorCommand::Insert(
        Annotation::freehand(
            vec![
                PhysicalPoint { x: 5, y: 60 },
                PhysicalPoint { x: 50, y: 60 },
            ]
            .into(),
            [250, 0, 0, 255],
            4.0,
        )
        .unwrap(),
    ))
    .unwrap();
    let out = render_export(&d).unwrap();
    assert!(out.enumerate_pixels().any(|(_, y, p)| y < 40 && p[0] < 100));
    assert_eq!(out.get_pixel(25, 60).0, [250, 0, 0, 255]);
    // Unsupported scalars must fail visibly instead of silently exporting empty glyphs.
    let missing = Annotation::text(
        PhysicalPoint { x: 0, y: 0 },
        "\u{10ffff}".into(),
        20.0,
        [0, 0, 0, 255],
    );
    assert!(missing.is_err());
}
#[test]
fn undo_redo_restores_document() {
    let mut d = document();
    d.apply(EditorCommand::Crop(Some(rect(0, 0, 20, 20))))
        .unwrap();
    let before = render_export(&d).unwrap();
    d.apply(EditorCommand::Insert(redaction())).unwrap();
    let after = render_export(&d).unwrap();
    let revision = d.revision();
    assert_ne!(before, after);
    assert!(d.undo().unwrap());
    assert_eq!(render_export(&d).unwrap(), before);
    assert!(d.revision() > revision);
    assert!(d.redo().unwrap());
    assert_eq!(render_export(&d).unwrap(), after);
    let mark = d.annotations()[0].clone();
    d.apply(EditorCommand::Erase(vec![mark.id()])).unwrap();
    assert!(d.annotations().is_empty());
    d.undo().unwrap();
    assert_eq!(d.annotations()[0].id(), mark.id());
}
#[test]
fn toolbar_pixels_are_absent() {
    let mut d = document();
    d.apply(EditorCommand::Crop(Some(rect(-20, -10, 100, 80))))
        .unwrap();
    let overlay = vectrcap::ui::capture_overlay::CaptureOverlay::new(d);
    assert_eq!(
        render_export(overlay.document()).unwrap().get_pixel(0, 0).0,
        [0, 0, 100, 255]
    );
    assert!(
        render_export(overlay.document())
            .unwrap()
            .pixels()
            .all(|p| *p != Rgba([255, 0, 255, 255]))
    );
}

// These fixtures pass through the real finish pane confirmation boundary before
// storage, rather than constructing an independently encoded export in the test.
fn image_finish(
    doc: &EditorDocument,
    action: vectrcap::ui::capture_overlay::OverlayAction,
    options: vectrcap::editor::export::ImageExportOptions,
) -> vectrcap::ui::image_export::ImageFinish {
    let mut pane = vectrcap::ui::image_export::ImageExportPane::default();
    *pane.options_mut() = options;
    let (tx, rx) = std::sync::mpsc::channel();
    pane.update(doc, move || {
        tx.send(()).unwrap();
    });
    rx.recv_timeout(std::time::Duration::from_secs(15)).unwrap();
    pane.update(doc, || {});
    pane.confirm(doc, action).unwrap()
}
struct FinishFixture {
    _dir: tempfile::TempDir,
    artifacts: vectrcap::persistence::artifacts::ArtifactStore,
    history: Arc<vectrcap::persistence::history::History>,
    policy: vectrcap::persistence::settings::ArchivePolicy,
}
impl FinishFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        Self {
            artifacts: vectrcap::persistence::artifacts::ArtifactStore::new(
                dir.path().join("spool"),
            )
            .unwrap(),
            history: Arc::new(
                vectrcap::persistence::history::History::open(&dir.path().join("history.db"))
                    .unwrap(),
            ),
            policy: vectrcap::persistence::settings::ArchivePolicy {
                archive_root: Some(dir.path().join("archive")),
                ..Default::default()
            },
            _dir: dir,
        }
    }
    fn persist(
        &self,
        transaction: &mut vectrcap::app::image_finish::ImageFinishTransaction,
        decision: vectrcap::app::image_finish::ArchiveDecision,
    ) -> Result<(), vectrcap::app::error::AppError> {
        transaction.persist(
            &self.artifacts,
            self.history.clone(),
            &self.policy,
            &Default::default(),
            None,
            decision,
        )
    }
}
fn selected_redacted_document() -> EditorDocument {
    let mut d = document();
    d.apply(EditorCommand::Crop(Some(rect(0, 0, 20, 20))))
        .unwrap();
    d.apply(EditorCommand::Insert(redaction())).unwrap();
    d
}
#[test]
fn copy_image_also_archives_by_default() {
    use vectrcap::{app::image_finish::ImageFinishTransaction, ui::capture_overlay::OverlayAction};
    let fx = FinishFixture::new();
    let finish = image_finish(
        &selected_redacted_document(),
        OverlayAction::Copy,
        Default::default(),
    );
    assert_eq!(finish.prepared.extension(), "png");
    let expected = finish.prepared.encoded().clone();
    let mut transaction = ImageFinishTransaction::new(finish).unwrap();
    fx.persist(&mut transaction, Default::default()).unwrap();
    let path = transaction.archive().unwrap().path.clone();
    assert_eq!(std::fs::read(&path).unwrap(), &*expected);
    assert_eq!(
        std::fs::read(&transaction.artifact().unwrap().path).unwrap(),
        &*expected
    );
    transaction.discard_unadmitted(&fx.artifacts).unwrap();
    assert_eq!(std::fs::read_dir(fx.artifacts.root()).unwrap().count(), 0);
    assert!(path.exists());
}
#[test]
fn pin_action_emits_only_flattened_pixels() {
    use vectrcap::{app::image_finish::ImageFinishTransaction, ui::capture_overlay::OverlayAction};
    let fx = FinishFixture::new();
    let doc = selected_redacted_document();
    let finish = image_finish(&doc, OverlayAction::Pin, Default::default());
    let decoded = image::load_from_memory(finish.prepared.bytes())
        .unwrap()
        .to_rgba8();
    let mut transaction = ImageFinishTransaction::new(finish).unwrap();
    fx.persist(&mut transaction, Default::default()).unwrap();
    assert_eq!(transaction.finish().prepared.pixels().as_ref(), &decoded);
    assert_eq!(decoded.get_pixel(0, 0).0, [7, 8, 9, 255]);
    assert_eq!(
        transaction.finish().screen_location,
        Some(PhysicalPoint { x: 0, y: 0 })
    );
    assert_eq!(decoded.dimensions(), (20, 20));
    assert_ne!(decoded.dimensions(), doc.source().dimensions());
}
#[test]
fn archive_contains_only_final_redacted_pixels() {
    use vectrcap::{
        app::image_finish::ImageFinishTransaction,
        editor::export::{ExportFormat, ImageExportOptions, Resize},
        ui::capture_overlay::OverlayAction,
    };
    for format in [
        ExportFormat::Png,
        ExportFormat::Jpeg,
        ExportFormat::WebpLossy,
        ExportFormat::WebpLossless,
    ] {
        let fx = FinishFixture::new();
        let doc = selected_redacted_document();
        let finish = image_finish(
            &doc,
            OverlayAction::Upload,
            ImageExportOptions::new(format, 90, Resize::Percent(50)).unwrap(),
        );
        let pixels = finish.prepared.pixels().clone();
        let bytes = finish.prepared.encoded().clone();
        let mut transaction = ImageFinishTransaction::new(finish).unwrap();
        fx.persist(&mut transaction, Default::default()).unwrap();
        let artifact = transaction.artifact().unwrap();
        assert_eq!(artifact.path.extension().unwrap(), "spool");
        assert_eq!(
            std::path::Path::new(&artifact.display_name)
                .extension()
                .unwrap(),
            format.extension()
        );
        let archived = std::fs::read(&transaction.archive().unwrap().path).unwrap();
        assert_eq!(archived, &*bytes);
        assert_eq!(
            image::load_from_memory(&archived).unwrap().to_rgba8(),
            *pixels
        );
        assert_eq!(pixels.dimensions(), (10, 10));
        let p = pixels.get_pixel(1, 1).0;
        for (value, expected) in p.into_iter().zip([7u8, 8, 9, 255]) {
            assert!(value.abs_diff(expected) <= if format.is_lossy() { 12 } else { 0 });
        }
    }
}
#[test]
fn archive_failure_requires_explicit_continue() {
    use vectrcap::{
        app::image_finish::{ArchiveDecision, ImageFinishTransaction},
        ui::capture_overlay::OverlayAction,
    };
    let fx = FinishFixture::new();
    std::fs::write(fx.policy.archive_root.as_ref().unwrap(), b"blocked").unwrap();
    let finish = image_finish(
        &selected_redacted_document(),
        OverlayAction::Copy,
        Default::default(),
    );
    let expected = finish.prepared.encoded().clone();
    let mut transaction = ImageFinishTransaction::new(finish).unwrap();
    assert!(
        fx.persist(&mut transaction, ArchiveDecision::ContinueWithoutLocalCopy)
            .is_err()
    );
    assert!(transaction.artifact().is_none());
    assert!(
        fx.persist(&mut transaction, ArchiveDecision::Retry)
            .is_err()
    );
    assert!(transaction.archive_failed());
    let staged = transaction.artifact().unwrap().id;
    assert!(
        fx.persist(&mut transaction, ArchiveDecision::Retry)
            .is_err()
    );
    assert_eq!(transaction.artifact().unwrap().id, staged);
    assert_eq!(transaction.finish().prepared.bytes(), &*expected);
    fx.persist(&mut transaction, ArchiveDecision::ContinueWithoutLocalCopy)
        .unwrap();
    assert!(transaction.archive().is_none());
    transaction.discard_unadmitted(&fx.artifacts).unwrap();
    assert_eq!(std::fs::read_dir(fx.artifacts.root()).unwrap().count(), 0);
}
#[test]
fn explicit_save_uses_selected_path_without_overwrite_in_every_mode() {
    use vectrcap::{
        app::image_finish::ImageFinishTransaction, persistence::settings::StorageMode,
        ui::capture_overlay::OverlayAction,
    };
    for mode in [
        StorageMode::AlwaysSave,
        StorageMode::PendingOrFailed,
        StorageMode::NeverAutomatic,
    ] {
        let mut fx = FinishFixture::new();
        fx.policy.storage_mode = mode;
        let finish = image_finish(
            &selected_redacted_document(),
            OverlayAction::Save,
            Default::default(),
        );
        let expected = finish.prepared.encoded().clone();
        let mut transaction = ImageFinishTransaction::new(finish).unwrap();
        let path = fx._dir.path().join("chosen.png");
        std::fs::write(&path, b"existing user file").unwrap();
        assert!(
            transaction
                .persist(
                    &fx.artifacts,
                    fx.history.clone(),
                    &fx.policy,
                    &Default::default(),
                    Some(&path),
                    Default::default()
                )
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"existing user file");
        assert!(transaction.saved_path().is_none());
        let path = fx._dir.path().join("other.png");
        transaction
            .persist(
                &fx.artifacts,
                fx.history.clone(),
                &fx.policy,
                &Default::default(),
                Some(&path),
                Default::default(),
            )
            .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), &*expected);
        assert!(transaction.artifact().is_none());
        assert!(!fx.policy.archive_root.unwrap().exists());
    }
}
#[test]
fn nonautomatic_copy_pin_do_not_persist_and_upload_keeps_spool() {
    use vectrcap::{
        app::image_finish::ImageFinishTransaction, persistence::settings::StorageMode,
        ui::capture_overlay::OverlayAction,
    };
    for mode in [StorageMode::PendingOrFailed, StorageMode::NeverAutomatic] {
        for action in [
            OverlayAction::Copy,
            OverlayAction::Pin,
            OverlayAction::Upload,
        ] {
            let mut fx = FinishFixture::new();
            fx.policy.storage_mode = mode;
            let finish = image_finish(&selected_redacted_document(), action, Default::default());
            let mut transaction = ImageFinishTransaction::new(finish).unwrap();
            fx.persist(&mut transaction, Default::default()).unwrap();
            assert_eq!(
                transaction.artifact().is_some(),
                action == OverlayAction::Upload
            );
            assert_eq!(
                transaction.archive().is_some(),
                action == OverlayAction::Upload && mode == StorageMode::PendingOrFailed
            );
        }
    }
}

#[derive(Default)]
struct ImageClipboard {
    images: std::sync::Mutex<Vec<Arc<RgbaImage>>>,
    fail: std::sync::atomic::AtomicBool,
}
impl vectrcap::platform::windows::clipboard::ClipboardWriter for ImageClipboard {
    fn write_text(&self, _: &str) -> Result<(), vectrcap::app::error::AppError> {
        Ok(())
    }
    fn write_image(&self, pixels: &RgbaImage) -> Result<(), vectrcap::app::error::AppError> {
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(vectrcap::app::error::AppError::Persistence(
                "injected image clipboard failure",
            ));
        }
        self.images.lock().unwrap().push(Arc::new(pixels.clone()));
        Ok(())
    }
}
#[derive(Default)]
struct FixtureCredentials(
    std::sync::Mutex<std::collections::HashSet<vectrcap::app::destinations::CredentialKey>>,
);
impl vectrcap::platform::CredentialStore for FixtureCredentials {
    fn read(
        &self,
        key: &vectrcap::app::destinations::CredentialKey,
    ) -> Result<vectrcap::vectr::client::UploadToken, vectrcap::app::error::AppError> {
        if !self.0.lock().unwrap().contains(key) {
            return Err(vectrcap::app::error::AppError::NeedsSetup(
                "fixture credential missing",
            ));
        }
        vectrcap::vectr::client::UploadToken::new("synthetic".into())
    }
    fn write(
        &self,
        key: &vectrcap::app::destinations::CredentialKey,
        _: &vectrcap::vectr::client::UploadToken,
    ) -> Result<(), vectrcap::app::error::AppError> {
        self.0.lock().unwrap().insert(key.clone());
        Ok(())
    }
    fn delete(
        &self,
        key: &vectrcap::app::destinations::CredentialKey,
    ) -> Result<(), vectrcap::app::error::AppError> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}

struct FixtureShell;
impl vectrcap::platform::windows::shell_actions::ShellActions for FixtureShell {
    fn open_path(&self, _: &std::path::Path) -> Result<(), vectrcap::app::error::AppError> {
        panic!("no OS shell effect expected")
    }
    fn reveal_path(&self, _: &std::path::Path) -> Result<(), vectrcap::app::error::AppError> {
        panic!("no OS shell effect expected")
    }
    fn open_url(&self, _: &str) -> Result<(), vectrcap::app::error::AppError> {
        panic!("no network effect expected")
    }
}
struct ControllerFixture {
    storage: FinishFixture,
    app: vectrcap::app::controller::AppController,
    clipboard: Arc<ImageClipboard>,
}
impl ControllerFixture {
    fn new() -> Self {
        use vectrcap::app::{
            controller::AppController, destination_manager::FileDestinationRepository,
            events::AppCommand,
        };
        let storage = FinishFixture::new();
        let clipboard = Arc::new(ImageClipboard::default());
        let mut app = AppController::open(
            Box::new(
                FileDestinationRepository::open(&storage._dir.path().join("settings.json"))
                    .unwrap(),
            ),
            storage.history.clone(),
            Arc::new(FixtureCredentials::default()),
            Arc::new(
                vectrcap::persistence::artifacts::ArtifactStore::new(
                    storage.artifacts.root().to_owned(),
                )
                .unwrap(),
            ),
            Arc::new(FixtureShell),
            clipboard.clone(),
        )
        .unwrap();
        let effects = app.dispatch(AppCommand::SetArchivePolicy {
            policy: storage.policy.clone(),
            revision: app.settings().revision,
        });
        assert_no_error(&effects);
        Self {
            storage,
            app,
            clipboard,
        }
    }
    fn set_mode(&mut self, mode: vectrcap::persistence::settings::StorageMode) {
        self.storage.policy.storage_mode = mode;
        let effects = self
            .app
            .dispatch(vectrcap::app::events::AppCommand::SetArchivePolicy {
                policy: self.storage.policy.clone(),
                revision: self.app.settings().revision,
            });
        assert_no_error(&effects);
    }
    fn seed_destination(&mut self) {
        use vectrcap::{
            app::{destination_manager::DestinationEdit, events::AppCommand},
            persistence::settings::ServerOrigin,
            vectr::client::UploadToken,
        };
        let mut destinations = self.app.settings().destinations.clone();
        let id = destinations
            .add(
                "Fixture",
                ServerOrigin::parse("https://fixture.invalid").unwrap(),
                None,
            )
            .unwrap();
        let effects = self.app.dispatch(AppCommand::ApplyDestinations {
            edit: DestinationEdit {
                destinations,
                tokens: vec![(id, UploadToken::new("synthetic".into()).unwrap())],
            },
            revision: self.app.settings().revision,
        });
        assert_no_error(&effects);
    }
    fn start(
        &mut self,
        mode: vectrcap::capture::request::CaptureMode,
    ) -> (
        uuid::Uuid,
        Arc<std::sync::Mutex<vectrcap::app::controller::CaptureSurface>>,
    ) {
        use vectrcap::app::events::{AppCommand, AppEffect, AppEvent};
        let effects = self.app.dispatch(AppCommand::StartCapture {
            mode,
            selection: Default::default(),
        });
        let (session, request) = effects
            .into_iter()
            .find_map(|e| match e {
                AppEffect::AcquireCapture { session, request } => Some((session, request)),
                _ => None,
            })
            .unwrap();
        let snapshot = fixture_snapshot(request);
        let surface = self
            .app
            .on_event(AppEvent::CaptureAcquired {
                session,
                result: Ok(snapshot),
            })
            .into_iter()
            .find_map(|e| match e {
                AppEffect::CaptureSurface(Some(surface)) => Some(surface),
                _ => None,
            })
            .unwrap();
        {
            use vectrcap::ui::annotation_toolbar::AnnotationTool;
            let mut state = surface.lock().unwrap();
            state.overlay.clear_crop().unwrap();
            state.overlay.set_tool(AnnotationTool::Region).unwrap();
            state
                .overlay
                .pointer_down(PhysicalPoint { x: 0, y: 0 })
                .unwrap();
            state
                .overlay
                .pointer_move(PhysicalPoint { x: 20, y: 20 }, false)
                .unwrap();
            state
                .overlay
                .pointer_up(PhysicalPoint { x: 20, y: 20 }, false)
                .unwrap();
            state.overlay.set_tool(AnnotationTool::Redaction).unwrap();
            state
                .overlay
                .pointer_down(PhysicalPoint { x: 0, y: 0 })
                .unwrap();
            state
                .overlay
                .pointer_up(PhysicalPoint { x: 12, y: 12 }, false)
                .unwrap();
        }
        (session, surface)
    }
}
fn assert_no_error(effects: &[vectrcap::app::events::AppEffect]) {
    for effect in effects {
        if let vectrcap::app::events::AppEffect::Error(error) = effect {
            panic!("unexpected controller error: {error}");
        }
    }
}
fn fixture_snapshot(request: vectrcap::capture::request::CaptureRequest) -> CaptureSnapshot {
    use vectrcap::{
        app::error::AppError,
        capture::{
            request::{CaptureService, CaptureSource, Countdown},
            windows::WindowCandidate,
        },
    };
    struct Source;
    impl CaptureSource for Source {
        fn monitors(&mut self) -> Result<Vec<MonitorGeometry>, AppError> {
            Ok(vec![MonitorGeometry::new(
                "fixture".into(),
                rect(-20, -10, 100, 80),
                1.0,
            )?])
        }
        fn pointer(&mut self) -> Result<PhysicalPoint, AppError> {
            Ok(PhysicalPoint { x: 0, y: 0 })
        }
        fn windows(&mut self) -> Result<Vec<WindowCandidate>, AppError> {
            Ok(vec![])
        }
        fn prepare_window(&mut self, _: WindowCandidate) -> Result<(), AppError> {
            unreachable!()
        }
        fn desktop(
            &mut self,
            bounds: PhysicalRect,
            _: &[MonitorGeometry],
            _: bool,
        ) -> Result<Vec<u8>, AppError> {
            Ok(RgbaImage::from_fn(bounds.width(), bounds.height(), |x, y| {
                Rgba([(x * 17) as u8, (y * 21) as u8, (x * y) as u8, 255])
            })
            .into_raw())
        }
        fn window(&mut self, _: WindowCandidate, _: bool) -> Result<Vec<u8>, AppError> {
            unreachable!()
        }
    }
    struct Clock;
    impl Countdown for Clock {
        fn wait(&mut self, _: u8, _: &tokio_util::sync::CancellationToken) -> Result<(), AppError> {
            Ok(())
        }
        fn hide_chrome(&mut self) -> Result<(), AppError> {
            Ok(())
        }
    }
    CaptureService::new(Source, Clock, Default::default())
        .snapshot(request)
        .unwrap()
}
fn confirm_surface(
    surface: &Arc<std::sync::Mutex<vectrcap::app::controller::CaptureSurface>>,
    action: vectrcap::ui::capture_overlay::OverlayAction,
    options: vectrcap::editor::export::ImageExportOptions,
) -> vectrcap::ui::image_export::ImageFinish {
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let mut state = surface.lock().unwrap();
        *state.overlay.output_mut().options_mut() = options;
        state.overlay.prepare_output(move || {
            tx.send(()).unwrap();
        });
    }
    rx.recv_timeout(std::time::Duration::from_secs(15)).unwrap();
    let mut state = surface.lock().unwrap();
    state.overlay.prepare_output(|| {});
    state.overlay.confirm_finish(action).unwrap()
}
fn archive_files(fx: &ControllerFixture) -> Vec<std::path::PathBuf> {
    let root = fx.storage.policy.archive_root.as_ref().unwrap();
    if !root.is_dir() {
        return vec![];
    }
    std::fs::read_dir(root)
        .unwrap()
        .flat_map(|month| {
            std::fs::read_dir(month.unwrap().path())
                .unwrap()
                .map(|e| e.unwrap().path())
        })
        .collect()
}
#[test]
fn controller_copy_pin_and_upload_use_real_finish_pixels_in_every_format() {
    use vectrcap::{
        app::{
            controller::AppController,
            events::{AppCommand, AppEffect},
        },
        capture::request::CaptureMode,
        editor::export::{ExportFormat, ImageExportOptions, Resize},
        ui::capture_overlay::OverlayAction,
    };
    for format in [
        ExportFormat::Png,
        ExportFormat::Jpeg,
        ExportFormat::WebpLossy,
        ExportFormat::WebpLossless,
    ] {
        for action in [
            OverlayAction::Copy,
            OverlayAction::Pin,
            OverlayAction::Upload,
        ] {
            let mut fx = ControllerFixture::new();
            if action == OverlayAction::Upload {
                fx.seed_destination();
            }
            let (session, surface) = fx.start(CaptureMode::Region);
            let finish = confirm_surface(
                &surface,
                action,
                ImageExportOptions::new(format, 73, Resize::Percent(50)).unwrap(),
            );
            let pixels = finish.prepared.pixels().clone();
            let bytes = finish.prepared.encoded().clone();
            let effects = fx.app.dispatch(AppCommand::FinishCapture {
                session,
                finish,
                save_path: None,
            });
            assert_no_error(&effects);
            assert!(
                effects
                    .iter()
                    .any(|e| matches!(e, AppEffect::CaptureSurface(None)))
            );
            assert_eq!(
                fx.app.settings().last_region.as_ref().unwrap().bounds(),
                rect(0, 0, 20, 20)
            );
            if action == OverlayAction::Copy {
                assert_eq!(
                    fx.clipboard.images.lock().unwrap()[0].as_ref(),
                    pixels.as_ref()
                );
            }
            if action == OverlayAction::Pin {
                let pin = effects
                    .iter()
                    .find_map(|e| {
                        if let AppEffect::PinImage(pin) = e {
                            Some(pin)
                        } else {
                            None
                        }
                    })
                    .unwrap();
                assert!(Arc::ptr_eq(&pin.pixels, &pixels));
                assert_eq!(pin.screen_location, Some(PhysicalPoint { x: 0, y: 0 }));
            }
            let paths = archive_files(&fx);
            assert_eq!(paths.len(), 1);
            assert_eq!(paths[0].extension().unwrap(), format.extension());
            assert_eq!(std::fs::read(&paths[0]).unwrap(), &*bytes);
            assert_eq!(image::load_from_memory(&bytes).unwrap().to_rgba8(), *pixels);
            let jobs = fx.storage.history.shell_jobs().unwrap();
            if action == OverlayAction::Upload {
                assert_eq!(jobs.len(), 1);
                assert_eq!(std::fs::read(&jobs[0].artifact.path).unwrap(), &*bytes);
                assert_eq!(
                    AppController::viewer_extension(&jobs[0].artifact),
                    Some(format.extension())
                );
            } else {
                assert!(jobs.is_empty());
                assert_eq!(
                    std::fs::read_dir(fx.storage.artifacts.root())
                        .unwrap()
                        .count(),
                    0
                );
            }
        }
    }
}
#[test]
fn controller_archive_failure_then_explicit_continue_retains_editor() {
    use vectrcap::{
        app::{
            events::{AppCommand, AppEffect},
            image_finish::ArchiveDecision,
        },
        capture::request::CaptureMode,
        ui::capture_overlay::OverlayAction,
    };
    let mut fx = ControllerFixture::new();
    std::fs::write(fx.storage.policy.archive_root.as_ref().unwrap(), b"blocked").unwrap();
    let (session, surface) = fx.start(CaptureMode::Region);
    let finish = confirm_surface(&surface, OverlayAction::Copy, Default::default());
    let bytes = finish.prepared.encoded().clone();
    let revision = surface.lock().unwrap().overlay.document().revision();
    let effects = fx.app.dispatch(AppCommand::FinishCapture {
        session,
        finish,
        save_path: None,
    });
    assert!(matches!(effects.as_slice(), [AppEffect::Error(_)]));
    assert!(fx.app.settings().last_region.is_none());
    assert!(fx.clipboard.images.lock().unwrap().is_empty());
    assert!(surface.lock().unwrap().archive_failed);
    let effects = fx.app.dispatch(AppCommand::RetryCaptureFinish {
        session,
        decision: ArchiveDecision::ContinueWithoutLocalCopy,
    });
    assert_no_error(&effects);
    assert!(fx.app.settings().last_region.is_some());
    assert_eq!(
        surface.lock().unwrap().overlay.document().revision(),
        revision
    );
    assert_eq!(
        fx.clipboard.images.lock().unwrap()[0].as_ref(),
        &image::load_from_memory(&bytes).unwrap().to_rgba8()
    );
}
#[test]
fn resume_capture_clears_stale_archive_failure_state() {
    use vectrcap::{
        app::events::{AppCommand, AppEffect},
        capture::request::CaptureMode,
        ui::capture_overlay::OverlayAction,
    };
    let mut fx = ControllerFixture::new();
    std::fs::write(fx.storage.policy.archive_root.as_ref().unwrap(), b"blocked").unwrap();
    let (session, surface) = fx.start(CaptureMode::Region);
    let finish = confirm_surface(&surface, OverlayAction::Copy, Default::default());
    let effects = fx.app.dispatch(AppCommand::FinishCapture {
        session,
        finish,
        save_path: None,
    });
    assert!(matches!(effects.as_slice(), [AppEffect::Error(_)]));
    assert!(surface.lock().unwrap().archive_failed);
    assert!(surface.lock().unwrap().pending_finish);

    let effects = fx.app.dispatch(AppCommand::ResumeCapture(session));
    assert_no_error(&effects);
    assert!(!surface.lock().unwrap().archive_failed);
    assert!(!surface.lock().unwrap().pending_finish);

    // The stale staged artifact from the failed finish must not leak into
    // the next one: the artifact store must be empty of unadmitted spool
    // entries before a fresh finish is confirmed.
    assert_eq!(
        std::fs::read_dir(fx.storage.artifacts.root())
            .unwrap()
            .count(),
        0
    );
}
#[test]
fn new_finish_is_accepted_even_when_discarding_the_stale_one_fails() {
    use std::os::windows::fs::OpenOptionsExt;
    use vectrcap::{
        app::events::{AppCommand, AppEffect},
        capture::request::CaptureMode,
        ui::capture_overlay::OverlayAction,
    };
    let mut fx = ControllerFixture::new();
    fx.clipboard
        .fail
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (session, surface) = fx.start(CaptureMode::Region);
    let finish = confirm_surface(&surface, OverlayAction::Copy, Default::default());
    assert!(matches!(
        fx.app
            .dispatch(AppCommand::FinishCapture {
                session,
                finish,
                save_path: None,
            })
            .as_slice(),
        [AppEffect::Error(_)]
    ));
    let path = std::fs::read_dir(fx.storage.artifacts.root())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    // Deny delete sharing on the stale staged artifact so discarding it, as
    // part of accepting a new/different finish below, fails.
    let guard = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)
        .unwrap();

    let save_path = fx.storage._dir.path().join("new-finish.png");
    // The document/options are unchanged from the first confirm, so the
    // already-prepared output is reused directly (no fresh prepare/wake
    // cycle runs) -- only the action differs, which is what makes `!same`.
    let finish = surface
        .lock()
        .unwrap()
        .overlay
        .confirm_finish(OverlayAction::Save)
        .unwrap();
    let effects = fx.app.dispatch(AppCommand::FinishCapture {
        session,
        finish,
        save_path: Some(save_path.clone()),
    });
    // The failed discard of the stale locked artifact must be reported...
    assert!(effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
    // ...but it must not have swallowed this new, different finish action:
    // the Save still ran and produced the chosen file.
    assert!(save_path.exists());

    drop(guard);
}
#[test]
fn cancelled_failed_and_stale_sessions_never_update_last_region() {
    use vectrcap::{
        app::{
            events::{AppCommand, AppEffect},
            image_finish::ArchiveDecision,
        },
        capture::request::CaptureMode,
        ui::{annotation_toolbar::AnnotationTool, capture_overlay::OverlayAction},
    };
    let mut fx = ControllerFixture::new();
    fx.clipboard
        .fail
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (session, surface) = fx.start(CaptureMode::Region);
    let finish = confirm_surface(&surface, OverlayAction::Copy, Default::default());
    let effects = fx.app.dispatch(AppCommand::FinishCapture {
        session,
        finish,
        save_path: None,
    });
    assert!(matches!(effects.as_slice(), [AppEffect::Error(_)]));
    assert!(fx.app.settings().last_region.is_none());
    {
        let mut state = surface.lock().unwrap();
        state.overlay.set_tool(AnnotationTool::Rectangle).unwrap();
        state
            .overlay
            .pointer_down(PhysicalPoint { x: 2, y: 2 })
            .unwrap();
        state
            .overlay
            .pointer_up(PhysicalPoint { x: 8, y: 8 }, false)
            .unwrap();
    }
    fx.clipboard
        .fail
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let effects = fx.app.dispatch(AppCommand::RetryCaptureFinish {
        session,
        decision: ArchiveDecision::Retry,
    });
    assert!(matches!(effects.as_slice(), [AppEffect::Error(_)]));
    assert!(fx.app.settings().last_region.is_none());
    assert!(fx.clipboard.images.lock().unwrap().is_empty());
    assert_no_error(&fx.app.dispatch(AppCommand::CancelCapture(session)));
    assert!(fx.app.settings().last_region.is_none());
    assert_eq!(
        std::fs::read_dir(fx.storage.artifacts.root())
            .unwrap()
            .count(),
        0
    );
}
#[test]
fn pending_archive_is_released_only_after_confirmed_upload_event() {
    use vectrcap::{
        app::{
            events::{AppCommand, AppEffect, AppEvent},
            types::UploadReceipt,
        },
        capture::request::CaptureMode,
        persistence::{history::JobStatus, settings::StorageMode},
        ui::capture_overlay::OverlayAction,
        vectr::queue::QueueEffect,
    };
    let mut fx = ControllerFixture::new();
    fx.seed_destination();
    fx.set_mode(StorageMode::PendingOrFailed);
    let (session, surface) = fx.start(CaptureMode::Region);
    let finish = confirm_surface(&surface, OverlayAction::Upload, Default::default());
    let effects = fx.app.dispatch(AppCommand::FinishCapture {
        session,
        finish,
        save_path: None,
    });
    assert_no_error(&effects);
    let id = effects
        .iter()
        .find_map(|e| {
            if let AppEffect::JobAccepted(id) = e {
                Some(*id)
            } else {
                None
            }
        })
        .unwrap();
    let archive = archive_files(&fx).pop().unwrap();
    fx.storage
        .history
        .transition(id, JobStatus::Running)
        .unwrap();
    let effects = fx.app.on_event(AppEvent::Queue(QueueEffect::Changed(id)));
    assert_no_error(&effects);
    assert!(archive.exists());
    let job = fx.storage.history.job(id).unwrap().unwrap();
    let session = uuid::Uuid::new_v4();
    fx.storage.history.set_session(id, session).unwrap();
    fx.storage
        .history
        .record_receipt(
            id,
            job.attempt,
            Some(session),
            &UploadReceipt::new("https://fixture.invalid/final.png", "final.png").unwrap(),
        )
        .unwrap();
    assert!(
        archive.exists(),
        "receipt persistence precedes the controller settlement event"
    );
    assert_no_error(&fx.app.on_event(AppEvent::Queue(QueueEffect::Changed(id))));
    assert!(!archive.exists());
    assert!(
        job.artifact.path.exists(),
        "archive reconciliation cannot delete the upload spool"
    );
}
#[test]
fn cancelled_acquisition_ignores_late_snapshot_and_fullscreen_does_not_replace_region() {
    use vectrcap::{
        app::events::{AppCommand, AppEffect, AppEvent},
        capture::request::CaptureMode,
        ui::capture_overlay::OverlayAction,
    };
    let mut fx = ControllerFixture::new();
    let effects = fx.app.dispatch(AppCommand::StartCapture {
        mode: CaptureMode::Region,
        selection: Default::default(),
    });
    let (session, request) = effects
        .into_iter()
        .find_map(|e| {
            if let AppEffect::AcquireCapture { session, request } = e {
                Some((session, request))
            } else {
                None
            }
        })
        .unwrap();
    assert_no_error(&fx.app.dispatch(AppCommand::CancelCapture(session)));
    assert!(
        fx.app
            .on_event(AppEvent::CaptureAcquired {
                session,
                result: Ok(fixture_snapshot(request))
            })
            .is_empty()
    );
    assert!(fx.app.settings().last_region.is_none());
    let (session, surface) = fx.start(CaptureMode::Fullscreen);
    let finish = confirm_surface(&surface, OverlayAction::Pin, Default::default());
    assert_no_error(&fx.app.dispatch(AppCommand::FinishCapture {
        session,
        finish,
        save_path: None,
    }));
    assert!(fx.app.settings().last_region.is_none());
    let (session, surface) = fx.start(CaptureMode::LastRegion);
    let finish = confirm_surface(&surface, OverlayAction::Pin, Default::default());
    assert_no_error(&fx.app.dispatch(AppCommand::FinishCapture {
        session,
        finish,
        save_path: None,
    }));
    assert_eq!(
        fx.app.settings().last_region.as_ref().unwrap().bounds(),
        rect(0, 0, 20, 20)
    );
}
#[test]
fn image_clipboard_serialization_preserves_top_down_final_rgba() {
    let pixels = RgbaImage::from_fn(3, 2, |x, y| Rgba([x as u8 + 30, y as u8 + 40, 70, 128]));
    let dib = vectrcap::platform::windows::clipboard::dibv5_bytes(&pixels).unwrap();
    assert_eq!(&dib[8..12], &(-2i32).to_le_bytes());
    assert_eq!(&dib[124..128], &[70, 40, 30, 128]);
    // The BMP decoder is independent of our DIB serializer. Add only its file
    // header so it can inspect every channel and the row orientation.
    let mut bmp = vec![0u8; 14];
    bmp[..2].copy_from_slice(b"BM");
    bmp[2..6].copy_from_slice(&((14 + dib.len()) as u32).to_le_bytes());
    bmp[10..14].copy_from_slice(&(138u32).to_le_bytes());
    bmp.extend_from_slice(&dib);
    let decoded = image::load_from_memory_with_format(&bmp, image::ImageFormat::Bmp)
        .unwrap()
        .to_rgba8();
    assert_eq!(decoded, pixels);
}

#[test]
fn copy_staging_failure_can_continue_in_memory_but_upload_cannot() {
    use vectrcap::{
        app::image_finish::{ArchiveDecision, ImageFinishTransaction},
        ui::capture_overlay::OverlayAction,
    };
    for action in [OverlayAction::Copy, OverlayAction::Upload] {
        let fx = FinishFixture::new();
        std::fs::remove_dir(fx.artifacts.root()).unwrap();
        std::fs::write(fx.artifacts.root(), b"blocked staging folder").unwrap();
        let finish = image_finish(&selected_redacted_document(), action, Default::default());
        let mut transaction = ImageFinishTransaction::new(finish).unwrap();
        assert!(
            fx.persist(&mut transaction, ArchiveDecision::Retry)
                .is_err()
        );
        let continued = fx.persist(&mut transaction, ArchiveDecision::ContinueWithoutLocalCopy);
        assert_eq!(continued.is_ok(), action == OverlayAction::Copy);
        assert!(transaction.artifact().is_none());
        assert!(transaction.archive().is_none());
    }
}
#[test]
fn changing_storage_mode_cannot_silently_bypass_a_failed_archive() {
    use vectrcap::{
        app::image_finish::{ArchiveDecision, ImageFinishTransaction},
        persistence::settings::StorageMode,
        ui::capture_overlay::OverlayAction,
    };
    let mut fx = FinishFixture::new();
    std::fs::write(fx.policy.archive_root.as_ref().unwrap(), b"blocked").unwrap();
    let mut transaction = ImageFinishTransaction::new(image_finish(
        &selected_redacted_document(),
        OverlayAction::Copy,
        Default::default(),
    ))
    .unwrap();
    assert!(
        fx.persist(&mut transaction, ArchiveDecision::Retry)
            .is_err()
    );
    fx.policy.storage_mode = StorageMode::NeverAutomatic;
    assert!(
        fx.persist(&mut transaction, ArchiveDecision::Retry)
            .is_err()
    );
    fx.persist(&mut transaction, ArchiveDecision::ContinueWithoutLocalCopy)
        .unwrap();
}
#[test]
fn copy_retry_reuses_archive_and_committed_last_region_survives_restart() {
    use vectrcap::{
        app::{
            controller::AppController, destination_manager::FileDestinationRepository,
            events::AppCommand,
        },
        capture::request::CaptureMode,
        ui::capture_overlay::OverlayAction,
    };
    let mut fx = ControllerFixture::new();
    fx.clipboard
        .fail
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (session, surface) = fx.start(CaptureMode::Region);
    let finish = confirm_surface(&surface, OverlayAction::Copy, Default::default());
    let effects = fx.app.dispatch(AppCommand::FinishCapture {
        session,
        finish,
        save_path: None,
    });
    assert!(matches!(
        effects.as_slice(),
        [vectrcap::app::events::AppEffect::Error(_)]
    ));
    let first = archive_files(&fx);
    assert_eq!(first.len(), 1);
    fx.clipboard
        .fail
        .store(false, std::sync::atomic::Ordering::SeqCst);
    // Pressing the finish button again must be just as safe as the Retry button.
    let finish = surface
        .lock()
        .unwrap()
        .overlay
        .confirm_finish(OverlayAction::Copy)
        .unwrap();
    assert_no_error(&fx.app.dispatch(AppCommand::FinishCapture {
        session,
        finish,
        save_path: None,
    }));
    assert_eq!(archive_files(&fx), first);
    let expected = fx.app.settings().last_region.clone();
    let path = fx.storage._dir.path().to_owned();
    let history = fx.storage.history.clone();
    drop(fx.app);
    let reloaded = AppController::open(
        Box::new(FileDestinationRepository::open(&path.join("settings.json")).unwrap()),
        history,
        Arc::new(FixtureCredentials::default()),
        Arc::new(vectrcap::persistence::artifacts::ArtifactStore::new(path.join("spool")).unwrap()),
        Arc::new(FixtureShell),
        Arc::new(ImageClipboard::default()),
    )
    .unwrap();
    assert_eq!(reloaded.settings().last_region, expected);
    assert!(expected.is_some());
}

#[test]
fn real_finish_quality_changes_never_replace_an_accepted_upload() {
    use vectrcap::{
        app::events::AppCommand,
        capture::request::CaptureMode,
        editor::export::{ExportFormat, ImageExportOptions, Resize},
        persistence::settings::StorageMode,
        ui::capture_overlay::OverlayAction,
    };
    for format in [ExportFormat::Jpeg, ExportFormat::WebpLossy] {
        let mut fx = ControllerFixture::new();
        fx.seed_destination();
        fx.set_mode(StorageMode::NeverAutomatic);
        let mut encoded = Vec::new();
        let mut artifacts = Vec::new();
        for quality in [10, 95] {
            let (session, surface) = fx.start(CaptureMode::Region);
            let finish = confirm_surface(
                &surface,
                OverlayAction::Upload,
                ImageExportOptions::new(format, quality, Resize::Width(15)).unwrap(),
            );
            assert_eq!(finish.prepared.dimensions(), (15, 15));
            encoded.push(finish.prepared.encoded().clone());
            assert_no_error(&fx.app.dispatch(AppCommand::FinishCapture {
                session,
                finish,
                save_path: None,
            }));
            let jobs = fx.storage.history.shell_jobs().unwrap();
            artifacts.push(
                jobs.iter()
                    .find(|job| {
                        !artifacts
                            .iter()
                            .any(|old: &vectrcap::app::types::Artifact| old.id == job.artifact.id)
                    })
                    .unwrap()
                    .artifact
                    .clone(),
            );
        }
        assert_ne!(encoded[0], encoded[1]);
        assert_ne!(encoded[0].len(), encoded[1].len());
        for (artifact, expected) in artifacts.iter().zip(&encoded) {
            assert_eq!(std::fs::read(&artifact.path).unwrap(), &**expected);
        }
        assert!(archive_files(&fx).is_empty());
    }
}

#[test]
fn cancellation_closes_even_when_a_staged_output_is_temporarily_locked() {
    use std::os::windows::fs::OpenOptionsExt;
    use vectrcap::{
        app::events::{AppCommand, AppEffect},
        capture::request::CaptureMode,
        ui::capture_overlay::OverlayAction,
    };
    let mut fx = ControllerFixture::new();
    fx.clipboard
        .fail
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (session, surface) = fx.start(CaptureMode::Region);
    let finish = confirm_surface(&surface, OverlayAction::Copy, Default::default());
    assert!(matches!(
        fx.app
            .dispatch(AppCommand::FinishCapture {
                session,
                finish,
                save_path: None
            })
            .as_slice(),
        [AppEffect::Error(_)]
    ));
    let path = std::fs::read_dir(fx.storage.artifacts.root())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    // The borrowed read handle intentionally refuses delete sharing.
    let guard = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(path)
        .unwrap();
    let effects = fx.app.dispatch(AppCommand::CancelCapture(session));
    assert!(effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, AppEffect::CaptureSurface(None)))
    );
    assert!(fx.app.settings().last_region.is_none());
    drop(guard);
    assert_eq!(
        fx.storage.artifacts.recover(&Default::default()).unwrap(),
        1
    );
    let effects = fx.app.dispatch(AppCommand::StartCapture {
        mode: CaptureMode::Region,
        selection: Default::default(),
    });
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, AppEffect::AcquireCapture { .. }))
    );
}
#[test]
fn acquisition_failure_is_visible_and_cannot_save_last_region() {
    use vectrcap::{
        app::{
            error::AppError,
            events::{AppCommand, AppEffect, AppEvent, Window},
        },
        capture::request::CaptureMode,
    };
    let mut fx = ControllerFixture::new();
    let effects = fx.app.dispatch(AppCommand::StartCapture {
        mode: CaptureMode::Region,
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
    let effects = fx.app.on_event(AppEvent::CaptureAcquired {
        session,
        result: Err(AppError::Capture("injected source failure")),
    });
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, AppEffect::Window(Some(Window::Settings))))
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, AppEffect::Error(AppError::Capture(_))))
    );
    assert!(fx.app.settings().last_region.is_none());
    assert!(archive_files(&fx).is_empty());
    assert_eq!(
        std::fs::read_dir(fx.storage.artifacts.root())
            .unwrap()
            .count(),
        0
    );
}
