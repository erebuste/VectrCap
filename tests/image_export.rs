use image::{Rgba, RgbaImage};
use std::sync::Arc;
use vectrcap::{
    capture::geometry::PhysicalRect,
    editor::{
        Annotation, EditorCommand, EditorDocument,
        export::{ExportFormat, ImageExportOptions, Resize, prepare_export},
    },
};
fn document() -> EditorDocument {
    EditorDocument::from_image(Arc::new(RgbaImage::from_fn(40, 20, |x, y| {
        Rgba([x as u8 * 5, y as u8 * 10, 80, 255])
    })))
    .unwrap()
}

fn drive_finish(pane: &mut vectrcap::ui::image_export::ImageExportPane, doc: &EditorDocument) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        pane.update(doc, || {});
        if !pane.is_preparing() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "preview worker timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn finish_controls_reuse_preview_for_every_action() {
    use vectrcap::ui::{capture_overlay::OverlayAction, image_export::ImageExportPane};
    let mut pane = ImageExportPane::default();
    let mut doc = document();
    assert!(pane.confirm(&doc, OverlayAction::Upload).is_err());
    pane.options_mut().set_format(ExportFormat::Jpeg).unwrap();
    pane.options_mut().set_resize(Resize::Percent(50)).unwrap();
    drive_finish(&mut pane, &doc);
    let prepared = pane.ready(&doc).unwrap().clone();
    assert_eq!(prepared.dimensions(), (20, 10));
    for action in [
        OverlayAction::Upload,
        OverlayAction::Copy,
        OverlayAction::Save,
        OverlayAction::Pin,
    ] {
        let finish = pane.confirm(&doc, action).unwrap();
        assert_eq!(finish.action, action);
        assert!(Arc::ptr_eq(finish.prepared.encoded(), prepared.encoded()));
        assert!(Arc::ptr_eq(finish.prepared.pixels(), prepared.pixels()));
        assert_eq!(finish.screen_location, None);
    }
    assert!(pane.confirm(&doc, OverlayAction::Continue).is_err());
    pane.options_mut().set_quality(35).unwrap();
    assert!(pane.confirm(&doc, OverlayAction::Copy).is_err());
    drive_finish(&mut pane, &doc);
    assert!(pane.confirm(&doc, OverlayAction::Copy).is_ok());
    doc.apply(EditorCommand::Crop(None)).unwrap();
    assert!(pane.confirm(&doc, OverlayAction::Pin).is_err());
}

#[test]
fn finish_control_error_keeps_previous_output_and_document() {
    use vectrcap::ui::{capture_overlay::OverlayAction, image_export::ImageExportPane};
    let doc = document();
    let mut pane = ImageExportPane::default();
    drive_finish(&mut pane, &doc);
    let previous = pane.ready(&doc).unwrap().encoded().clone();
    pane.options_mut().set_resize(Resize::Width(100)).unwrap();
    drive_finish(&mut pane, &doc);
    assert!(pane.error().is_some());
    assert!(pane.ready(&doc).is_none());
    assert!(Arc::ptr_eq(&previous, pane.previous().unwrap().encoded()));
    assert_eq!(doc.revision(), 0);
    assert!(pane.confirm(&doc, OverlayAction::Save).is_err());
    pane.options_mut().set_resize(Resize::Original).unwrap();
    drive_finish(&mut pane, &doc);
    assert!(pane.ready(&doc).is_some());
    let ctx = eframe::egui::Context::default();
    for scale in [1.0, 2.0] {
        ctx.set_zoom_factor(scale);
        let mut emitted = None;
        let mut output = ctx.run_ui(
            eframe::egui::RawInput {
                screen_rect: Some(eframe::egui::Rect::from_min_size(
                    eframe::egui::Pos2::ZERO,
                    eframe::egui::vec2(600.0, 700.0),
                )),
                ..Default::default()
            },
            |ui| {
                emitted = pane.draw(ui, &doc);
            },
        );
        output.textures_delta.clear();
        assert!(
            emitted.is_none(),
            "rendering controls cannot finalize a capture"
        );
    }
}

#[test]
fn replacing_options_at_equal_revision_prepares_the_new_format() {
    use vectrcap::ui::image_export::ImageExportPane;
    let doc = document();
    let mut pane = ImageExportPane::default();
    drive_finish(&mut pane, &doc);
    *pane.options_mut() =
        ImageExportOptions::new(ExportFormat::Jpeg, 35, Resize::Percent(50)).unwrap();
    drive_finish(&mut pane, &doc);
    let prepared = pane
        .ready(&doc)
        .expect("replacement options must produce a new preview even at revision zero");
    assert_eq!(prepared.mime(), "image/jpeg");
    assert_eq!(prepared.dimensions(), (20, 10));
}

