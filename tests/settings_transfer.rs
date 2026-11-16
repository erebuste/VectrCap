mod task11_support;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex},
};
use task11_support::InstallFixture;
use vectrcap::{
    app::{destinations::*, error::AppError, shortcuts::*},
    persistence::{filename::FilenamePreset, settings::*, settings_transfer::*},
    recording::{audio::devices::SourceKind, options::*},
    workflow::{
        profile::*,
        runner::{TaskIntent, WorkflowContext},
    },
};

#[derive(Default)]
struct Effects {
    log: Mutex<Vec<&'static str>>,
    live: Mutex<HashMap<RegistrationId, KeyChord>>,
    fail_key: Mutex<Option<KeyCode>>,
    devices_fail: Mutex<bool>,
}
struct Registrar(Arc<Effects>);
impl ShortcutRegistrar for Registrar {
    fn register(&self, id: RegistrationId, chord: KeyChord) -> Result<(), AppError> {
        self.0.log.lock().unwrap().push("register");
        if *self.0.fail_key.lock().unwrap() == Some(chord.key) {
            return Err(AppError::InUse("fixture conflict"));
        }
        self.0.live.lock().unwrap().insert(id, chord);
        Ok(())
    }
    fn unregister(&self, id: RegistrationId) {
        self.0.log.lock().unwrap().push("unregister");
        self.0.live.lock().unwrap().remove(&id);
    }
}
impl ImportAudioResolver for Effects {
    fn resolve(&self, kind: SourceKind) -> Result<EndpointSelection, AppError> {
        self.log.lock().unwrap().push("resolve device");
        if *self.devices_fail.lock().unwrap() {
            return Err(AppError::NeedsSetup("fixture device unavailable"));
        }
        Ok(EndpointSelection::Id(
            match kind {
                SourceKind::System => "local-system",
                SourceKind::Microphone => "local-mic",
            }
            .into(),
        ))
    }
}
fn apply(
    f: &mut InstallFixture,
    bytes: &[u8],
    selection: ImportSelection,
    e: &Arc<Effects>,
) -> Result<(), AppError> {
    let preview = preview_import(bytes, f.manager.settings())?;
    let (mut hotkeys, _) = ShortcutManager::new(
        Box::new(Registrar(e.clone())),
        f.manager.settings().shortcuts.bindings.clone(),
    );
    SettingsTransfer::new(&mut f.manager, &mut hotkeys, e.as_ref()).apply_import(preview, selection)
}
fn document(s: &Settings) -> Vec<u8> {
    SettingsTransfer::export(s).unwrap()
}
fn selection() -> ImportSelection {
    ImportSelection {
        default_destination: true,
        ..ImportSelection::safe_preferences()
    }
}
fn destination(s: &mut Settings, name: &str, origin: &str) -> DestinationId {
    s.destinations
        .add(name, ServerOrigin::parse(origin).unwrap(), None)
        .unwrap()
}
fn bound() -> Settings {
    let mut s = Settings::default();
    let a = destination(&mut s, "First", "https://one.example");
    s.destinations.bind_credential(a, &HashSet::new()).unwrap();
    let b = destination(&mut s, "Second", "https://one.example");
    s.destinations.bind_credential(b, &HashSet::new()).unwrap();
    s.destinations.set_default(b).unwrap();
    s.tasks.global.destination = DestinationSelection::Specific(a);
    s.tasks.overrides.insert(
        ShortcutAction::UploadFile,
        TaskProfile {
            destination: DestinationSelection::Specific(b),
            ..Default::default()
        },
    );
    s.shortcuts
        .bindings
        .iter_mut()
        .find(|b| b.action == ShortcutAction::UploadFile)
        .unwrap()
        .destination = DestinationSelection::Specific(b);
    s
}
fn changed() -> Settings {
    let mut s = Settings::default();
    s.region.dim_percent = 50;
    s.appearance.ui_scale_percent = 150;
    s.notifications_enabled = false;
    s.filename = FilenamePreset {
        prefix: "Backup".into(),
        template: "{prefix}_{date}_{time}_{id}".into(),
    };
    let b = s
        .shortcuts
        .bindings
        .iter_mut()
        .find(|b| b.action == ShortcutAction::ShowSettings)
        .unwrap();
    b.enabled = true;
    b.chord = Some(KeyChord {
        ctrl: true,
        alt: true,
        shift: false,
        win: false,
        key: KeyCode::from_vk(0x41).unwrap(),
    });
    s
}

