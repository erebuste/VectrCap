mod support;
mod task11_support;

use std::{fs, os::windows::fs::OpenOptionsExt, time::Duration};
use support::{Fixture, Reply};
use vectrcap::{
    app::{
        error::AppError,
        events::{AppCommand, AppEffect},
    },
    diagnostics::{
        checks::{self, HealthClient, SCAN_ENTRY_BUDGET, StorageRoots},
        report::{Availability, DiagnosticSnapshot, ErrorCode, ReportPreview, TokenStatus},
    },
    persistence::{history::History, settings::Settings},
    ui::troubleshooting::TroubleshootingPane,
};

const INFO: &str = r#"{"name":"UPLOADER-PRIVATE-CANARY","domains":["https://domain-private-canary.example"],"domain":"https://domain-private-canary.example","settings":{"expires":"1.5d","maxViews":12,"nameFormat":"random","convert":"off","keepName":false,"passwordProtected":true}}"#;
const CANARIES: &[&str] = &[
    "PATH-PRIVATE-CANARY",
    "TOKEN-PRIVATE-CANARY",
    "LINK-PRIVATE-CANARY",
    "DEVICE-PRIVATE-CANARY",
    "UPLOADER-PRIVATE-CANARY",
    "domain-private-canary",
    "INSTALL-PRIVATE-CANARY",
    "LABEL-PRIVATE-CANARY",
];

fn roots(dir: &std::path::Path) -> StorageRoots {
    StorageRoots {
        spool: dir.join("PATH-PRIVATE-CANARY"),
        archive: Some(dir.join("archive")),
    }
}
fn assert_private(preview: &ReportPreview) {
    assert!(preview.as_str().len() <= 1024 * 1024);
    for canary in CANARIES {
        assert!(
            !preview.as_str().contains(canary),
            "private canary leaked: {canary}"
        );
    }
}
fn collect() -> (tempfile::TempDir, DiagnosticSnapshot) {
    let dir = tempfile::tempdir().unwrap();
    let history = History::open(&dir.path().join("history.db")).unwrap();
    let r = roots(dir.path());
    fs::create_dir(&r.spool).unwrap();
    fs::write(
        r.spool.join("DEVICE-PRIVATE-CANARY.png"),
        b"TOKEN-PRIVATE-CANARY LINK-PRIVATE-CANARY",
    )
    .unwrap();
    let snapshot = checks::collect(&history, &r, 1);
    (dir, snapshot)
}

#[test]
fn report_excludes_private_canaries() {
    let (_dir, mut snapshot) = collect();
    snapshot.connection.uploader = checks::uploader_result(Err(AppError::Protocol(
        "TOKEN-PRIVATE-CANARY PATH-PRIVATE-CANARY DEVICE-PRIVATE-CANARY",
    )));
    snapshot.connection.health = checks::health_result(Err(AppError::Io {
        stage: "LINK-PRIVATE-CANARY",
        kind: std::io::ErrorKind::PermissionDenied,
    }));
    let preview = ReportPreview::new(&snapshot).unwrap();
    assert_private(&preview);
    let value: serde_json::Value = serde_json::from_str(preview.as_str()).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(
        value["snapshot"]["connection"]["uploader"]["error"],
        "protocol"
    );
}

#[test]
fn collection_never_reads_pixels_credentials_or_history_contents() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = task11_support::controller(dir.path());
    // Deliberately malformed context must not be deserialized by aggregate SQL.
    let store =
        vectrcap::persistence::artifacts::ArtifactStore::new(dir.path().join("spool")).unwrap();
    let source = dir.path().join("DEVICE-PRIVATE-CANARY.png");
    fs::write(&source, b"TOKEN-PRIVATE-CANARY LINK-PRIVATE-CANARY").unwrap();
    let a = store
        .stage_file(&source, vectrcap::app::types::ArtifactKind::File)
        .unwrap();
    let history = History::open(&dir.path().join("history.db")).unwrap();
    let mut d = Settings::default().destinations;
    let id = d
        .add(
            "synthetic",
            vectrcap::persistence::settings::ServerOrigin::parse("https://fixture.example")
                .unwrap(),
            None,
        )
        .unwrap();
    d.bind_credential(id, &Default::default()).unwrap();
    let dest = vectrcap::app::destinations::resolve_destination(
        Default::default(),
        Default::default(),
        &d,
    )
    .unwrap();
    history.create_job(&a, &dest).unwrap();
    let c = rusqlite::Connection::open(dir.path().join("history.db")).unwrap();
    c.execute(
        "UPDATE jobs SET dest_label=?1, dest_origin=?1",
        ["LABEL-PRIVATE-CANARY"],
    )
    .unwrap();
    c.execute(
        "UPDATE artifacts SET display_name=?1",
        ["DEVICE-PRIVATE-CANARY"],
    )
    .unwrap();
    // Deny data reads while still allowing filesystem attribute queries.
    let _pixel_guard = fs::OpenOptions::new()
        .read(true)
        .share_mode(2 | 4)
        .open(&a.path)
        .unwrap();
    assert!(fs::read(&a.path).is_err());
    let snapshot = checks::collect(
        &history,
        &StorageRoots {
            spool: store.root().to_owned(),
            archive: None,
        },
        1,
    );
    assert_eq!(snapshot.pending_jobs, 1);
    assert_eq!(snapshot.storage[0].files, 1);
    assert_eq!(snapshot.storage[0].bytes, a.size);
    assert_private(&ReportPreview::new(&snapshot).unwrap());
    // Opening the page/refresh must not invoke the protected credential adapter.
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    assert!(
        app.dispatch(AppCommand::RefreshDiagnostics { reply: tx })
            .is_empty()
    );
    assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok());
}

