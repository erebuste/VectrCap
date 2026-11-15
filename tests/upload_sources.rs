use std::{ffi::OsString, sync::Arc};
use vectrcap::{app::upload_sources::*, platform::windows::clipboard_sources::*};
#[test]
fn drop_uses_existing_file_validation_and_batch_order() {
    let files = vec![OsString::from(r"C:\b.zip"), r"C:\a.zip".into()];
    let UploadSource::Files(actual) = UploadSource::files(files.clone()).unwrap() else {
        panic!()
    };
    assert_eq!(actual, files);
    assert!(UploadSource::files(vec![r"\\server\a".into()]).is_err());
}
#[test]
fn clipboard_file_list_wins_over_preview_bitmap() {
    let source = snapshot_formats(
        Some(vec![r"C:\a.zip".into()]),
        Some(Arc::new(image::RgbaImage::new(1, 1))),
    )
    .unwrap();
    assert!(matches!(source, UploadSource::Files(_)));
}
#[test]
fn clipboard_image_opens_review_before_upload() {
    let source = UploadSource::ClipboardImage(Arc::new(image::RgbaImage::new(1, 1)));
    assert!(matches!(
        route_source(source, SourceFocus::UploadZone).unwrap(),
        SourceAction::ReviewImage(_)
    ));
}
#[test]
fn clipboard_source_is_immutable_after_acceptance() {
    let mut image = Arc::new(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([1, 2, 3, 4]),
    ));
    let source = snapshot_formats(None, Some(image.clone())).unwrap();
    Arc::make_mut(&mut image).put_pixel(0, 0, image::Rgba([9, 9, 9, 9]));
    let UploadSource::ClipboardImage(accepted) = source else {
        panic!()
    };
    assert_eq!(accepted.get_pixel(0, 0).0, [1, 2, 3, 4]);
}
#[test]
fn text_html_and_urls_never_upload_or_fetch() {
    assert!(snapshot_formats(None, None).is_err());
    let mut bytes = vec![0; 20];
    bytes[..4].copy_from_slice(&20u32.to_le_bytes());
    assert!(decode_file_list(&bytes).is_err());
}
#[test]
fn paste_into_text_field_remains_text() {
    let source = UploadSource::ClipboardImage(Arc::new(image::RgbaImage::new(1, 1)));
    assert!(matches!(
        route_source(source, SourceFocus::TextField).unwrap(),
        SourceAction::KeepTextPaste
    ));
}
#[test]
fn clipboard_busy_or_empty_has_no_upload_effect() {
    struct Empty;
    impl ClipboardSourceReader for Empty {
        fn snapshot(&self) -> Result<UploadSource, vectrcap::app::error::AppError> {
            Err(vectrcap::app::error::AppError::InUse(
                "clipboard unavailable",
            ))
        }
    }
    assert!(read_for_action(&Empty, SourceFocus::UploadZone).is_err());
}
#[test]
fn oversized_bitmap_rejected_before_allocation() {
    assert!(validate_bitmap_budget(100_000, 100_000, 400_000, 1).is_err());
    assert!(validate_bitmap_budget(1, 1, 4, 128 * 1024 * 1024 + 1).is_err());
    assert!(validate_bitmap_budget(2, 2, 1, 4).is_err());
    assert!(validate_bitmap_budget(2, 2, 8, 16).is_ok());
}
mod task11_support;
#[test]
fn controller_clipboard_review_is_image_local_and_does_not_admit_upload() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = task11_support::controller(dir.path());
    let before = app.settings().last_region.clone();
    let effects = app.dispatch(vectrcap::app::events::AppCommand::UploadSource(
        UploadSource::ClipboardImage(Arc::new(image::RgbaImage::new(2, 3))),
    ));
    let surface = effects
        .iter()
        .find_map(|e| match e {
            vectrcap::app::events::AppEffect::CaptureSurface(Some(surface)) => Some(surface),
            _ => None,
        })
        .expect("clipboard must open review");
    assert!(surface.lock().unwrap().image_local);
    assert!(!effects.iter().any(|e| matches!(
        e,
        vectrcap::app::events::AppEffect::JobAccepted(_)
            | vectrcap::app::events::AppEffect::StageFiles { .. }
            | vectrcap::app::events::AppEffect::AcquireCapture { .. }
    )));
    assert_eq!(app.settings().last_region, before);
}