#[test]
fn export_never_contains_secret_or_history_canaries() {
    let mut s = bound();
    s.recording_options.mp4.audio.system = EndpointSelection::Id("SYNTHETIC_PRIVATE_DEVICE".into());
    let bounds = vectrcap::capture::geometry::PhysicalRect::new(0, 0, 100, 100).unwrap();
    let monitor = vectrcap::capture::geometry::MonitorGeometry::new(
        "SYNTHETIC_PRIVATE_MONITOR".into(),
        bounds,
        1.0,
    )
    .unwrap();
    s.last_region =
        Some(vectrcap::capture::last_region::LastRegion::new(bounds, &[monitor]).unwrap());
    s.startup.launch_at_sign_in = true;
    let bytes = document(&s);
    let text = String::from_utf8(bytes).unwrap();
    assert!(!text.contains("SYNTHETIC_PRIVATE_MONITOR"));
    for d in s.destinations.iter() {
        assert!(!text.contains(&d.id.0.to_string()));
        assert!(
            !text.contains(&serde_json::to_string(d.credential_ref.as_ref().unwrap()).unwrap())
        );
    }
    for key in [
        "token",
        "credential_ref",
        "credential_generation",
        "last_region",
        "endpoint",
        "metadata_cache",
        "pending_jobs",
        "pause_intent",
        "favorites",
        "diagnostics",
        "history",
        "startup",
        "source_image",
        "SYNTHETIC_PRIVATE_DEVICE",
    ] {
        assert!(!text.contains(key), "export includes {key}");
    }
}
#[test]
fn roundtrip_keeps_hotkeys_profiles_and_preferences() {
    let s = changed();
    let mut f = InstallFixture::new();
    apply(&mut f, &document(&s), selection(), &Arc::default()).unwrap();
    let got = f.manager.settings();
    assert_eq!(s.shortcuts, got.shortcuts);
    assert_eq!(s.tasks, got.tasks);
    assert_eq!(s.region, got.region);
    assert_eq!(s.filename, got.filename);
    assert_eq!(s.appearance, got.appearance);
    assert_eq!(s.notifications_enabled, got.notifications_enabled);
}
#[test]
fn unsupported_version_or_forbidden_fields_change_nothing() {
    let mut f = InstallFixture::new();
    let before = f.manager.settings().clone();
    let e = Arc::new(Effects::default());
    let base: Value = serde_json::from_slice(&document(&changed())).unwrap();
    for key in [
        "token",
        "credential_ref",
        "credential_blob",
        "credential_target",
        "uploader_metadata_cache",
        "pause_intents",
        "pending_jobs",
        "audio_endpoint_ids",
        "current_contrast_colors",
        "diagnostic_report",
        "urls",
        "install_data",
        "metadata_cache",
        "pause_intent",
        "favorites",
        "diagnostics",
        "history",
        "startup",
        "source_images",
        "last_region",
    ] {
        let mut v = base.clone();
        v[key] = json!("SYNTHETIC_SECRET_CANARY");
        let err = preview_import(&serde_json::to_vec(&v).unwrap(), f.manager.settings())
            .err()
            .unwrap();
        assert!(!err.to_string().contains("SYNTHETIC"));
    }
    let mut v = base;
    v["schema_version"] = json!(2);
    assert!(apply(&mut f, &serde_json::to_vec(&v).unwrap(), selection(), &e).is_err());
    assert_eq!(&before, f.manager.settings());
    assert!(e.log.lock().unwrap().is_empty());
}
#[test]
fn oversized_or_malformed_document_rejected() {
    for bytes in [
        vec![b' '; 1024 * 1024 + 1],
        b"{\"token\":\"SYNTHETIC_SECRET\"".to_vec(),
        b"null".to_vec(),
        b"[]".to_vec(),
    ] {
        let err = preview_import(&bytes, &Settings::default()).err().unwrap();
        assert!(!format!("{err:?}").contains("SYNTHETIC_SECRET"));
    }
}
#[test]
fn import_validation_precedes_side_effects() {
    let s = changed();
    let e = Arc::new(Effects::default());
    let mut f = InstallFixture::new();
    let before = f.manager.settings().clone();
    assert!(preview_import(&document(&s), &before).is_ok());
    let base: Value = serde_json::from_slice(&document(&s)).unwrap();
    for (path, value) in [
        (vec!["region", "dim_percent"], json!(81)),
        (vec!["region", "lens_source_count"], json!(10)),
        (vec!["region", "lens_scale"], json!(99)),
        (
            vec!["region", "mouse", "right"],
            json!("cmd.exe /c SYNTHETIC"),
        ),
        (vec!["appearance", "ui_scale_percent"], json!(101)),
        (vec!["filename", "template"], json!("../SYNTHETIC")),
        (vec!["image", "quality"], json!(0)),
    ] {
        let mut v = base.clone();
        let mut ptr = &mut v;
        for p in &path[..path.len() - 1] {
            ptr = &mut ptr[*p];
        }
        ptr[path[path.len() - 1]] = value;
        assert!(
            apply(&mut f, &serde_json::to_vec(&v).unwrap(), selection(), &e).is_err(),
            "accepted {path:?}"
        );
    }
    assert_eq!(&before, f.manager.settings());
    // A preview has no capability for uploads, browser opens, OS registration,
    // credential deletion, audio capture or filesystem cleanup.
    assert!(e.log.lock().unwrap().is_empty());
    assert!(f.store.entries.lock().unwrap().is_empty());
}
#[test]
fn stale_import_preview_requires_refresh() {
    let mut f = InstallFixture::new();
    let p = preview_import(&document(&changed()), f.manager.settings()).unwrap();
    f.manager.set_notifications_enabled(false).unwrap();
    let before = f.manager.settings().clone();
    let e = Arc::new(Effects::default());
    let (mut h, _) = ShortcutManager::new(
        Box::new(Registrar(e.clone())),
        before.shortcuts.bindings.clone(),
    );
    assert!(
        SettingsTransfer::new(&mut f.manager, &mut h, e.as_ref())
            .apply_import(p, selection())
            .is_err()
    );
    assert_eq!(&before, f.manager.settings());
    assert!(e.log.lock().unwrap().is_empty());
}
#[test]
fn hotkey_registration_failure_restores_current_settings() {
    let mut f = InstallFixture::new();
    let before = f.manager.settings().clone();
    let e = Arc::new(Effects::default());
    *e.fail_key.lock().unwrap() = Some(KeyCode::from_vk(0x41).unwrap());
    assert!(apply(&mut f, &document(&changed()), selection(), &e).is_err());
    assert_eq!(&before, f.manager.settings());
    assert_eq!(*f.repo.settings.lock().unwrap(), before);
    assert!(e.live.lock().unwrap().is_empty());
}
#[test]
fn config_commit_failure_restores_all_groups_and_hotkeys() {
    let mut f = InstallFixture::new();
    let before = f.manager.settings().clone();
    *f.repo.fail.lock().unwrap() = true;
    let e = Arc::new(Effects::default());
    assert!(apply(&mut f, &document(&changed()), selection(), &e).is_err());
    assert_eq!(&before, f.manager.settings());
    assert_eq!(*f.repo.settings.lock().unwrap(), before);
    assert!(e.live.lock().unwrap().is_empty());
}
#[test]
fn different_origin_never_reuses_or_exports_token() {
    let mut f = InstallFixture::new();
    let s = bound();
    apply(&mut f, &document(&s), selection(), &Arc::default()).unwrap();
    let original = f.manager.settings().clone();
    let mut other = Settings::default();
    destination(&mut other, "Third", "https://different.example");
    apply(&mut f, &document(&other), selection(), &Arc::default()).unwrap();
    for d in original.destinations.iter() {
        assert_eq!(f.manager.settings().destinations.get(d.id), Some(d));
    }
    assert!(
        f.manager
            .settings()
            .destinations
            .iter()
            .all(|d| d.credential_ref.is_none())
    );
}
#[test]
fn automation_and_cleanup_require_explicit_selection() {
    let mut s = Settings::default();
    s.tasks.global.capture.insert(CaptureTask::Upload);
    s.tasks.global.after_upload.insert(UploadTask::OpenUrl);
    s.archive.cleanup_age_days = Some(1);
    let bytes = document(&s);
    let mut f = InstallFixture::new();
    apply(
        &mut f,
        &bytes,
        ImportSelection::safe_preferences(),
        &Arc::default(),
    )
    .unwrap();
    assert!(
        !f.manager
            .settings()
            .tasks
            .global
            .capture
            .contains(&CaptureTask::Upload)
    );
    assert!(
        !f.manager
            .settings()
            .tasks
            .global
            .after_upload
            .contains(&UploadTask::OpenUrl)
    );
    assert_eq!(f.manager.settings().archive.cleanup_age_days, None);
    let mut sel = selection();
    sel.automation = true;
    sel.cleanup = true;
    apply(&mut f, &bytes, sel, &Arc::default()).unwrap();
    assert_eq!(f.manager.settings().tasks, s.tasks);
    assert_eq!(f.manager.settings().archive, s.archive);
}
#[test]
fn archive_path_import_does_not_move_or_delete_files() {
    let dir = tempfile::tempdir().unwrap();
    let keep = dir.path().join("keep.txt");
    std::fs::write(&keep, b"synthetic").unwrap();
    let absent = dir.path().join("not-created");
    let mut s = Settings::default();
    s.archive.archive_root = Some(absent.clone());
    s.archive.cleanup_age_days = Some(1);
    let mut f = InstallFixture::new();
    let p = preview_import(&document(&s), f.manager.settings()).unwrap();
    assert!(!p.diff().is_empty());
    apply(&mut f, &document(&s), selection(), &Arc::default()).unwrap();
    assert_eq!(std::fs::read(keep).unwrap(), b"synthetic");
    assert!(!absent.exists());
}
#[test]
fn audio_endpoint_ids_are_not_exported() {
    let mut s = Settings::default();
    s.recording_options.mp4.audio.system = EndpointSelection::Id("SYNTHETIC_SYSTEM_DEVICE".into());
    s.recording_options.mp4.audio.microphone = EndpointSelection::Id("SYNTHETIC_MIC_DEVICE".into());
    let text = String::from_utf8(document(&s)).unwrap();
    assert!(!text.contains("SYNTHETIC"));
    assert!(!text.contains("Default"));
}
#[test]
fn imported_audio_mode_needs_explicit_opt_in_and_local_resolution() {
    let mut s = changed();
    s.recording_options.mp4.audio.mode = AudioMode::Both;
    let bytes = document(&s);
    let mut f = InstallFixture::new();
    let e = Arc::new(Effects::default());
    apply(&mut f, &bytes, selection(), &e).unwrap();
    assert_eq!(
        f.manager.settings().recording_options.mp4.audio.mode,
        AudioMode::Off
    );
    assert!(!e.log.lock().unwrap().contains(&"resolve device"));
    let mut sel = selection();
    sel.audio_enablement = true;
    *e.devices_fail.lock().unwrap() = true;
    let before = f.manager.settings().clone();
    assert!(apply(&mut f, &bytes, sel.clone(), &e).is_err());
    assert_eq!(&before, f.manager.settings());
    *e.devices_fail.lock().unwrap() = false;
    apply(&mut f, &bytes, sel, &e).unwrap();
    let audio = &f.manager.settings().recording_options.mp4.audio;
    assert_eq!(audio.mode, AudioMode::Both);
    assert_eq!(audio.system, EndpointSelection::Id("local-system".into()));
    assert_eq!(audio.microphone, EndpointSelection::Id("local-mic".into()));
}
#[test]
fn running_job_keeps_snapshotted_profile() {
    let s = bound();
    let context = WorkflowContext::new(
        vectrcap::app::types::ArtifactKind::Screenshot,
        EffectiveProfile::snapshot(&s, None),
        TaskIntent::RunSelectedTasks,
        DestinationSelection::Inherit,
        &s.destinations,
        None,
    );
    let frozen = serde_json::to_vec(&context).unwrap();
    let mut f = InstallFixture::new();
    apply(&mut f, &document(&changed()), selection(), &Arc::default()).unwrap();
    assert_eq!(serde_json::to_vec(&context).unwrap(), frozen);
}
#[test]
fn destination_export_contains_only_safe_connection_preferences() {
    let s = bound();
    let v: Value = serde_json::from_slice(&document(&s)).unwrap();
    let ds = v["destinations"].as_array().unwrap();
    assert_eq!(ds.len(), 2);
    for d in ds {
        let keys: HashSet<_> = d.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            HashSet::from(["key", "name", "origin", "link_domain_override"])
        );
    }
    assert_ne!(ds[0]["key"], ds[1]["key"]);
    assert_eq!(ds[0]["origin"], ds[1]["origin"]);
}
#[test]
fn portable_keys_remap_all_task_hotkey_default_refs() {
    let s = bound();
    let mut f = InstallFixture::new();
    apply(&mut f, &document(&s), selection(), &Arc::default()).unwrap();
    let got = f.manager.settings();
    let ids: Vec<_> = got.destinations.iter().map(|d| d.id).collect();
    for old in s.destinations.iter() {
        assert!(!ids.contains(&old.id));
    }
    assert_ne!(ids[0], ids[1]);
    assert_eq!(got.destinations.default_id(), Some(ids[1]));
    assert_eq!(
        got.tasks.global.destination,
        DestinationSelection::Specific(ids[0])
    );
    assert_eq!(
        got.tasks.overrides[&ShortcutAction::UploadFile].destination,
        DestinationSelection::Specific(ids[1])
    );
    assert_eq!(
        got.shortcuts
            .get(ShortcutAction::UploadFile)
            .unwrap()
            .destination,
        DestinationSelection::Specific(ids[1])
    );
}
#[test]
fn import_never_auto_reuses_same_origin_credentials() {
    let s = bound();
    let mut f = InstallFixture::new();
    *f.repo.settings.lock().unwrap() = s.clone();
    // Reload a fixture manager from its fake repository with existing live bindings.
    f.manager = vectrcap::app::destination_manager::DestinationManager::open(
        Box::new(task11_support::RepoAdapter(f.repo.clone())),
        f.store.clone(),
        Arc::new(
            vectrcap::persistence::history::History::open(&f._dir.path().join("other.db")).unwrap(),
        ),
        f.refs.clone(),
    )
    .unwrap();
    let mut imported = Settings::default();
    destination(&mut imported, "Third", "https://one.example");
    apply(&mut f, &document(&imported), selection(), &Arc::default()).unwrap();
    assert!(
        f.manager
            .settings()
            .destinations
            .iter()
            .find(|d| d.name == "Third")
            .unwrap()
            .credential_ref
            .is_none()
    );
    for d in s.destinations.iter() {
        assert_eq!(Some(d), f.manager.settings().destinations.get(d.id));
    }
}
#[test]
fn mapped_origin_mismatch_rejected() {
    let mut f = InstallFixture::new();
    let mut local = Settings::default();
    let id = destination(&mut local, "Local", "https://elsewhere.example");
    apply(&mut f, &document(&local), selection(), &Arc::default()).unwrap();
    let id = f
        .manager
        .settings()
        .destinations
        .iter()
        .next()
        .map(|d| d.id)
        .unwrap_or(id);
    let before = f.manager.settings().clone();
    let mut sel = selection();
    sel.destination_mapping = BTreeMap::from([(1, id)]);
    assert!(apply(&mut f, &document(&bound()), sel, &Arc::default()).is_err());
    assert_eq!(&before, f.manager.settings());
}
#[test]
fn new_imported_destination_needs_token() {
    let mut f = InstallFixture::new();
    apply(&mut f, &document(&bound()), selection(), &Arc::default()).unwrap();
    assert!(matches!(
        resolve_destination(
            DestinationSelection::Inherit,
            DestinationSelection::Inherit,
            &f.manager.settings().destinations
        ),
        Err(AppError::NeedsSetup(_))
    ));
}
#[test]
fn import_cannot_retarget_running_jobs() {
    let s = bound();
    let frozen = resolve_destination(
        DestinationSelection::Inherit,
        DestinationSelection::Inherit,
        &s.destinations,
    )
    .unwrap();
    let old = frozen.clone();
    let mut f = InstallFixture::new();
    apply(&mut f, &document(&s), selection(), &Arc::default()).unwrap();
    assert_eq!(old, frozen);
    assert_ne!(
        f.manager.settings().destinations.default_id(),
        Some(frozen.id)
    );
}
#[test]
fn secret_install_file_is_not_settings_json() {
    let bytes=br#"{"format":"vectrcap-destination","version":1,"name":"Synthetic","origin":"https://one.example","token":"SYNTHETIC_TOKEN_CANARY"}"#;
    assert!(vectrcap::app::destination_install::DestinationInstallV1::parse(bytes).is_ok());
    assert!(preview_import(bytes, &Settings::default()).is_err());
    assert!(
        vectrcap::app::destination_install::DestinationInstallV1::parse(&document(
            &Settings::default()
        ))
        .is_err()
    );
}
#[test]
fn preferences_fixture_file_io_and_replacement_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("preferences.json");
    let s = changed();
    write_export(&file, &s, false).unwrap();
    assert!(write_export(&file, &Settings::default(), false).is_err());
    assert_eq!(std::fs::read(&file).unwrap(), document(&s));
    let p = read_import(&file, &Settings::default()).unwrap();
    assert!(!p.diff().is_empty());
    let mut malformed: Value = serde_json::from_slice(&document(&s)).unwrap();
    malformed["region"]["lens_source_count"] = json!(10);
    std::fs::write(&file, serde_json::to_vec(&malformed).unwrap()).unwrap();
    assert!(read_import(&file, &Settings::default()).is_err());
    malformed = serde_json::from_slice(&document(&s)).unwrap();
    malformed["shortcuts"][0]["action"] = malformed["shortcuts"][1]["action"].clone();
    std::fs::write(&file, serde_json::to_vec(&malformed).unwrap()).unwrap();
    assert!(read_import(&file, &Settings::default()).is_err());
    write_export(&file, &Settings::default(), true).unwrap();
    assert_eq!(std::fs::read(file).unwrap(), document(&Settings::default()));
}
#[test]
fn selected_groups_preserve_unselected_fields() {
    let mut f = InstallFixture::new();
    let before = f.manager.settings().clone();
    let sel = ImportSelection {
        appearance: true,
        ..Default::default()
    };
    apply(&mut f, &document(&changed()), sel, &Arc::default()).unwrap();
    let mut expected = before;
    expected.appearance.ui_scale_percent = 150;
    expected.revision += 1;
    assert_eq!(&expected, f.manager.settings());
}