#[test]
fn opening_page_never_starts_audio_or_requests_permission() {
    let mut pane = TroubleshootingPane::default();
    let ctx = eframe::egui::Context::default();
    let settings = Settings::default();
    let mut output = ctx.run_ui(Default::default(), |ui| {
        assert!(pane.controls(ui, &settings).is_empty());
    });
    output.textures_delta.clear();
    assert!(pane.preview().is_none());
    assert_eq!(pane.snapshot().audio, Availability::NotProbed);
    assert_eq!(pane.snapshot().capture, Availability::NotProbed);
}

#[tokio::test]
async fn health_test_sends_no_auth_and_follows_no_redirects() {
    let target = Fixture::new(vec![]).await;
    let f = Fixture::new(vec![
        Reply::json(302, "TOKEN-PRIVATE-CANARY")
            .header("Location", &format!("{}/stolen", target.origin.as_str())),
    ])
    .await;
    let health = HealthClient::with_root_certificate(f.origin.clone(), f.root.clone()).unwrap();
    let result = checks::health_result(health.check().await);
    assert_eq!(result.error, Some(ErrorCode::Redirect));
    assert_eq!(f.log().len(), 1);
    let r = &f.log()[0];
    assert_eq!((&*r.method, &*r.path), ("GET", "/healthz"));
    assert!(
        !r.headers.contains("authorization:")
            && !r.headers.contains("cookie:")
            && !r.headers.contains("x-vectr-domain:")
    );
    assert!(target.log().is_empty());
}

