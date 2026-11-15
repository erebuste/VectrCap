//! Selection and controls; never acquires pixels or simulates scrolling.
use crate::{
    app::{
        desktop::{DesktopClient, DesktopView},
        events::AppCommand,
    },
    scrolling::{runtime::ScrollControl, session::ScrollState},
};
use eframe::egui;
use std::sync::Arc;
#[derive(Default)]
pub struct ScrollingHost {
    selection_id: Option<uuid::Uuid>,
}
impl ScrollingHost {
    pub fn show(&mut self, ctx: &egui::Context, client: &DesktopClient, view: &DesktopView) {
        let active = view
            .scrolling
            .as_ref()
            .filter(|v| v.selection.is_some())
            .map(|v| v.id);
        if self.selection_id != active
            && let Some(id) = self.selection_id.take()
        {
            let title = format!("VectrCap scrolling selection {id}");
            match crate::platform::windows::recording_events::hide_selection(&title) {
                Ok(()) => {
                    ctx.send_viewport_cmd_to(
                        egui::ViewportId::from_hash_of(("scroll-selection", id)),
                        egui::ViewportCommand::Close,
                    );
                    if let Err(e) = client.submit(AppCommand::ScrollingSelectionClosed(id)) {
                        client.report_error(e);
                    }
                }
                Err(e) => {
                    client.report_error(e);
                    let _ = client.submit(AppCommand::ScrollingControl(ScrollControl::Cancel));
                }
            }
        }
        let Some(scroll) = &view.scrolling else {
            return;
        };
        let id = scroll.id;
        if let Some(surface) = &scroll.selection {
            self.selection_id = Some(id);
            let builder = surface
                .try_lock()
                .ok()
                .and_then(|s| s.overlay.viewport_builder(s.scale).ok());
            if let Some(builder) = builder {
                let surface = surface.clone();
                let client = client.clone();
                ctx.show_viewport_deferred(
                    egui::ViewportId::from_hash_of(("scroll-selection", id)),
                    builder.with_title(format!("VectrCap scrolling selection {id}")),
                    move |ui, _| {
                        let view = client.snapshot();
                        if view
                            .scrolling
                            .as_ref()
                            .and_then(|v| v.selection.as_ref())
                            .is_none_or(|s| !Arc::ptr_eq(s, &surface))
                        {
                            match crate::platform::windows::recording_events::hide_selection(
                                &format!("VectrCap scrolling selection {id}"),
                            ) {
                                Ok(()) => {
                                    let _ = client.submit(AppCommand::ScrollingSelectionClosed(id));
                                }
                                Err(e) => client.report_error(e),
                            }
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                            return;
                        }
                        let Ok(mut s) = surface.try_lock() else {
                            return;
                        };
                        let event = if ui.ctx().input(|i| i.viewport().close_requested()) {
                            Some(super::capture_overlay::CaptureOverlayEvent::Cancel)
                        } else {
                            view.settings
                                .as_ref()
                                .and_then(|settings| s.overlay.show(ui, &settings.shortcuts))
                        };
                        let command = match event {
                            Some(
                                super::capture_overlay::CaptureOverlayEvent::RecordingSelected(
                                    rect,
                                ),
                            ) => Some(AppCommand::ScrollingSelection { session: id, rect }),
                            Some(super::capture_overlay::CaptureOverlayEvent::Cancel) => {
                                Some(AppCommand::ScrollingControl(ScrollControl::Cancel))
                            }
                            _ => None,
                        };
                        drop(s);
                        if let Some(command) = command
                            && let Err(e) = client.submit(command)
                        {
                            client.report_error(e);
                        }
                    },
                );
            }
            return;
        }
        if let Some(bounds) = scroll.controls {
            let title = format!("VectrCap scrolling controls {id}");
            let _ = crate::platform::windows::recording_events::place_overlay(&title, bounds);
            let client = client.clone();
            ctx.show_viewport_deferred(egui::ViewportId::from_hash_of(("scroll-controls",id)),egui::ViewportBuilder::default().with_title(title.clone()).with_decorations(false).with_resizable(false).with_taskbar(false).with_visible(false).with_window_level(egui::WindowLevel::AlwaysOnTop).with_inner_size([bounds.width()as f32,bounds.height()as f32]).with_clamp_size_to_monitor_size(false),move|ui,_|{
                let view=client.snapshot();let Some(scroll)=view.scrolling.filter(|v|v.id==id)else{ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);return;};
                if let Err(e)=crate::platform::windows::recording_events::place_overlay(&title,bounds){client.report_error(e);let _=client.submit(AppCommand::ScrollingControl(ScrollControl::Stop));return;}
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Visible(true));
                if let Some(update)=scroll.update{
                    ui.heading(if update.state==ScrollState::Countdown{format!("Starting in {:.1}s",update.countdown.as_secs_f32())}else{format!("Manual scrolling: {:?}",update.state)});
                    ui.label(format!("{} accepted segments · {:?} pixels",update.accepted_segments,update.dimensions));
                    if let Some(reason)=update.partial_reason{ui.label(reason);}
                    ui.label("Wait for the first accepted segment, then scroll the original window slowly. Resume, then focus that window, to continue.");
                    ui.horizontal_wrapped(|ui|{
                        for (label,control) in [(if update.state==ScrollState::Paused{"Resume"}else{"Pause"},if update.state==ScrollState::Paused{ScrollControl::Resume}else{ScrollControl::Pause}),("Stop and review",ScrollControl::Stop),("Cancel",ScrollControl::Cancel)]{
                            if ui.button(label).clicked() && let Err(e)=client.submit(AppCommand::ScrollingControl(control)){client.report_error(e);}
                        }
                    });
                }else{ui.label("Preparing manual scrolling capture…");if ui.button("Cancel").clicked(){let _=client.submit(AppCommand::ScrollingControl(ScrollControl::Cancel));}}
                if ui.ctx().input(|i|i.viewport().close_requested()){ui.ctx().send_viewport_cmd(egui::ViewportCommand::CancelClose);let _=client.submit(AppCommand::ScrollingControl(ScrollControl::Stop));}
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            });
        }
    }
}