#[test]
fn explicit_exact_origin_mapping_preserves_credentials_and_distinct_siblings() {
    let s = bound();
    let mut f = InstallFixture::new();
    *f.repo.settings.lock().unwrap() = s.clone();
    f.manager = vectrcap::app::destination_manager::DestinationManager::open(
        Box::new(task11_support::RepoAdapter(f.repo.clone())),
        f.store.clone(),
        Arc::new(
            vectrcap::persistence::history::History::open(&f._dir.path().join("mapped.db"))
                .unwrap(),
        ),
        f.refs.clone(),
    )
    .unwrap();
    let ids: Vec<_> = s.destinations.iter().map(|d| d.id).collect();
    let mut sel = selection();
    sel.destination_mapping = BTreeMap::from([(1, ids[0]), (2, ids[1])]);
    apply(&mut f, &document(&s), sel.clone(), &Arc::default()).unwrap();
    assert_eq!(f.manager.settings().destinations, s.destinations);
    assert_eq!(f.manager.settings().tasks, s.tasks);
    sel.destination_mapping.insert(2, ids[0]);
    let before = f.manager.settings().clone();
    assert!(apply(&mut f, &document(&s), sel, &Arc::default()).is_err());
    assert_eq!(&before, f.manager.settings());
}
#[test]
fn nested_forbidden_fields_and_missing_references_rejected() {
    let s = bound();
    let base: Value = serde_json::from_slice(&document(&s)).unwrap();
    for (pointer, key) in [
        ("/destinations/0", "token"),
        ("/destinations/0", "credential_ref"),
        ("/destinations/0", "target"),
        ("/recording/audio", "system"),
        ("/tasks/global", "diagnostics"),
        ("/region", "command"),
        ("/image", "source_image"),
    ] {
        let mut v = base.clone();
        v.pointer_mut(pointer).unwrap()[key] = json!("SYNTHETIC_PRIVATE_CANARY");
        let err = preview_import(&serde_json::to_vec(&v).unwrap(), &Settings::default())
            .err()
            .unwrap();
        assert!(!err.to_string().contains("SYNTHETIC"));
    }
    for pointer in [
        "/default_destination",
        "/shortcuts/0/destination",
        "/tasks/global/destination",
    ] {
        let mut v = base.clone();
        *v.pointer_mut(pointer).unwrap() = json!(99);
        assert!(preview_import(&serde_json::to_vec(&v).unwrap(), &Settings::default()).is_err());
    }
}
#[test]
fn import_defers_transaction_until_capture_acquisition_and_session_end() {
    use vectrcap::app::{controller::AppController, events::*};
    let f = InstallFixture::new();
    let history = Arc::new(
        vectrcap::persistence::history::History::open(&f._dir.path().join("controller.db"))
            .unwrap(),
    );
    let artifacts = Arc::new(
        vectrcap::persistence::artifacts::ArtifactStore::new(f._dir.path().join("spool")).unwrap(),
    );
    let mut c = AppController::open(
        Box::new(task11_support::RepoAdapter(f.repo.clone())),
        history,
        f.store.clone(),
        artifacts,
        Arc::new(task11_support::NoShell),
        Arc::new(task11_support::Clipboard::default()),
    )
    .unwrap();
    let e = Arc::new(Effects::default());
    c.initialize_shortcuts(Box::new(Registrar(e.clone())));
    let effects = c.dispatch(AppCommand::StartCapture {
        mode: vectrcap::capture::request::CaptureMode::Region,
        selection: DestinationSelection::Inherit,
    });
    let session = effects
        .into_iter()
        .find_map(|effect| match effect {
            AppEffect::AcquireCapture { session, .. } => Some(session),
            _ => None,
        })
        .expect("capture should be acquiring");
    let before = c.settings().clone();
    let preview = preview_import(&document(&changed()), &before).unwrap();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    c.dispatch(AppCommand::ApplySettingsImport {
        preview: Box::new(preview),
        selection: selection(),
        reply: tx,
    });
    assert_eq!(c.settings(), &before);
    assert!(rx.try_recv().is_err());
    assert!(e.log.lock().unwrap().is_empty());
    c.dispatch(AppCommand::CancelCapture(session));
    assert_eq!(c.settings(), &before);
    c.on_event(AppEvent::CaptureAcquired {
        session,
        result: Err(AppError::Cancelled),
    });
    rx.try_recv().unwrap().unwrap();
    assert_eq!(c.settings().region, changed().region);
}
#[test]
fn import_updates_only_future_workflow_contexts() {
    use vectrcap::app::{controller::AppController, events::*};
    let s = bound();
    let f = InstallFixture::new();
    *f.repo.settings.lock().unwrap() = s.clone();
    for d in s.destinations.iter() {
        f.store
            .entries
            .lock()
            .unwrap()
            .insert(d.credential_ref.clone().unwrap(), "SYNTHETIC_TOKEN".into());
    }
    let history = Arc::new(
        vectrcap::persistence::history::History::open(&f._dir.path().join("contexts.db")).unwrap(),
    );
    let artifacts = Arc::new(
        vectrcap::persistence::artifacts::ArtifactStore::new(f._dir.path().join("spool")).unwrap(),
    );
    let mut c = AppController::open(
        Box::new(task11_support::RepoAdapter(f.repo.clone())),
        history,
        f.store.clone(),
        artifacts,
        Arc::new(task11_support::NoShell),
        Arc::new(task11_support::Clipboard::default()),
    )
    .unwrap();
    let e = Arc::new(Effects::default());
    c.initialize_shortcuts(Box::new(Registrar(e.clone())));
    let accepted = uuid::Uuid::new_v4();
    assert!(
        c.dispatch(AppCommand::BeginWorkflow {
            id: accepted,
            selection: DestinationSelection::Inherit
        })
        .iter()
        .any(|e| matches!(e, AppEffect::WorkflowAccepted(_)))
    );
    let frozen = resolve_destination(
        s.tasks.global.destination,
        DestinationSelection::Inherit,
        &s.destinations,
    )
    .unwrap();

    let mut imported = Settings::default();
    destination(&mut imported, "Future", "https://new.example");
    let p = preview_import(&document(&imported), c.settings()).unwrap();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    c.dispatch(AppCommand::ApplySettingsImport {
        preview: Box::new(p),
        selection: selection(),
        reply: tx,
    });
    rx.try_recv().unwrap().unwrap();
    let source = f._dir.path().join("synthetic.txt");
    std::fs::write(&source, b"synthetic upload").unwrap();
    let spool =
        vectrcap::persistence::artifacts::ArtifactStore::new(f._dir.path().join("spool")).unwrap();
    let artifact = spool
        .stage_file(&source, vectrcap::app::types::ArtifactKind::File)
        .unwrap();
    let effects = c.dispatch(AppCommand::AcceptWorkflow {
        id: accepted,
        artifact,
    });
    assert!(!effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
    let h =
        vectrcap::persistence::history::History::open(&f._dir.path().join("contexts.db")).unwrap();
    let jobs = h.shell_jobs().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].destination, frozen);
    assert_ne!(c.settings().destinations.default_id(), Some(frozen.id));
}

