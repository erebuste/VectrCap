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
