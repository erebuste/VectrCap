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
use std::sync::Arc;

#[derive(Default)]
pub struct CaptureHost {
    pin_host: super::pin_window::PinHost,
    capture_builder: Option<(uuid::Uuid, egui::ViewportBuilder)>,
}
impl CaptureHost {
    pub fn show(&mut self, ctx: &egui::Context, client: &DesktopClient, view: &DesktopView) {
        if view.capture_surface.is_none() {
            self.capture_builder = None;
        }
        if let Some(surface) = &view.capture_surface {
            if let Ok(state) = surface.try_lock() {
                match if state.image_local || state.scrolling_review.is_some() {
                    Ok(egui::ViewportBuilder::default()
                        .with_title("VectrCap image review")
                        .with_inner_size([1000.0, 800.0]))
                } else {
                    state.overlay.viewport_builder(state.native_scale)
                } {
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
                        ui.label(&state.task_target);
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
                        if let Some(status)=&mut state.scrolling_review {
                            ui.label(format!("Scrolling capture: {} accepted segments",status.segments));
                            if let Some(reason)=status.reason {
                                ui.label(format!("Partial result: {reason}"));
                                if !status.accepted && ui.button("Accept reliable partial result").clicked(){status.accepted=true;}
                            }
                        }
                        let partial_blocked=state.scrolling_review.as_ref().is_some_and(|r|!r.accepted);
                        let event = if ui.ctx().input(|i| i.viewport().close_requested()) {
                            Some(CaptureOverlayEvent::Cancel)
                        } else {
                            view.settings.as_ref().and_then(|settings| {
                                if state.image_local || state.scrolling_review.is_some(){
                                    let super::super::app::controller::CaptureSurface{overlay,review_viewport,..}=&mut *state;
                                    ui.add_enabled_ui(!partial_blocked,|ui|overlay.show_review(ui,&settings.shortcuts,review_viewport)).inner
                                }else{state.overlay.show(ui,&settings.shortcuts)}
                            })
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
                            Some(CaptureOverlayEvent::RecordingSelected(_)) => {},
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
        self.pin_host.show(ctx, client, view);
    }
}