#[test]
fn image_tool_and_pin_defaults_roundtrip_and_validate_before_apply() {
    let mut s = changed();
    s.image.format = vectrcap::editor::export::ExportFormat::WebpLossy;
    s.image.quality = 75;
    s.image.resize = vectrcap::editor::export::Resize::Percent(50);
    s.pin.set_border_width(8).unwrap();
    s.pin.smooth = false;
    s.tools.width_tenths = 85;
    let mut f = InstallFixture::new();
    apply(&mut f, &document(&s), selection(), &Arc::default()).unwrap();
    assert_eq!(f.manager.settings().image, s.image);
    assert_eq!(f.manager.settings().pin, s.pin);
    assert_eq!(f.manager.settings().tools, s.tools);
    let mut v: Value = serde_json::from_slice(&document(&s)).unwrap();
    v["pin"]["border_width"] = json!(17);
    assert!(preview_import(&serde_json::to_vec(&v).unwrap(), &Settings::default()).is_err());
    let mut v: Value = serde_json::from_slice(&document(&s)).unwrap();
    v["tools"]["width_tenths"] = json!(0);
    assert!(preview_import(&serde_json::to_vec(&v).unwrap(), &Settings::default()).is_err());
}
#[test]
fn imported_defaults_are_used_by_future_editors_and_pins() {
    let mut s = changed();
    s.image.format = vectrcap::editor::export::ExportFormat::Jpeg;
    s.image.quality = 65;
    s.pin.set_border_width(6).unwrap();
    let mut f = InstallFixture::new();
    apply(&mut f, &document(&s), selection(), &Arc::default()).unwrap();
    let doc = vectrcap::editor::EditorDocument::from_image(Arc::new(image::RgbaImage::new(4, 4)))
        .unwrap();
    let mut overlay = vectrcap::ui::capture_overlay::CaptureOverlay::new(doc);
    overlay
        .initialize_preferences(f.manager.settings())
        .unwrap();
    assert_eq!(overlay.output_mut().options().format(), s.image.format);
    assert_eq!(overlay.output_mut().options().quality(), Some(65));
    let mut pins = vectrcap::pin::PinManager::with_defaults(s.pin.clone()).unwrap();
    pins.update_work_areas(
        vec![vectrcap::capture::geometry::PhysicalRect::new(0, 0, 100, 100).unwrap()],
        vectrcap::capture::geometry::PhysicalPoint { x: 5, y: 5 },
    )
    .unwrap();
    let id = pins
        .open(
            vectrcap::pin::PinnedImage::new(Arc::new(image::RgbaImage::new(4, 4)), None).unwrap(),
            None,
        )
        .unwrap();
    assert_eq!(pins.get(id).unwrap().state.options(), &s.pin);
}

