//! Renders the real VectrCap main window from fixture data to a PNG, for side-by-side
//! comparison with docs/mockups/2026-11-17-main-window.html. No runtime, network or tray.
//!
//! cargo run --example ui_snapshot -- <variant> <width> <height> <out.png>
//! variants: history, history-empty, history-paused, settings-<Section> (e.g. settings-Connections)
use eframe::egui;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use vectrcap::ui::{
    fixtures::{MockupVariant, mockup_view, write_mockup_media},
    settings::SettingsCategory,
    shell::Shell,
};

fn variant(name: &str) -> Option<MockupVariant> {
    match name {
        "history" => Some(MockupVariant::HistoryBusy),
        "history-empty" => Some(MockupVariant::HistoryEmpty),
        "history-paused" => Some(MockupVariant::HistoryPaused),
        _ => name
            .strip_prefix("settings-")
            .and_then(|section| {
                SettingsCategory::ALL
                    .into_iter()
                    .find(|c| format!("{c:?}") == section)
            })
            .map(MockupVariant::Settings),
    }
}

struct Snapshot {
    shell: Shell,
    out: PathBuf,
    frames: u32,
    saved: Arc<AtomicBool>,
}

impl eframe::App for Snapshot {
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        self.shell.clear_color(visuals)
    }
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // The shell cancels window closes (it hides to the tray), so stop routing
        // frames through it once the image is saved.
        if self.saved.load(Ordering::Acquire) {
            println!("pixels_per_point={}", ctx.pixels_per_point());
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        self.shell.logic(ctx, frame);
        self.frames += 1;
        // ~1.5 s at 60 Hz lets fonts, layout and the preview worker settle.
        if self.frames == 90 {
            let out = self.out.clone();
            let saved = self.saved.clone();
            let repaint = ctx.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                egui::ScreenshotCallback::new(move |image| {
                    let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                    image::save_buffer(
                        &out,
                        &bytes,
                        image.width() as u32,
                        image.height() as u32,
                        image::ExtendedColorType::Rgba8,
                    )
                    .expect("write snapshot PNG");
                    saved.store(true, Ordering::Release);
                    repaint.request_repaint();
                }),
            ));
        }
        ctx.request_repaint();
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if !self.saved.load(Ordering::Acquire) {
            self.shell.ui(ui, frame);
        }
    }
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (name, width, height, out, zoom) = match args.as_slice() {
        [name, width, height, out] => (name, width, height, out, 1.0),
        [name, width, height, out, zoom] => {
            (name, width, height, out, zoom.parse::<f32>().expect("zoom"))
        }
        _ => {
            eprintln!("usage: ui_snapshot <variant> <width> <height> <out.png> [zoom]");
            std::process::exit(2);
        }
    };
    let Some(variant) = variant(name) else {
        eprintln!("unknown variant {name}");
        std::process::exit(2);
    };
    write_mockup_media().expect("write fixture media");
    let size = [
        width.parse::<f32>().expect("width"),
        height.parse::<f32>().expect("height"),
    ];
    let out = PathBuf::from(out);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(size)
            .with_resizable(false),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "VectrCap snapshot",
        options,
        Box::new(move |cc| {
            // The Shell applies the UI scale setting each frame, so set it there.
            let mut view = mockup_view(variant);
            if let Some(settings) = view.settings.as_mut() {
                settings.appearance.ui_scale_percent = (zoom * 100.0).round() as u16;
            }
            let mut shell = Shell::from_fixture(view, cc.egui_ctx.clone());
            if let MockupVariant::Settings(category) = variant {
                shell.select_settings_category(category);
            }
            Ok(Box::new(Snapshot {
                shell,
                out,
                frames: 0,
                saved: Arc::new(AtomicBool::new(false)),
            }))
        }),
    )
}
