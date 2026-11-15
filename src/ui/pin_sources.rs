//! Source chooser only dispatches typed local actions; it never decodes images.
use crate::app::{
    desktop::{DesktopClient, DesktopView},
    events::{AppCommand, Lifecycle},
};
use crate::pin::source::PinSource;
use eframe::egui;

#[derive(Default)]
pub struct PinSources {
    picker_requests: u64,
}
impl PinSources {
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        client: &DesktopClient,
        view: &DesktopView,
        gate: &crate::platform::windows::file_dialog::PickerGate,
    ) {
        if self.picker_requests != view.pin_picker_requests {
            self.picker_requests = view.pin_picker_requests;
            crate::platform::windows::file_dialog::spawn_pick_pin_file(
                client.clone(),
                gate.clone(),
            );
        }
        if !view.pin_source_chooser || view.lifecycle != Lifecycle::Running {
            return;
        }
        let client = client.clone();
        ctx.show_viewport_deferred(egui::ViewportId::from_hash_of("pin-sources"), egui::ViewportBuilder::default().with_title("Pin to screen").with_inner_size([400.0, 230.0]).with_resizable(false).with_window_level(egui::WindowLevel::AlwaysOnTop), move |ui, _| {
            let live = client.snapshot();
            if !live.pin_source_chooser || live.lifecycle != Lifecycle::Running {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            ui.heading("Pin to screen");
            ui.label("Choose a reference image. GIFs show their first frame.");
            for (label, source) in [("From screen", PinSource::Screen), ("From clipboard image", PinSource::Clipboard), ("From image file…", PinSource::File)] {
                if ui.button(label).clicked() && let Err(error) = client.submit(AppCommand::PinSource(source)) { client.report_error(error); }
            }
            ui.small("Clipboard text and URLs cannot be pinned. Use History → Pin for a local image.");
            if ui.ctx().input(|i| i.viewport().close_requested() || i.key_pressed(egui::Key::Escape))
                && let Err(error) = client.submit(AppCommand::DismissPinSourceChooser) { client.report_error(error); }
        });
    }
}