#[test]
fn removing_override_cannot_enable_inherited_automation_without_consent() {
    let mut f = InstallFixture::new();
    let mut current = Settings::default();
    current.tasks.global.capture.insert(CaptureTask::Upload);
    current
        .tasks
        .global
        .after_upload
        .insert(UploadTask::OpenUrl);
    current
        .tasks
        .overrides
        .insert(ShortcutAction::UploadFile, TaskProfile::default());
    *f.repo.settings.lock().unwrap() = current.clone();
    f.manager = vectrcap::app::destination_manager::DestinationManager::open(
        Box::new(task11_support::RepoAdapter(f.repo.clone())),
        f.store.clone(),
        Arc::new(
            vectrcap::persistence::history::History::open(&f._dir.path().join("automation.db"))
                .unwrap(),
        ),
        f.refs.clone(),
    )
    .unwrap();
    let imported = Settings::default();
    apply(&mut f, &document(&imported), selection(), &Arc::default()).unwrap();
    let p =
        EffectiveProfile::snapshot(f.manager.settings(), Some(ShortcutAction::UploadFile)).profile;
    assert!(!p.capture.contains(&CaptureTask::Upload));
    assert!(!p.after_upload.contains(&UploadTask::OpenUrl));
}
#[test]
fn failed_import_preserves_working_registrations_and_credentials() {
    let s = bound();
    let mut f = InstallFixture::new();
    let mut current = s.clone();
    let b = current
        .shortcuts
        .bindings
        .iter_mut()
        .find(|b| b.action == ShortcutAction::ShowSettings)
        .unwrap();
    b.enabled = true;
    b.chord = Some(KeyChord {
        ctrl: true,
        alt: true,
        shift: false,
        win: false,
        key: KeyCode::from_vk(0x43).unwrap(),
    });
    *f.repo.settings.lock().unwrap() = current.clone();
    for d in current.destinations.iter() {
        f.store.entries.lock().unwrap().insert(
            d.credential_ref.clone().unwrap(),
            "SYNTHETIC_TOKEN_CANARY".into(),
        );
    }
    f.manager = vectrcap::app::destination_manager::DestinationManager::open(
        Box::new(task11_support::RepoAdapter(f.repo.clone())),
        f.store.clone(),
        Arc::new(
            vectrcap::persistence::history::History::open(&f._dir.path().join("rollback.db"))
                .unwrap(),
        ),
        f.refs.clone(),
    )
    .unwrap();
    let e = Arc::new(Effects::default());
    let (mut h, _) = ShortcutManager::new(
        Box::new(Registrar(e.clone())),
        current.shortcuts.bindings.clone(),
    );
    let old_live = e.live.lock().unwrap().clone();
    let old_tokens = f.store.entries.lock().unwrap().clone();
    let p = preview_import(&document(&changed()), &current).unwrap();
    *f.repo.fail.lock().unwrap() = true;
    assert!(
        SettingsTransfer::new(&mut f.manager, &mut h, e.as_ref())
            .apply_import(p, selection())
            .is_err()
    );
    assert_eq!(f.manager.settings(), &current);
    assert_eq!(*f.repo.settings.lock().unwrap(), current);
    assert_eq!(*e.live.lock().unwrap(), old_live);
    assert_eq!(*f.store.entries.lock().unwrap(), old_tokens);
    assert_eq!(h.bindings(), current.shortcuts.bindings);
}

