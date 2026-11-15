//! Generated-window native proof. No desktop capture, device acquisition or uploads.
//! cargo run --example recording_probe -- --case mp4-both --seconds 5 --pause
use eframe::egui;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
use vectrcap::{
    capture::geometry::PhysicalRect,
    recording::{options::*, worker::*},
};
const TITLE: &str = "VectrCap GENERATED recording proof - no personal content";
struct Pattern {
    done: Arc<AtomicBool>,
    media_time: Arc<AtomicU64>,
}
impl eframe::App for Pattern {
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        if self.done.load(Ordering::Acquire) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(16));
    }
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let p = ui.painter();
        let scale = ui.ctx().pixels_per_point();
        let rect = |x: f32, y: f32, w: f32, h: f32| {
            egui::Rect::from_min_size(
                egui::pos2(x / scale, y / scale),
                egui::vec2(w / scale, h / scale),
            )
        };
        p.rect_filled(ui.max_rect(), 0.0, egui::Color32::from_rgb(255, 0, 255));
        p.rect_filled(
            rect(31.0, 17.0, 641.0, 359.0),
            0.0,
            egui::Color32::from_rgb(16, 80, 128),
        );
        p.rect_filled(
            rect(31.0, 17.0, 80.0, 80.0),
            0.0,
            egui::Color32::from_rgb(230, 230, 230),
        );
        p.rect_filled(
            rect(592.0, 296.0, 80.0, 80.0),
            0.0,
            egui::Color32::from_rgb(20, 210, 70),
        );
        // Alternate moving and stationary periods; magenta is exclusively outside selection.
        let t = self.media_time.load(Ordering::Acquire) as f32 / 1_000_000_000.0;
        let x = if (t as u32 / 2).is_multiple_of(2) {
            150.0 + (t * 100.0) % 300.0
        } else {
            300.0
        };
        p.rect_filled(
            rect(x, 160.0, 30.0, 60.0),
            0.0,
            egui::Color32::from_rgb(240, 190, 30),
        );
        p.rect_filled(
            rect(231.0, 267.0, 60.0, 40.0),
            0.0,
            if t.fract() < 0.1 {
                egui::Color32::WHITE
            } else {
                egui::Color32::BLACK
            },
        );
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let case = value("--case").unwrap_or_else(|| "gif".into());
    if !["gif", "mp4-off", "mp4-system", "mp4-microphone", "mp4-both"].contains(&case.as_str()) {
        return Err("unknown proof case (all audio cases use synthetic PCM)".into());
    }
    let seconds: u32 = value("--seconds").unwrap_or_else(|| "5".into()).parse()?;
    let pause = args.iter().any(|a| a == "--pause");
    let cancel = args.iter().any(|a| a == "--cancel");
    let slow: u64 = value("--slow-ms").unwrap_or_else(|| "0".into()).parse()?;
    let root = std::env::current_dir()?.join("target/recording-probe");
    std::fs::create_dir_all(&root)?;
    let mut options = RecordingOptions::new(
        "generated-window-only".into(),
        PhysicalRect::new(31, 17, 641, 359)?,
        case != "gif",
    );
    options.cursor = false;
    options.countdown_seconds = 0;
    options.timed_stop_seconds = Some(seconds);
    if let FormatOptions::Mp4(m) = &mut options.format {
        m.audio.mode = match case.as_str() {
            "mp4-system" => AudioMode::System,
            "mp4-microphone" => AudioMode::Microphone,
            "mp4-both" => AudioMode::Both,
            _ => AudioMode::Off,
        };
    }
    if let Some(fps) = value("--fps") {
        let fps = fps.parse()?;
        match &mut options.format {
            FormatOptions::Gif(g) => g.fps = fps,
            FormatOptions::Mp4(m) => m.fps = fps,
        }
    }
    if let Some(q) = value("--quality") {
        let FormatOptions::Mp4(m) = &mut options.format else {
            return Err("quality is MP4 only".into());
        };
        m.quality = match q.as_str() {
            "small" => Quality::Small,
            "balanced" => Quality::Balanced,
            "high" => Quality::High,
            _ => return Err("unknown quality".into()),
        };
    }
    if let Some(size) = value("--size") {
        options.output_size = match size.as_str() {
            "original" => OutputSize::Original,
            "720" => OutputSize::Fit1280x720,
            "1080" => OutputSize::Fit1920x1080,
            _ => return Err("unknown size".into()),
        };
    }
    options.validate()?;
    let done = Arc::new(AtomicBool::new(false));
    let result = Arc::new(Mutex::new(None));
    let d = done.clone();
    let r = result.clone();
    let media_time = Arc::new(AtomicU64::new(0));
    let worker_time = media_time.clone();
    let worker = std::thread::spawn(move || {
        let result = run_case(case, root, options, pause, cancel, slow, worker_time);
        *r.lock().unwrap() = Some(result);
        d.store(true, Ordering::Release);
    });
    eframe::run_native(
        TITLE,
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([800.0, 500.0])
                .with_decorations(false)
                .with_resizable(false),
            renderer: eframe::Renderer::Glow,
            ..Default::default()
        },
        Box::new(move |_| Ok(Box::new(Pattern { done, media_time }))),
    )?;
    worker.join().map_err(|_| "proof thread panicked")?;
    result
        .lock()
        .unwrap()
        .take()
        .ok_or("missing proof result")??;
    Ok(())
}
fn run_case(
    case: String,
    root: PathBuf,
    options: RecordingOptions,
    pause: bool,
    cancel: bool,
    slow: u64,
    media_time: Arc<AtomicU64>,
) -> Result<(), String> {
    std::thread::sleep(Duration::from_secs(2));
    let (tx, rx) = mpsc::sync_channel(32);
    let token = CancellationToken::new();
    let handle = RecordingWorker::start_probe(
        options,
        token,
        tx,
        ProbeSettings {
            window_title: TITLE.into(),
            output_root: root.clone(),
            synthetic_audio: true,
            encoder_delay: Duration::from_millis(slow),
            media_time,
        },
    )
    .map_err(|e| e.to_string())?;
    let mut paused = false;
    let mut recording_start = None;
    let mut paused_at = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(event) => {
                println!("event={event:?}");
                match event {
                    RecordingEvent::Started => recording_start = Some(Instant::now()),
                    RecordingEvent::Paused(_) => paused_at = Some(Instant::now()),
                    RecordingEvent::Finalizing => break,
                    _ => {}
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if recording_start.is_some_and(|t| t.elapsed() >= Duration::from_secs(2)) && !paused {
            if cancel {
                handle
                    .control(RecordingControl::Cancel)
                    .map_err(|e| e.to_string())?;
                handle
                    .control(RecordingControl::Cancel)
                    .map_err(|e| e.to_string())?;
                break;
            }
            if pause {
                handle
                    .control(RecordingControl::Pause)
                    .map_err(|e| e.to_string())?;
                handle
                    .control(RecordingControl::Pause)
                    .map_err(|e| e.to_string())?;
            }
            paused = true;
        }
        if paused_at.is_some_and(|t| t.elapsed() >= Duration::from_secs(10)) {
            handle
                .control(RecordingControl::Resume)
                .map_err(|e| e.to_string())?;
            handle
                .control(RecordingControl::Resume)
                .map_err(|e| e.to_string())?;
            paused_at = None;
        }
    }
    handle
        .control(RecordingControl::Stop)
        .map_err(|e| e.to_string())?;
    handle
        .control(RecordingControl::Stop)
        .map_err(|e| e.to_string())?;
    let outcome = handle.finish();
    if cancel {
        if matches!(outcome, Err(vectrcap::app::error::AppError::Cancelled)) {
            println!("cancel=discarded");
            return Ok(());
        }
        return Err("cancel unexpectedly published output".into());
    }
    let out = outcome.map_err(|e| e.to_string())?;
    let ext = out.path.extension().unwrap().to_str().unwrap();
    let destination = root.join(format!("{case}-{}.{}", uuid::Uuid::new_v4(), ext));
    let mut dest = std::fs::File::create_new(&destination).map_err(|e| e.to_string())?;
    let mut source = std::fs::File::open(&out.path).map_err(|e| e.to_string())?;
    std::io::copy(&mut source, &mut dest).map_err(|e| e.to_string())?;
    dest.sync_all().map_err(|e| e.to_string())?;
    println!(
        "output={} dimensions={}x{} active={:.6} fps={} audio={:?}",
        destination.display(),
        out.width,
        out.height,
        out.active_duration.as_secs_f64(),
        out.options.fps(),
        out.options.audio().map(|a| a.mode)
    );
    Ok(())
}