#[tokio::test]
async fn health_success_does_not_claim_valid_token() {
    let f = Fixture::new(vec![Reply::json(200, br#"{"ok":true}"#)]).await;
    let mut snapshot = DiagnosticSnapshot::default();
    let health = HealthClient::with_root_certificate(f.origin.clone(), f.root.clone()).unwrap();
    snapshot.connection.health = checks::health_result(health.check().await);
    assert_eq!(
        snapshot.connection.health.availability,
        Availability::Available
    );
    assert_eq!(snapshot.connection.uploader.status, TokenStatus::NotTested);
    assert_eq!(f.log().len(), 1);
    assert_private(&ReportPreview::new(&snapshot).unwrap());
}

#[tokio::test]
async fn uploader_metadata_success_is_distinct_from_health() {
    let f = Fixture::new(vec![Reply::json(200, INFO)]).await;
    let mut snapshot = DiagnosticSnapshot::default();
    snapshot.connection.uploader =
        checks::uploader_result(f.client_token("TOKEN-PRIVATE-CANARY").uploader_info().await);
    assert_eq!(snapshot.connection.uploader.status, TokenStatus::Verified);
    assert_eq!(
        snapshot.connection.health.availability,
        Availability::NotProbed
    );
    assert_eq!(f.log()[0].path, "/api/uploader");
    assert!(
        f.log()[0]
            .headers
            .contains("authorization: token-private-canary")
    );
    assert_private(&ReportPreview::new(&snapshot).unwrap());
    let f = Fixture::new(vec![Reply::json(404, "")]).await;
    let status = checks::uploader_result(f.client().uploader_info().await);
    assert_eq!(status.status, TokenStatus::UnverifiedManualDomain);
}

#[tokio::test]
async fn config_import_and_destination_metadata_stay_out_of_reports() {
    let (dir, _) = collect();
    let mut settings = Settings::default();
    settings
        .destinations
        .add(
            "LABEL-PRIVATE-CANARY",
            vectrcap::persistence::settings::ServerOrigin::parse(
                "https://domain-private-canary.example",
            )
            .unwrap(),
            None,
        )
        .unwrap();
    settings.archive.archive_root = Some(dir.path().join("PATH-PRIVATE-CANARY"));
    let backup =
        vectrcap::persistence::settings_transfer::SettingsTransfer::export(&settings).unwrap();
    assert!(
        vectrcap::persistence::settings_transfer::preview_import(&backup, &Settings::default())
            .is_ok()
    );
    fs::write(
        roots(dir.path())
            .spool
            .join("INSTALL-PRIVATE-CANARY.vectrcap"),
        b"TOKEN-PRIVATE-CANARY",
    )
    .unwrap();
    fs::write(roots(dir.path()).spool.join("preferences.json"), &backup).unwrap();
    let history = History::open(&dir.path().join("history.db")).unwrap();
    let mut snapshot = checks::collect(
        &history,
        &roots(dir.path()),
        settings.destinations.len() as u32,
    );
    assert_eq!(snapshot.storage[0].files, 3);
    let f = Fixture::new(vec![Reply::json(200, INFO)]).await;
    snapshot.connection.uploader = checks::uploader_result(f.client().uploader_info().await);
    assert_private(&ReportPreview::new(&snapshot).unwrap());
}

#[test]
fn bounded_storage_scan_reports_partial_errors() {
    let dir = tempfile::tempdir().unwrap();
    let r = roots(dir.path());
    fs::create_dir(&r.spool).unwrap();
    for n in 0..SCAN_ENTRY_BUDGET + 10 {
        fs::write(r.spool.join(format!("{n}.png")), b"abc").unwrap();
    }
    let scan = checks::scan_storage(&r);
    assert!(scan[0].partial);
    assert!(scan[0].entries <= SCAN_ENTRY_BUDGET as u64);
    assert!(scan[0].files > 0 && scan[0].bytes == scan[0].files * 3);
    assert!(scan[1].partial);
    let empty = dir.path().join("empty");
    fs::create_dir(&empty).unwrap();
    let scan = checks::scan_storage(&StorageRoots {
        spool: empty,
        archive: r.archive,
    });
    assert_eq!(scan[1].error, Some(ErrorCode::Missing));
}

#[test]
fn saved_report_matches_exact_preview() {
    let (dir, mut snapshot) = collect();
    let preview = ReportPreview::new(&snapshot).unwrap();
    let exact = preview.as_str().to_owned();
    snapshot.pending_jobs = 9876;
    let path = dir.path().join("diagnostic.json");
    preview.save(&path, false).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), exact);
    let clipboard = task11_support::Clipboard::default();
    preview.copy(&clipboard).unwrap();
    assert_eq!(clipboard.0.lock().unwrap()[0], exact);
    assert!(preview.save(&path, false).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), exact);
    assert_ne!(ReportPreview::new(&snapshot).unwrap().as_str(), exact);
    struct Fail;
    impl vectrcap::platform::windows::clipboard::ClipboardWriter for Fail {
        fn write_text(&self, _: &str) -> Result<(), AppError> {
            Err(AppError::Persistence("clipboard unavailable"))
        }
    }
    assert!(preview.copy(&Fail).is_err());
    assert_eq!(preview.as_str(), exact);
}

#[test]
fn diagnostic_failure_does_not_block_capture() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = task11_support::controller(dir.path());
    let c = rusqlite::Connection::open(dir.path().join("history.db")).unwrap();
    c.execute_batch("ALTER TABLE jobs RENAME TO synthetic_unavailable_jobs")
        .unwrap();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    assert!(
        app.dispatch(AppCommand::RefreshDiagnostics { reply: tx })
            .is_empty()
    );
    let snapshot = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(snapshot.history.availability, Availability::Unavailable);
    let effects = app.dispatch(AppCommand::StartCapture {
        mode: vectrcap::capture::request::CaptureMode::Region,
        selection: Default::default(),
    });
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, AppEffect::AcquireCapture { .. }))
    );
}