#[test]
fn exhausted_revision_rejected_before_device_resolution_or_registration() {
    let mut f = InstallFixture::new();
    let current = Settings {
        revision: u64::MAX,
        ..Default::default()
    };
    *f.repo.settings.lock().unwrap() = current;
    f.manager = vectrcap::app::destination_manager::DestinationManager::open(
        Box::new(task11_support::RepoAdapter(f.repo.clone())),
        f.store.clone(),
        Arc::new(
            vectrcap::persistence::history::History::open(&f._dir.path().join("exhausted.db"))
                .unwrap(),
        ),
        f.refs.clone(),
    )
    .unwrap();
    let mut imported = changed();
    imported.recording_options.mp4.audio.mode = AudioMode::System;
    let mut sel = selection();
    sel.audio_enablement = true;
    let e = Arc::new(Effects::default());
    assert!(apply(&mut f, &document(&imported), sel, &e).is_err());
    assert!(e.log.lock().unwrap().is_empty());
}

#[test]
fn diff_shows_actual_archive_path_hotkey_and_audio_changes() {
    let mut s = changed();
    let dir = tempfile::tempdir().unwrap();
    s.archive.archive_root = Some(dir.path().join("future-archive"));
    s.recording_options.mp4.audio.mode = AudioMode::Both;
    let p = preview_import(&document(&s), &Settings::default()).unwrap();
    let archive = p.diff().iter().find(|g| g.group == "Archive").unwrap();
    assert!(archive.details.iter().any(|s| s.contains("future-archive")));
    let hotkeys = p
        .diff()
        .iter()
        .find(|g| g.group == "Hotkeys and tool bindings")
        .unwrap();
    assert!(hotkeys.details.iter().any(|s| s.contains("65")));
    let audio = p
        .diff()
        .iter()
        .find(|g| g.group == "Audio enablement")
        .unwrap();
    assert!(audio.details.iter().any(|s| s.contains("Both")));
}
