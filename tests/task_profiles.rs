use vectrcap::{
    app::{shortcuts::ShortcutAction, types::ArtifactKind},
    persistence::settings::{Settings, StorageMode},
    workflow::profile::*,
};

#[test]
fn profiles_inherit_or_override_per_hotkey() {
    let mut settings = Settings::default();
    let action = ShortcutAction::RegionScreenshot;
    assert_eq!(
        EffectiveProfile::snapshot(&settings, Some(action)).profile,
        settings.tasks.global
    );
    let mut custom = settings.tasks.global.clone();
    custom.capture.insert(CaptureTask::Upload);
    settings.tasks.overrides.insert(action, custom.clone());
    assert_eq!(
        EffectiveProfile::snapshot(&settings, Some(action)).profile,
        custom
    );
    assert!(
        !EffectiveProfile::snapshot(&settings, None)
            .profile
            .capture
            .contains(&CaptureTask::Upload)
    );
}
#[test]
fn storage_mode_and_save_task_have_one_setting() {
    let mut settings = Settings::default();
    settings.tasks.set_save_final(&mut settings.archive, false);
    assert_eq!(settings.archive.storage_mode, StorageMode::NeverAutomatic);
    assert!(!settings.tasks.save_final(&settings.archive));
    settings.tasks.set_save_final(&mut settings.archive, true);
    assert_eq!(settings.archive.storage_mode, StorageMode::AlwaysSave);
    assert!(
        !settings
            .tasks
            .global
            .capture
            .contains(&CaptureTask::SaveFinal)
    );
}
#[test]
fn default_capture_does_not_upload() {
    let p = TaskProfile::default();
    assert!(!p.capture.contains(&CaptureTask::Upload));
    assert!(p.capture.contains(&CaptureTask::Review));
    assert_eq!(CaptureTask::ALL.len(), 12);
    assert_eq!(
        UploadTask::ALL,
        [
            UploadTask::CopyUrl,
            UploadTask::OpenUrl,
            UploadTask::ShowResult
        ]
    );
    for name in ["Ocr", "Effects", "Print", "ShortenUrl", "Script"] {
        assert!(serde_json::from_str::<CaptureTask>(&format!("\"{name}\"")).is_err());
    }
}
#[test]
fn default_after_upload_copies_url() {
    let p = TaskProfile::default();
    assert_eq!(
        p.after_upload.into_iter().collect::<Vec<_>>(),
        vec![UploadTask::CopyUrl]
    );
}
#[test]
fn profile_snapshot_ignores_mid_job_changes() {
    let mut settings = Settings::default();
    let p = EffectiveProfile::snapshot(&settings, None);
    settings.tasks.global.capture.insert(CaptureTask::Upload);
    settings.archive.storage_mode = StorageMode::NeverAutomatic;
    assert!(!p.profile.capture.contains(&CaptureTask::Upload));
    assert_eq!(p.archive.storage_mode, StorageMode::AlwaysSave);
}
#[test]
fn incompatible_tasks_are_disabled_with_reason() {
    for task in [CaptureTask::CopyImage, CaptureTask::Pin] {
        assert!(task.unavailable_reason(ArtifactKind::Mp4).is_some());
        assert!(task.unavailable_reason(ArtifactKind::Screenshot).is_none());
    }
    assert!(
        CaptureTask::Review
            .unavailable_reason(ArtifactKind::File)
            .is_some()
    );
}
