//! Native surface hosting. Rendering never acquires source pixels or writes
//! artifacts; all finish/cancel decisions go back through the controller.
use crate::{
    app::{
        desktop::{DesktopClient, DesktopView},
        events::{AppCommand, Lifecycle},
        image_finish::ArchiveDecision,
    },
    ui::capture_overlay::{CaptureOverlayEvent, OverlayAction},
};
use eframe::egui;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Default)]
pub struct CaptureHost {
    pin_views: HashMap<crate::pin::PinId, Arc<Mutex<super::image_review::ImageViewport>>>,
    capture_builder: Option<(uuid::Uuid, egui::ViewportBuilder)>,
}
impl CaptureHost {
    pub fn show(&mut self, ctx: &egui::Context, client: &DesktopClient, view: &DesktopView) {
        if view.capture_surface.is_none() {
            self.capture_builder = None;
        }
        if let Some(surface) = &view.capture_surface {
            if let Ok(state) = surface.try_lock() {
                match state.overlay.viewport_builder(state.native_scale) {
                    Ok(builder) => self.capture_builder = Some((state.session, builder)),
                    Err(error) => client.report_error(error),
                }
            }
            let Some((session, builder)) = self.capture_builder.clone() else {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
                return;
            };
            {
                let client = client.clone();
                let surface = surface.clone();
                ctx.show_viewport_deferred(
                    egui::ViewportId::from_hash_of(("capture", session)),
                    builder,
                    move |ui, _| {
                        let view = client.snapshot();
                        if view
                            .capture_surface
                            .as_ref()
                            .is_none_or(|live| !Arc::ptr_eq(live, &surface))
                            || view.lifecycle != Lifecycle::Running
                        {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                            return;
                        }
                        let Ok(mut state) = surface.try_lock() else {
                            ui.ctx()
                                .request_repaint_after(std::time::Duration::from_millis(50));
                            return;
                        };
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::Visible(!state.dialog_open));
                        if state.dialog_open {
                            return;
                        }
                        let mut command = None;
                        if state.intended_pin { ui.label("Pin from screen: select and annotate, then choose Pin. Copy, Save and Upload remain available."); }
                        let mut choose_folder = false;
                        if state.pending_finish && !state.working {
                            egui::Window::new("Finish recovery").show(ui.ctx(), |ui| {
                                ui.label("The editor and prepared output are retained.");
                                if ui.button("Retry finish").clicked() {
                                    command = Some(AppCommand::RetryCaptureFinish {
                                        session,
                                        decision: ArchiveDecision::Retry,
                                    });
                                }
                                if state.archive_failed {
                                    choose_folder = ui.button("Choose archive folder…").clicked();
                                    if ui.button("Continue without local copy").clicked() {
                                        command = Some(AppCommand::RetryCaptureFinish {
                                            session,
                                            decision: ArchiveDecision::ContinueWithoutLocalCopy,
                                        });
                                    }
                                }
                            });
                        }
                        let event = if ui.ctx().input(|i| i.viewport().close_requested()) {
                            Some(CaptureOverlayEvent::Cancel)
                        } else {
                            view.settings
                                .as_ref()
                                .and_then(|settings| state.overlay.show(ui, &settings.shortcuts))
                        };
                        let mut save = None;
                        match event {
                            Some(CaptureOverlayEvent::Cancel) => {
                                command = Some(AppCommand::CancelCapture(session))
                            }
                            Some(CaptureOverlayEvent::Finish(finish))
                                if finish.action == OverlayAction::Save =>
                            {
                                save = Some(finish)
                            }
                            Some(CaptureOverlayEvent::Finish(finish)) => {
                                command = Some(AppCommand::FinishCapture {
                                    session,
                                    finish,
                                    save_path: None,
                                })
                            }
                            None => {}
                        }
                        if command.is_some() || save.is_some() {
                            state.working = true;
                        }
                        drop(state);
                        if choose_folder && let Some(settings) = &view.settings {
                            crate::platform::windows::file_dialog::spawn_capture_archive_folder(
                                client.clone(),
                                surface.clone(),
                                settings.archive.clone(),
                                settings.revision,
                            );
                        }
                        if let Some(finish) = save {
                            crate::platform::windows::file_dialog::spawn_capture_save(
                                client.clone(),
                                surface.clone(),
                                finish,
                            );
                        }
                        if let Some(command) = command
                            && let Err(error) = client.submit(command)
                        {
                            if let Ok(mut state) = surface.lock() {
                                state.working = false;
                                state.overlay.finish_failed(error.clone());
                            }
                            client.report_error(error);
                        }
                    },
                );
            }
        }
        // Task 6 hands off only immutable final pixels and optional placement.
        // This minimal host keeps that handoff visible; Task 7 owns pin tools.
        self.pin_views
            .retain(|id, _| view.pins.iter().any(|p| p.id == *id));
        if view.capture_acquiring.is_none() && view.capture_surface.is_none() {
            for pin in &view.pins {
                let viewport = self.pin_views.entry(pin.id).or_default().clone();
                let client = client.clone();
                let pin = pin.clone();
                ctx.show_viewport_deferred(
                    egui::ViewportId::from_hash_of(("pin", pin.id)),
                    egui::ViewportBuilder::default()
                        .with_title("VectrCap pin")
                        .with_inner_size([600.0, 450.0])
                        .with_window_level(egui::WindowLevel::AlwaysOnTop),
                    move |ui, _| {
                        let live = client.snapshot();
                        if !live.pins.iter().any(|p| p.id == pin.id)
                            || live.capture_acquiring.is_some()
                            || live.capture_surface.is_some()
                        {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                            return;
                        }
                        if ui.ctx().input(|i| i.viewport().close_requested()) {
                            if let Err(error) = client.submit(AppCommand::Pin {
                                id: pin.id,
                                input: crate::pin::PinInput::Close,
                            }) {
                                client.report_error(error);
                            }
                        } else if let Ok(mut viewport) = viewport.lock()
                            && let Err(error) = viewport.show(ui, pin.image.pixels())
                        {
                            client.report_error(error);
                        }
                    },
                );
            }
        }
    }
}
