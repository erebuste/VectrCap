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
            crate::platform::windows::recording_events::never_activate(&title);
            let client = client.clone();
            ctx.show_viewport_deferred(
                egui::ViewportId::from_hash_of(("scroll-controls", id)),
                egui::ViewportBuilder::default()
                    .with_title(title.clone())
                    .with_decorations(false)
                    .with_resizable(false)
                    .with_taskbar(false)
                    .with_visible(false)
                    // Showing (or clicking) the panel must not take focus from
                    // the window being scrolled: any focus change pauses capture.
                    .with_active(false)
                    .with_window_level(egui::WindowLevel::AlwaysOnTop)
                    .with_inner_size([bounds.width() as f32, bounds.height() as f32])
                    .with_clamp_size_to_monitor_size(false),
                move |ui, _| {
                    let view = client.snapshot();
                    let Some(scroll) = view.scrolling.filter(|v| v.id == id) else {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        return;
                    };
                    if let Err(e) =
                        crate::platform::windows::recording_events::place_overlay(&title, bounds)
                    {
                        client.report_error(e);
                        let _ = client.submit(AppCommand::ScrollingControl(ScrollControl::Stop));
                        return;
                    }
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    let send = |control| {
                        if let Err(e) = client.submit(AppCommand::ScrollingControl(control)) {
                            client.report_error(e);
                        }
                    };
                    if let Some(update) = scroll.update {
                        let (heading, hint) = status(
                            update.state,
                            update.accepted_segments,
                            update.dimensions.map(|(_, h)| h),
                            update.countdown,
                        );
                        ui.heading(heading);
                        ui.label(hint);
                        if update.state == ScrollState::Paused
                            && let Some(reason) = update.partial_reason
                        {
                            ui.weak(reason);
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            let paused = update.state == ScrollState::Paused;
                            if ui.button(if paused { "Resume" } else { "Pause" }).clicked() {
                                send(if paused {
                                    ScrollControl::Resume
                                } else {
                                    ScrollControl::Pause
                                });
                            }
                            if ui
                                .button("Done")
                                .on_hover_text("Stop and open the result")
                                .clicked()
                            {
                                send(ScrollControl::Stop);
                            }
                            if ui.button("Cancel").clicked() {
                                send(ScrollControl::Cancel);
                            }
                        });
                    } else {
                        ui.label("Preparing scrolling capture…");
                        if ui.button("Cancel").clicked() {
                            send(ScrollControl::Cancel);
                        }
                    }
                    if ui.ctx().input(|i| i.viewport().close_requested()) {
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::CancelClose);
                        send(ScrollControl::Stop);
                    }
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(100));
                },
            );
        }
    }
}

/// What the panel says: a short heading and the next step.
fn status(
    state: ScrollState,
    segments: u32,
    height: Option<u32>,
    countdown: std::time::Duration,
) -> (String, String) {
    match state {
        ScrollState::Countdown => (
            format!("Starting in {}…", countdown.as_secs_f32().ceil() as u32),
            "Get ready to scroll the window you selected.".into(),
        ),
        ScrollState::Capturing if segments == 0 => (
            "Hold still…".into(),
            "Taking the first screen. Start scrolling once it shows a height.".into(),
        ),
        ScrollState::Capturing => (
            "Scroll down slowly".into(),
            format!(
                "{} px captured. Click Done when you reach the end.",
                height.unwrap_or(0)
            ),
        ),
        ScrollState::Paused => (
            "Paused".into(),
            "Click Resume, then keep scrolling the same window. \
             If you went too fast, scroll back up a little first."
                .into(),
        ),
        _ => ("Finishing…".into(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn status_tells_the_next_step_in_plain_words() {
        let soon = Duration::from_millis(2100);
        assert_eq!(
            status(ScrollState::Countdown, 0, None, soon).0,
            "Starting in 3…"
        );
        assert_eq!(
            status(ScrollState::Capturing, 0, None, soon).0,
            "Hold still…"
        );
        let (heading, hint) = status(ScrollState::Capturing, 4, Some(1240), soon);
        assert_eq!(heading, "Scroll down slowly");
        assert!(hint.contains("1240 px"), "{hint}");
        assert!(hint.contains("Done"), "{hint}");
        assert!(
            status(ScrollState::Paused, 4, Some(1240), soon)
                .1
                .contains("Resume")
        );
    }
}
