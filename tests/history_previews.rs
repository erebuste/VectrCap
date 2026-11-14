use vectrcap::{
    app::types::ArtifactKind,
    persistence::{artifacts::ArtifactStore, history::browser::HistoryRecord},
    ui::history_previews::decode,
};
fn record(root: &std::path::Path, width: u32) -> HistoryRecord {
    let path = root.join("fixture.png");
    image::RgbaImage::from_pixel(width, 4, image::Rgba([200, 40, 20, 255]))
        .save(&path)
        .unwrap();
    let store = ArtifactStore::new(root.join("spool")).unwrap();
    let artifact = store.stage_file(&path, ArtifactKind::Screenshot).unwrap();
    HistoryRecord {
        id: uuid::Uuid::new_v4(),
        name: "fixture.png".into(),
        kind: "screenshot".into(),
        size: artifact.size,
        created_ms: 0,
        favorite: false,
        local_available: true,
        retryable: false,
        artifact,
        job: None,
    }
}
#[test]
fn previews_decode_finalized_bytes_and_reject_missing_changed_or_oversized_images() {
    let dir = tempfile::tempdir().unwrap();
    let row = record(dir.path(), 8);
    let preview = decode(&row).unwrap();
    assert_eq!(preview.size, [8, 4]);
    std::fs::remove_file(&row.artifact.path).unwrap();
    assert!(decode(&row).is_none());
    let mut row = record(dir.path(), 8);
    row.artifact.sha256 = [0; 32];
    assert!(decode(&row).is_none());
    let large = tempfile::tempdir().unwrap();
    let row = record(large.path(), 5000);
    assert!(decode(&row).is_none());
}
#[test]
fn stale_results_never_repopulate_a_withdrawn_page() {
    use vectrcap::ui::history_previews::HistoryPreviews;
    let dir = tempfile::tempdir().unwrap();
    let row = record(dir.path(), 8);
    let mut cache = HistoryPreviews::default();
    let ctx = eframe::egui::Context::default();
    cache.update(&ctx, &[row]);
    cache.suspend();
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(5));
        let mut output = ctx.run_ui(Default::default(), |_| cache.update(&ctx, &[]));
        output.textures_delta.clear();
        assert!(cache.images.is_empty());
    }
}

#[test]
fn network_device_and_junction_paths_are_rejected_without_reading_targets() {
    use vectrcap::persistence::history::browser::open_local_file;
    for path in [
        r"\\untrusted.invalid\share\file.png",
        r"\\?\UNC\untrusted.invalid\share\file.png",
        r"\\.\PhysicalDrive0",
        r"C:relative.png",
    ] {
        assert!(open_local_file(std::path::Path::new(path)).is_err());
    }
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("file.png"), b"synthetic").unwrap();
    let link = root.path().join("link");
    let status = std::process::Command::new("cmd")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&link)
        .arg(&target)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(open_local_file(&link.join("file.png")).is_err());
    assert!(target.join("file.png").exists());
    // Remove only the junction itself before TempDir's ordinary cleanup.
    std::fs::remove_dir(&link).unwrap();
}
#[test]
fn cancellation_stops_local_verification_before_content_access() {
    let root = tempfile::tempdir().unwrap();
    let row = record(root.path(), 8);
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        vectrcap::persistence::history::browser::verified_local_cancellable(
            &row.artifact,
            u64::MAX,
            &cancel
        ),
        Err(vectrcap::app::error::AppError::Cancelled)
    ));
}