#[test]
fn review_window_preserves_pixels_and_has_no_implicit_finish() {
    use vectrcap::{app::error::AppError, ui::image_review::ImageReview};
    let source = document().source().clone();
    let mut review = ImageReview::from_image(source.clone()).unwrap();
    assert!(
        review
            .apply(EditorCommand::Crop(Some(
                PhysicalRect::new(100, 100, 10, 10).unwrap()
            )))
            .is_err()
    );
    assert_eq!(review.document().revision(), 0);
    review.finish_failed(AppError::Persistence("archive write failed"));
    let ctx = eframe::egui::Context::default();
    let mut event = None;
    let mut output = ctx.run_ui(Default::default(), |_| {
        event = review.show(&ctx);
    });
    output.textures_delta.clear();
    assert!(event.is_none());
    assert!(Arc::ptr_eq(review.document().source(), &source));
    assert_eq!(review.document().screen_location(), None);
}
#[test]
fn png_original_is_default() {
    let d = document();
    let options = ImageExportOptions::default();
    let out = prepare_export(d.export_revision(), options.clone()).unwrap();
    assert_eq!(out.format(), ExportFormat::Png);
    assert_eq!(out.mime(), "image/png");
    assert_eq!(out.extension(), "png");
    assert_eq!(out.dimensions(), (40, 20));
    assert_eq!(out.pixels().as_ref(), d.source().as_ref());
    assert_eq!(options.quality(), None);
    assert_eq!(options.resize(), Resize::Original);
    assert!(out.bytes().starts_with(b"\x89PNG\r\n\x1a\n"));
}
#[test]
fn quality_and_resize_bounds_are_checked() {
    for quality in [0, 101, 255] {
        assert!(ImageExportOptions::new(ExportFormat::Jpeg, quality, Resize::Original).is_err());
    }
    for resize in [
        Resize::Percent(0),
        Resize::Percent(101),
        Resize::Width(0),
        Resize::Height(0),
    ] {
        assert!(ImageExportOptions::new(ExportFormat::Png, 90, resize).is_err());
    }
    let d = document();
    for resize in [Resize::Width(41), Resize::Height(21)] {
        assert!(
            prepare_export(
                d.export_revision(),
                ImageExportOptions::new(ExportFormat::Png, 90, resize).unwrap()
            )
            .is_err()
        );
    }
    for (resize, expected) in [
        (Resize::Percent(50), (20, 10)),
        (Resize::Width(13), (13, 7)),
        (Resize::Height(1), (2, 1)),
    ] {
        assert_eq!(
            prepare_export(
                d.export_revision(),
                ImageExportOptions::new(ExportFormat::Png, 90, resize).unwrap()
            )
            .unwrap()
            .dimensions(),
            expected
        );
    }
}
#[test]
fn redaction_precedes_resampling_and_encoding() {
    // Different secret pixels under identical redaction must produce identical bytes,
    // including lossy outputs and resize filter edges.
    for format in [
        ExportFormat::Png,
        ExportFormat::Jpeg,
        ExportFormat::WebpLossy,
        ExportFormat::WebpLossless,
    ] {
        let mut outputs = vec![];
        for secret in [[255, 0, 0, 255], [0, 255, 0, 255]] {
            let mut source = RgbaImage::from_pixel(40, 20, Rgba([20, 30, 40, 255]));
            for y in 5..15 {
                for x in 10..30 {
                    source.put_pixel(x, y, Rgba(secret));
                }
            }
            let mut d = EditorDocument::from_image(Arc::new(source)).unwrap();
            d.apply(EditorCommand::Insert(
                Annotation::redaction(PhysicalRect::new(10, 5, 20, 10).unwrap(), [0, 0, 0])
                    .unwrap(),
            ))
            .unwrap();
            outputs.push(
                prepare_export(
                    d.export_revision(),
                    ImageExportOptions::new(format, 90, Resize::Percent(50)).unwrap(),
                )
                .unwrap(),
            );
        }
        assert_eq!(outputs[0].bytes(), outputs[1].bytes(), "{format:?}");
        assert!(
            outputs[0].pixels().get_pixel(10, 5).0[..3]
                .iter()
                .all(|v| *v < 15)
        );
    }
}
#[test]
fn preview_and_archive_upload_use_identical_bytes() {
    let d = document();
    let out = prepare_export(d.export_revision(), ImageExportOptions::default()).unwrap();
    let preview = out.clone();
    let archive = out.clone();
    let upload = out.clone();
    assert!(Arc::ptr_eq(preview.encoded(), archive.encoded()));
    assert!(Arc::ptr_eq(archive.encoded(), upload.encoded()));
    assert_eq!(out.encoded_size(), out.bytes().len());
    assert_eq!(
        image::load_from_memory(out.bytes()).unwrap().to_rgba8(),
        *out.pixels().as_ref()
    );
    let dir = tempfile::tempdir().unwrap();
    let history = Arc::new(
        vectrcap::persistence::history::History::open(&dir.path().join("history.sqlite3")).unwrap(),
    );
    let spool =
        vectrcap::persistence::artifacts::ArtifactStore::new(dir.path().join("spool")).unwrap();
    let archive = vectrcap::persistence::archive::ArchiveStore::new(
        history,
        dir.path().join("archive"),
        Default::default(),
    );
    for format in [
        ExportFormat::Png,
        ExportFormat::Jpeg,
        ExportFormat::WebpLossy,
        ExportFormat::WebpLossless,
    ] {
        let prepared = prepare_export(
            d.export_revision(),
            ImageExportOptions::new(format, 90, Resize::Original).unwrap(),
        )
        .unwrap();
        let path = dir.path().join(format!("fixture.{}", prepared.extension()));
        std::fs::write(&path, prepared.bytes()).unwrap();
        assert_eq!(
            image::open(&path).unwrap().to_rgba8(),
            *prepared.pixels().as_ref()
        );
        let artifact = spool
            .stage_file(&path, vectrcap::app::types::ArtifactKind::Screenshot)
            .unwrap();
        let saved = archive
            .save_finalized(&artifact, chrono::Local::now())
            .unwrap();
        assert_eq!(std::fs::read(&artifact.path).unwrap(), prepared.bytes());
        assert_eq!(std::fs::read(&saved.path).unwrap(), prepared.bytes());
        assert_eq!(saved.path.extension().unwrap(), prepared.extension());
    }
}
#[test]
fn copy_pin_match_decoded_output() {
    let d = document();
    for format in [
        ExportFormat::Jpeg,
        ExportFormat::WebpLossy,
        ExportFormat::WebpLossless,
    ] {
        let out = prepare_export(
            d.export_revision(),
            ImageExportOptions::new(format, 70, Resize::Percent(50)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            *out.pixels().as_ref(),
            image::load_from_memory(out.bytes()).unwrap().to_rgba8()
        );
        assert!(Arc::ptr_eq(out.pixels(), out.clone().pixels()));
    }
}
#[test]
fn jpeg_alpha_uses_visible_white_background() {
    let d = EditorDocument::from_image(Arc::new(RgbaImage::from_pixel(
        20,
        20,
        Rgba([200, 0, 0, 0]),
    )))
    .unwrap();
    let out = prepare_export(
        d.export_revision(),
        ImageExportOptions::new(ExportFormat::Jpeg, 90, Resize::Original).unwrap(),
    )
    .unwrap();
    assert!(
        out.pixels()
            .pixels()
            .all(|p| p.0[..3].iter().all(|v| *v >= 253) && p[3] == 255)
    );
}
#[test]
fn stale_preview_cannot_finish() {
    let mut d = document();
    let mut options = ImageExportOptions::default();
    let out = prepare_export(d.export_revision(), options.clone()).unwrap();
    assert!(out.matches(&d, &options));
    options.set_resize(Resize::Percent(50)).unwrap();
    assert!(!out.matches(&d, &options));
    options.set_resize(Resize::Original).unwrap();
    assert!(!out.matches(&d, &options));
    let out = prepare_export(d.export_revision(), options.clone()).unwrap();
    d.apply(EditorCommand::Crop(Some(
        PhysicalRect::new(0, 0, 20, 10).unwrap(),
    )))
    .unwrap();
    assert!(!out.matches(&d, &options));
    d.undo().unwrap();
    assert!(!out.matches(&d, &options));
    assert!(!out.matches(&document(), &options));
}
#[test]
fn metadata_contains_no_source_or_paths() {
    let d = document();
    for format in [
        ExportFormat::Png,
        ExportFormat::Jpeg,
        ExportFormat::WebpLossy,
        ExportFormat::WebpLossless,
    ] {
        let out = prepare_export(
            d.export_revision(),
            ImageExportOptions::new(format, 90, Resize::Original).unwrap(),
        )
        .unwrap();
        // Fresh encoders take only RGBA pixels: no source file/path/EXIF API exists.
        let bytes = out.bytes();
        for tag in [
            b"EXIF".as_slice(),
            b"eXIf",
            b"iTXt",
            b"tEXt",
            b"XMP ",
            b"C:\\Users",
        ] {
            assert!(!bytes.windows(tag.len()).any(|v| v == tag), "{format:?}");
        }
    }
}
#[test]
fn encoding_failure_keeps_document() {
    let d = document();
    let before = d.source().clone();
    let revision = d.revision();
    let previous = prepare_export(d.export_revision(), ImageExportOptions::default()).unwrap();
    // WebP has a format limit of 16383 per axis. A long review image remains editable.
    let long = EditorDocument::from_image(Arc::new(RgbaImage::new(1, 16384))).unwrap();
    assert!(
        prepare_export(
            long.export_revision(),
            ImageExportOptions::new(ExportFormat::WebpLossy, 90, Resize::Original).unwrap()
        )
        .is_err()
    );
    assert_eq!(long.bounds().height(), 16384);
    assert!(
        prepare_export(
            d.export_revision(),
            ImageExportOptions::new(ExportFormat::Png, 90, Resize::Width(500)).unwrap()
        )
        .is_err()
    );
    assert_eq!(d.revision(), revision);
    assert!(Arc::ptr_eq(d.source(), &before));
    assert!(previous.matches(&d, &ImageExportOptions::default()));
}
#[test]
fn new_export_never_mutates_queued_artifact() {
    let mut d = document();
    let options = ImageExportOptions::default();
    let queued = prepare_export(d.export_revision(), options.clone()).unwrap();
    let original = queued.bytes().to_vec();
    d.apply(EditorCommand::Insert(
        Annotation::redaction(PhysicalRect::new(0, 0, 40, 20).unwrap(), [0, 0, 0]).unwrap(),
    ))
    .unwrap();
    let updated = prepare_export(d.export_revision(), options).unwrap();
    assert_ne!(queued.bytes(), updated.bytes());
    assert_eq!(queued.bytes(), original);
}

#[test]
fn preview_worker_is_single_flight_and_rejects_stale_result() {
    use vectrcap::ui::image_export::ExportPreview;
    let mut d = document();
    let options = ImageExportOptions::default();
    let mut preview = ExportPreview::default();
    preview.start(&d, &options, || {}).unwrap();
    assert!(preview.is_preparing());
    assert!(preview.start(&d, &options, || {}).is_err());
    d.apply(EditorCommand::Crop(Some(
        PhysicalRect::new(0, 0, 20, 10).unwrap(),
    )))
    .unwrap();
    assert!(
        preview
            .wait(&d, &options, std::time::Duration::from_secs(10))
            .is_err()
    );
    assert!(preview.ready(&d, &options).is_none());
    assert!(!preview.is_preparing());
    preview.start(&d, &options, || {}).unwrap();
    preview
        .wait(&d, &options, std::time::Duration::from_secs(10))
        .unwrap();
    assert_eq!(preview.ready(&d, &options).unwrap().dimensions(), (20, 10));
}
#[test]
fn preview_failure_keeps_previous_valid_output() {
    use vectrcap::ui::image_export::ExportPreview;
    let d = document();
    let options = ImageExportOptions::default();
    let mut preview = ExportPreview::default();
    preview.start(&d, &options, || {}).unwrap();
    preview
        .wait(&d, &options, std::time::Duration::from_secs(10))
        .unwrap();
    let previous = preview.ready(&d, &options).unwrap().encoded().clone();
    let invalid = ImageExportOptions::new(ExportFormat::Png, 90, Resize::Width(500)).unwrap();
    preview.start(&d, &invalid, || {}).unwrap();
    assert!(preview.ready(&d, &options).is_none());
    assert!(
        preview
            .wait(&d, &invalid, std::time::Duration::from_secs(10))
            .is_err()
    );
    assert!(preview.ready(&d, &invalid).is_none());
    assert!(Arc::ptr_eq(
        &previous,
        preview.previous().unwrap().encoded()
    ));
}
#[test]
fn forked_document_does_not_share_export_identity() {
    let mut a = document();
    let mut b = a.clone();
    a.apply(EditorCommand::Insert(
        Annotation::redaction(PhysicalRect::new(0, 0, 20, 10).unwrap(), [0, 0, 0]).unwrap(),
    ))
    .unwrap();
    b.apply(EditorCommand::Insert(
        Annotation::redaction(PhysicalRect::new(0, 0, 20, 10).unwrap(), [255, 255, 255]).unwrap(),
    ))
    .unwrap();
    let options = ImageExportOptions::default();
    let out = prepare_export(a.export_revision(), options.clone()).unwrap();
    assert!(out.matches(&a, &options));
    assert!(!out.matches(&b, &options));
}
