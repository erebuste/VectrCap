use vectrcap::persistence::settings::Settings;
#[test]
fn approved_settings_without_revision_remain_loadable_and_updates_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let mut value = serde_json::to_value(Settings::default()).unwrap();
    value.as_object_mut().unwrap().remove("revision");
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let mut settings = Settings::load(&path).unwrap();
    assert_eq!(
        serde_json::to_value(&settings).unwrap().get("revision"),
        Some(&serde_json::json!(0))
    );
    // Check persistence with no new secret-bearing fields.
    let mut value = serde_json::to_value(&settings).unwrap();
    value["revision"] = serde_json::json!(7);
    settings = serde_json::from_value(value).unwrap();
    settings.save(&path).unwrap();
    assert_eq!(
        serde_json::to_value(Settings::load(&path).unwrap()).unwrap()["revision"],
        7
    );
}

#[test]
fn failed_atomic_replace_preserves_old_revision_and_has_no_partial_output() {
    use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let old = Settings::default();
    old.save(&path).unwrap();
    let held = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)
        .unwrap();
    let new = Settings {
        revision: 1,
        ..old.clone()
    };
    assert!(new.save(&path).is_err());
    drop(held);
    assert_eq!(Settings::load(&path).unwrap(), old);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn legacy_settings_default_notifications_on_and_reject_invalid_values() {
    let mut value = serde_json::to_value(Settings::default()).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .remove("notifications_enabled");
    let settings: Settings = serde_json::from_value(value.clone()).unwrap();
    assert!(settings.notifications_enabled);
    value["notifications_enabled"] = serde_json::json!("false");
    assert!(serde_json::from_value::<Settings>(value).is_err());
}