#[tokio::test]
async fn metadata_errors_and_health_tls_failures_are_sanitized_without_fallback() {
    for (code, body, expected) in [
        (401, "", ErrorCode::Unauthorized),
        (403, "", ErrorCode::Forbidden),
        (429, "", ErrorCode::RateLimited),
        (200, "bad", ErrorCode::Protocol),
        (302, "", ErrorCode::Redirect),
    ] {
        let f = Fixture::new(vec![Reply::json(code, body)]).await;
        let result = checks::uploader_result(f.client().uploader_info().await);
        assert_eq!(result.status, TokenStatus::Failed);
        assert_eq!(result.error, Some(expected));
    }
    let f = Fixture::new(vec![Reply::json(200, br#"{"ok":true}"#)]).await;
    let client = HealthClient::new(f.origin.clone()).unwrap();
    assert_eq!(
        checks::health_result(client.check().await).error,
        Some(ErrorCode::Tls)
    );
}

#[tokio::test]
async fn independent_connection_failures_preserve_the_other_probe_result() {
    let health = Fixture::new(vec![Reply::json(503, "PATH-PRIVATE-CANARY")]).await;
    let metadata = Fixture::new(vec![Reply::json(200, INFO)]).await;
    let result = checks::test_connection(
        HealthClient::with_root_certificate(health.origin.clone(), health.root.clone()),
        Ok(metadata.client()),
    )
    .await;
    assert_eq!(result.health.error, Some(ErrorCode::Http));
    assert_eq!(result.uploader.status, TokenStatus::Verified);
    let health = Fixture::new(vec![Reply::json(200, br#"{"ok":true}"#)]).await;
    let metadata = Fixture::new(vec![Reply::json(401, "TOKEN-PRIVATE-CANARY")]).await;
    let result = checks::test_connection(
        HealthClient::with_root_certificate(health.origin.clone(), health.root.clone()),
        Ok(metadata.client()),
    )
    .await;
    assert_eq!(result.health.availability, Availability::Available);
    assert_eq!(result.uploader.error, Some(ErrorCode::Unauthorized));
    let snapshot = DiagnosticSnapshot {
        connection: result,
        ..Default::default()
    };
    assert_private(&ReportPreview::new(&snapshot).unwrap());
}

#[tokio::test]
async fn health_requires_a_bounded_valid_success_document() {
    for body in [
        b"healthy".as_slice(),
        br#"{"ok":false}"#.as_slice(),
        br#"{"ok":"true"}"#.as_slice(),
        b"".as_slice(),
    ] {
        let f = Fixture::new(vec![Reply::json(200, body)]).await;
        let client = HealthClient::with_root_certificate(f.origin.clone(), f.root.clone()).unwrap();
        assert_eq!(
            checks::health_result(client.check().await).error,
            Some(ErrorCode::Protocol)
        );
    }
    let f = Fixture::new(vec![Reply::json(200, vec![b' '; 2 * 1024 * 1024])]).await;
    let client = HealthClient::with_root_certificate(f.origin.clone(), f.root.clone()).unwrap();
    assert_eq!(
        checks::health_result(client.check().await).error,
        Some(ErrorCode::Protocol)
    );
}

#[tokio::test]
async fn explicit_connection_cancel_releases_pending_probes() {
    let (reply, arrived, _release) = Reply::json(200, br#"{"ok":true}"#).blocked();
    let health = Fixture::new(vec![reply]).await;
    let client =
        HealthClient::with_root_certificate(health.origin.clone(), health.root.clone()).unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let stop = cancel.clone();
    let check = tokio::spawn(checks::test_connection_cancellable(
        Ok(client),
        Err(AppError::NeedsSetup("no token")),
        cancel,
    ));
    tokio::time::timeout(Duration::from_secs(3), arrived.notified())
        .await
        .unwrap();
    stop.cancel();
    let result = tokio::time::timeout(Duration::from_secs(1), check)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.health.error, Some(ErrorCode::Cancelled));
    assert_eq!(result.uploader.status, TokenStatus::NeedsSetup);
}

#[test]
fn storage_scan_refuses_reparse_points_and_keeps_other_totals() {
    let dir = tempfile::tempdir().unwrap();
    let r = roots(dir.path());
    fs::create_dir(&r.spool).unwrap();
    fs::write(r.spool.join("known.png"), b"abc").unwrap();
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.png"), vec![0u8; 100]).unwrap();
    let link = r.spool.join("junction");
    let status = std::process::Command::new("cmd")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&link)
        .arg(outside)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let summary = checks::scan_storage(&r)[0];
    assert_eq!((summary.files, summary.bytes), (1, 3));
    assert!(summary.partial);
    assert_eq!(summary.error, Some(ErrorCode::ReparsePoint));
    let root_scan = checks::scan_storage(&StorageRoots {
        spool: link.clone(),
        archive: None,
    })[0];
    assert_eq!(root_scan.availability, Availability::Unavailable);
    assert_eq!(root_scan.files, 0);
    fs::remove_dir(link).unwrap();
}
