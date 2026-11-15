//! Native local-only probe: cargo +stable run --example capture_overlay_verification --offline.
//! Acquires one fresh desktop snapshot before creating any probe window. Uses
//! the real overlay/event loop/finish pane, but deliberately executes no output
//! effects, settings writes, uploads, archive writes, OS Copy or Pin operations.
use eframe::egui;
use vectrcap::{
    app::{error::AppError, shortcuts::ShortcutSettings},
    capture::request::{CaptureMode, CaptureRequest, CaptureService},
    platform::windows::{
        appearance,
        capture::{NativeCountdown, WindowsCaptureSource},
        monitors::{DpiContext, system_drag_threshold_at},
    },
    ui::capture_overlay::{CaptureOverlay, CaptureOverlayEvent},
};

struct Probe {
    overlay: CaptureOverlay,
    shortcuts: ShortcutSettings,
    zoom: f32,
    last_view: Option<String>,
}
impl eframe::App for Probe {
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        ctx.set_visuals(appearance::visuals());
        ctx.set_zoom_factor(self.zoom);
        let geometry = ctx.input(|i| {
            format!(
                "Native geometry: {:?}; ppp {}; source {:?}",
                i.viewport().inner_rect,
                ctx.pixels_per_point(),
                self.overlay.document().bounds()
            )
        });
        if self.last_view.as_ref() != Some(&geometry) {
            println!("{geometry}");
            self.last_view = Some(geometry);
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        egui::Window::new("Native verification probe")
            .default_pos([900.0, 12.0])
            .resizable(false)
            .show(ui.ctx(), |ui| {
                ui.label("Fresh frozen desktop; memory-only output verification.");
                ui.label("Production archive / OS Copy / Pin are not connected here.");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.zoom, 1.0, "100% chrome");
                    ui.selectable_value(&mut self.zoom, 2.0, "200% chrome");
                });
                if let Some(crop) = self.overlay.document().crop() {
                    ui.label(format!(
                        "Physical crop: {}, {} · {} × {}",
                        crop.left(),
                        crop.top(),
                        crop.width(),
                        crop.height()
                    ));
                }
            });
        match self.overlay.show(ui, &self.shortcuts) {
            Some(CaptureOverlayEvent::Cancel) => {
                println!("Native overlay cancelled without output effects.");
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Some(CaptureOverlayEvent::Finish(finish)) => {
                println!(
                    "Validated {:?}: {} × {} · {} · {} bytes; no effects executed.",
                    finish.action,
                    finish.prepared.dimensions().0,
                    finish.prepared.dimensions().1,
                    finish.prepared.extension(),
                    finish.prepared.encoded_size()
                );
                self.overlay.finish_failed(AppError::InUse("verification probe received the prepared output; production output effects are not connected in this probe"));
            }
            None => {}
        }
    }
    fn persist_egui_memory(&self) -> bool {
        false
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _dpi = DpiContext::enter()?;
    let request = CaptureRequest::new(CaptureMode::Region, 0, false, Default::default())?;
    let snapshot = CaptureService::new(
        WindowsCaptureSource::new(),
        NativeCountdown::new(|| Ok(())),
        tokio_util::sync::CancellationToken::new(),
    )
    .snapshot(request)?;
    let pointer = snapshot
        .launch_pointer()
        .ok_or(AppError::Capture("snapshot has no launch pointer"))?;
    let scale = snapshot
        .monitors()
        .iter()
        .find(|m| m.bounds().contains(pointer))
        .ok_or(AppError::Capture("launch monitor is unavailable"))?
        .scale_factor() as f32;
    let threshold = system_drag_threshold_at(pointer)?;
    let overlay = CaptureOverlay::from_snapshot(snapshot, pointer, threshold)?;
    let viewport = overlay.viewport_builder(scale)?;
    eframe::run_native(
        "VectrCap capture",
        eframe::NativeOptions {
            viewport,
            renderer: eframe::Renderer::Glow,
            ..Default::default()
        },
        Box::new(move |cc| {
            appearance::install_fonts(&cc.egui_ctx);
            Ok(Box::new(Probe {
                overlay,
                shortcuts: Default::default(),
                zoom: 1.0,
                last_view: None,
            }))
        }),
    )?;
    Ok(())
}
