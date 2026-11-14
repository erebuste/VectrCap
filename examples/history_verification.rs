//! Interactive production-shell fixture. No credentials; no network uploads.
//! cargo run --example history_verification [optional-local-mp4]
use std::{path::Path, sync::Arc};
use vectrcap::app::destinations::CredentialKey;
use vectrcap::vectr::client::UploadToken;
use vectrcap::{
    app::{
        desktop::DesktopRuntime,
        error::AppError,
        events::{AppCommand, Window},
        types::ArtifactKind,
    },
    persistence::{artifacts::ArtifactStore, history::History, settings::Settings},
    platform::{CredentialClientFactory, CredentialStore},
    ui::shell::Shell,
};
struct NoCredentials;
impl CredentialStore for NoCredentials {
    fn read(&self, _: &CredentialKey) -> Result<UploadToken, AppError> {
        Err(AppError::NeedsSetup(
            "synthetic verification fixture has no credentials",
        ))
    }
    fn write(&self, _: &CredentialKey, _: &UploadToken) -> Result<(), AppError> {
        Err(AppError::NeedsSetup("synthetic fixture is read-only"))
    }
    fn delete(&self, _: &CredentialKey) -> Result<(), AppError> {
        Ok(())
    }
}
fn seed(root: &Path) {
    let history = History::open(&root.join("history.db")).unwrap();
    drop(history);
    Settings::default()
        .save(&root.join("settings.json"))
        .unwrap();
    let spool = ArtifactStore::new(root.join("spool")).unwrap();
    let picture = root.join("Synthetic capture.png");
    let mut image = image::RgbaImage::new(480, 270);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        *pixel = image::Rgba([(x / 2) as u8, (y / 2) as u8, 140, 255]);
    }
    image.save(&picture).unwrap();
    let animation = root.join("Synthetic recording.gif");
    let mut gif = image::codecs::gif::GifEncoder::new(std::fs::File::create(&animation).unwrap());
    gif.encode_frame(image::Frame::new(image.clone())).unwrap();
    drop(gif);
    let text = root.join("Synthetic notes.txt");
    std::fs::write(&text, "VectrCap verified local document opening.").unwrap();
    let conn = rusqlite::Connection::open(root.join("history.db")).unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    for index in 0..60 {
        let (source, kind) = match index % 3 {
            0 => (&picture, ArtifactKind::Screenshot),
            1 => (&animation, ArtifactKind::Gif),
            _ => (&text, ArtifactKind::File),
        };
        let mut artifact = spool.stage_file(source, kind).unwrap();
        artifact.display_name = format!("{:02} {}", index, artifact.display_name);
        let tag = match kind {
            ArtifactKind::Screenshot => "screenshot",
            ArtifactKind::Gif => "gif",
            _ => "file",
        };
        let id = uuid::Uuid::new_v4();
        let date = now - (index as i64 / 15) * 86400000;
        conn.execute(
            "INSERT INTO artifacts VALUES(?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![
                artifact.id.0.to_string(),
                artifact.path.to_str().unwrap(),
                artifact.display_name,
                tag,
                artifact.size as i64,
                artifact.sha256.as_slice(),
                date
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO history_items VALUES(?1,?2,?3,?4)",
            rusqlite::params![id.to_string(), artifact.id.0.to_string(), index == 0, date],
        )
        .unwrap();
        if index % 4 != 3 {
            let status = if index % 4 == 0 {
                "completed"
            } else if index % 4 == 1 {
                "failed"
            } else {
                "cancelled"
            };
            conn.execute("INSERT INTO jobs(id,artifact_id,dest_id,dest_label,dest_origin,dest_credential_generation,status,failure_code,receipt_url,receipt_name,created_ms,updated_ms,acceptance_ordinal,attempt) VALUES(?1,?2,?3,'Synthetic destination','https://example.invalid',1,?4,?5,?6,?7,?8,?8,?9,1)",rusqlite::params![uuid::Uuid::new_v4().to_string(),artifact.id.0.to_string(),uuid::Uuid::new_v4().to_string(),status,if status=="failed"{Some("transient")}else{None},if status=="completed"{Some(format!("https://example.invalid/synthetic/{index}"))}else{None},if status=="completed"{Some(artifact.display_name.clone())}else{None},date,index+1]).unwrap();
        }
        if index == 4 {
            std::fs::remove_file(&artifact.path).unwrap();
        }
    }
    if let Some(path) = std::env::args_os().nth(1) {
        let artifact = spool
            .stage_file(Path::new(&path), ArtifactKind::Mp4)
            .unwrap();
        let id = uuid::Uuid::new_v4();
        conn.execute(
            "INSERT INTO artifacts VALUES(?1,?2,'MP4 poster verification.mp4','mp4',?3,?4,?5)",
            rusqlite::params![
                artifact.id.0.to_string(),
                artifact.path.to_str().unwrap(),
                artifact.size as i64,
                artifact.sha256.as_slice(),
                now + 1
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO history_items VALUES(?1,?2,0,?3)",
            rusqlite::params![id.to_string(), artifact.id.0.to_string(), now + 1],
        )
        .unwrap();
    }
    conn.execute("UPDATE meta SET value=1000 WHERE key='upload_ordinal'", [])
        .unwrap();
}
fn main() -> eframe::Result {
    let root = tempfile::tempdir().unwrap();
    if std::env::args_os()
        .nth(1)
        .is_some_and(|s| s == "--poster-check")
    {
        let source = std::env::args_os()
            .nth(2)
            .expect("provide an absolute local MP4 path");
        let store = ArtifactStore::new(root.path().join("spool")).unwrap();
        let artifact = store
            .stage_file(Path::new(&source), ArtifactKind::Mp4)
            .unwrap();
        let row = vectrcap::persistence::history::browser::HistoryRecord {
            id: uuid::Uuid::new_v4(),
            name: "poster.mp4".into(),
            kind: "mp4".into(),
            size: artifact.size,
            created_ms: 0,
            favorite: false,
            local_available: true,
            retryable: false,
            artifact,
            job: None,
        };
        let poster = vectrcap::ui::history_previews::decode(&row)
            .expect("native MP4 poster extraction failed");
        assert!(
            poster.size[0] > 0
                && poster.size[0] <= 512
                && poster.size[1] > 0
                && poster.size[1] <= 512
        );
        println!(
            "Native MP4 poster verified: {} x {}",
            poster.size[0], poster.size[1]
        );
        return Ok(());
    }
    seed(root.path());
    let mut runtime = None;
    eframe::run_native(
        "VectrCap History verification",
        eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default().with_inner_size([1050.0, 900.0]),
            renderer: eframe::Renderer::Glow,
            ..Default::default()
        },
        Box::new(|cc| {
            let store = Arc::new(NoCredentials);
            let factory = Arc::new(CredentialClientFactory::new(store.clone()));
            let ctx = cc.egui_ctx.clone();
            let worker = DesktopRuntime::spawn(
                root.path().to_path_buf(),
                store,
                factory,
                Arc::new(move || ctx.request_repaint()),
            )?;
            worker.client.submit(AppCommand::Show(Window::History))?;
            let shutdown = worker.client.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(90));
                let _ = shutdown.submit(AppCommand::Quit);
            });
            let shell = Shell::new(
                worker.client.clone(),
                Default::default(),
                cc.egui_ctx.clone(),
            )?;
            runtime = Some(worker);
            Ok(Box::new(shell))
        }),
    )?;
    drop(runtime);
    Ok(())
}
