use crate::{
    app::{
        desktop::{DesktopClient, DesktopView},
        error::AppError,
        events::AppCommand,
    },
    capture::geometry::PhysicalRect,
    recording::session::{RecordingInput, RecordingState},
};
use eframe::egui;
use std::sync::Arc;
pub fn control_bounds(
    selection: PhysicalRect,
    monitor: PhysicalRect,
    size: (u32, u32),
) -> Result<PhysicalRect, AppError> {
    let (w, h) = size;
    if w == 0 || h == 0 || w > monitor.width() || h > monitor.height() {
        return Err(AppError::InvalidInput(
            "recording controls do not fit on the selected display",
        ));
    }
    let left = selection.left();
    let top = selection.top();
    let x = left.clamp(monitor.left(), monitor.right().saturating_sub(w as i32));
    let y = top.clamp(monitor.top(), monitor.bottom().saturating_sub(h as i32));
    for (cx, cy) in [
        (x, selection.bottom().saturating_add(4)),
        (x, top.saturating_sub(h as i32).saturating_sub(4)),
        (selection.right().saturating_add(4), y),
        (left.saturating_sub(w as i32).saturating_sub(4), y),
    ] {
        if let Ok(r) = PhysicalRect::new(cx, cy, w, h)
            && monitor.contains_rect(r)
            && selection.intersection(r).is_none()
        {
            return Ok(r);
        }
    }
    Err(AppError::InvalidInput(
        "leave room outside the recording area for controls, or select a smaller rectangle",
    ))
}
#[derive(Default)]
pub struct RecordingHost {
    review: super::recording_review::ReviewPlayer,
    selection_id: Option<uuid::Uuid>,
}
impl RecordingHost {
    pub fn show(&mut self, ctx: &egui::Context, client: &DesktopClient, view: &DesktopView) {
        let active_selection = view
            .recording
            .as_ref()
            .filter(|r| r.selection.is_some())
            .map(|r| r.id);
        if self.selection_id != active_selection
            && let Some(id) = self.selection_id.take()
        {
            match crate::platform::windows::recording_events::hide_selection(&format!(
                "VectrCap recording selection {id}"
            )) {
                Ok(()) => {
                    ctx.send_viewport_cmd_to(
                        egui::ViewportId::from_hash_of(("recording-selection", id)),
                        egui::ViewportCommand::Close,
                    );
                    if let Err(e) = client.submit(AppCommand::RecordingSelectionClosed(id)) {
                        client.report_error(e);
                    }
                }
                Err(e) => {
                    client.report_error(e);
                    let _ = client.submit(AppCommand::Recording(RecordingInput::Cancel));
                }
            }
        }
        let Some(recording) = &view.recording else {
            self.review.clear();
            return;
        };
        let id = recording.id;
        if let Some(surface) = &recording.selection {
            self.selection_id = Some(id);
            let builder = surface
                .try_lock()
                .ok()
                .and_then(|s| s.overlay.viewport_builder(s.scale).ok());
            if let Some(builder) = builder {
                let builder = builder.with_title(format!("VectrCap recording selection {id}"));
                let surface = surface.clone();
                let client = client.clone();
                ctx.show_viewport_deferred(
                    egui::ViewportId::from_hash_of(("recording-selection", id)),
                    builder,
                    move |ui, _| {
                        let view = client.snapshot();
                        if view
                            .recording
                            .as_ref()
                            .and_then(|r| r.selection.as_ref())
                            .is_none_or(|s| !Arc::ptr_eq(s, &surface))
                        {
                            let title = format!("VectrCap recording selection {id}");
                            match crate::platform::windows::recording_events::hide_selection(&title)
                            {
                                Ok(()) => {
                                    let _ = client.submit(AppCommand::RecordingSelectionClosed(id));
                                }
                                Err(e) => client.report_error(e),
                            }
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                            return;
                        }
                        let Ok(mut state) = surface.try_lock() else {
                            return;
                        };
                        if let (Some(settings), Some(recording)) = (&view.settings, &view.recording)
                        {
                            let summary = super::recording_settings::selection_summary(
                                settings,
                                recording.mp4,
                                state.overlay.document().crop(),
                            );
                            state.overlay.set_recording_summary(summary);
                        }
                        let event = if ui.ctx().input(|i| i.viewport().close_requested()) {
                            Some(super::capture_overlay::CaptureOverlayEvent::Cancel)
                        } else {
                            view.settings
                                .as_ref()
                                .and_then(|s| state.overlay.show(ui, &s.shortcuts))
                        };
                        let command = match event {
                            Some(
                                super::capture_overlay::CaptureOverlayEvent::RecordingSelected(
                                    rect,
                                ),
                            ) => Some(AppCommand::RecordingSelection { session: id, rect }),
                            Some(super::capture_overlay::CaptureOverlayEvent::Cancel) => {
                                Some(AppCommand::Recording(RecordingInput::Cancel))
                            }
                            _ => None,
                        };
                        drop(state);
                        if let Some(c) = command
                            && let Err(e) = client.submit(c)
                        {
                            client.report_error(e);
                        }
                    },
                );
            }
            return;
        }
        if matches!(
            recording.state,
            RecordingState::Countdown
                | RecordingState::Recording
                | RecordingState::Paused
                | RecordingState::Finalizing
                | RecordingState::Interrupted
        ) && let Some(o) = &recording.options
        {
            if let Some(bounds) = recording.controls {
                let title = format!("VectrCap recording controls {id}");
                let _ = crate::platform::windows::recording_events::place_overlay(&title, bounds);
                let client = client.clone();
                let title_copy = title.clone();
                ctx.show_viewport_deferred(
                    egui::ViewportId::from_hash_of(("recording-controls", id)),
                    egui::ViewportBuilder::default()
                        .with_title(title)
                        .with_decorations(false)
                        .with_resizable(false)
                        .with_taskbar(false)
                        .with_visible(false)
                        .with_window_level(egui::WindowLevel::AlwaysOnTop)
                        .with_inner_size([bounds.width() as f32, bounds.height() as f32])
                        .with_clamp_size_to_monitor_size(false),
                    move |ui, _| {
                        let view = client.snapshot();
                        let Some(r) = view.recording.filter(|r| {
                            r.id == id
                                && matches!(
                                    r.state,
                                    RecordingState::Countdown
                                        | RecordingState::Recording
                                        | RecordingState::Paused
                                        | RecordingState::Finalizing
                                        | RecordingState::Interrupted
                                )
                        }) else {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                            return;
                        };
                        if let Err(e) = crate::platform::windows::recording_events::place_overlay(
                            &title_copy,
                            bounds,
                        ) {
                            client.report_error(e);
                            let _ = client.submit(AppCommand::Recording(RecordingInput::Cancel));
                            return;
                        }
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        let mut commands = draw_controls(ui, &r);
                        if ui.ctx().input(|i| i.viewport().close_requested()) {
                            ui.ctx()
                                .send_viewport_cmd(egui::ViewportCommand::CancelClose);
                            commands.push(AppCommand::Recording(RecordingInput::Cancel));
                        }
                        for c in commands {
                            if let Err(e) = client.submit(c) {
                                client.report_error(e);
                            }
                        }
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(100));
                    },
                );
            }
            // Four thin, click-through windows form a boundary entirely outside the source.
            let r = o.rect;
            for (index, (x, y, w, h)) in [
                (
                    r.left().saturating_sub(2),
                    r.top().saturating_sub(2),
                    r.width().saturating_add(4),
                    2,
                ),
                (
                    r.left().saturating_sub(2),
                    r.bottom(),
                    r.width().saturating_add(4),
                    2,
                ),
                (r.left().saturating_sub(2), r.top(), 2, r.height()),
                (r.right(), r.top(), 2, r.height()),
            ]
            .into_iter()
            .enumerate()
            {
                let Ok(bounds) = PhysicalRect::new(x, y, w, h) else {
                    continue;
                };
                let title = format!("VectrCap recording boundary {id} {index}");
                let _ = crate::platform::windows::recording_events::place_overlay(&title, bounds);
                let client = client.clone();
                ctx.show_viewport_deferred(
                    egui::ViewportId::from_hash_of(("recording-boundary", id, index)),
                    egui::ViewportBuilder::default()
                        .with_title(title.clone())
                        .with_decorations(false)
                        .with_resizable(false)
                        .with_taskbar(false)
                        .with_visible(false)
                        .with_mouse_passthrough(true)
                        .with_inner_size([w as f32, h as f32])
                        .with_clamp_size_to_monitor_size(false)
                        .with_window_level(egui::WindowLevel::AlwaysOnTop),
                    move |ui, _| {
                        if client.snapshot().recording.is_none_or(|r| {
                            r.id != id
                                || !matches!(
                                    r.state,
                                    RecordingState::Countdown
                                        | RecordingState::Recording
                                        | RecordingState::Paused
                                )
                        }) {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                            return;
                        }
                        if crate::platform::windows::recording_events::place_overlay(&title, bounds)
                            .is_ok()
                        {
                            ui.ctx()
                                .send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        }
                        ui.painter()
                            .rect_filled(ui.max_rect(), 0.0, egui::Color32::RED);
                    },
                );
            }
        } else if matches!(
            recording.state,
            RecordingState::Review | RecordingState::Failed
        ) {
            self.review.show(ctx, client, recording);
        }
    }
}
pub fn draw_controls(
    ui: &mut egui::Ui,
    r: &crate::app::controller::RecordingView,
) -> Vec<AppCommand> {
    let mut out = vec![];
    ui.heading(if r.state == RecordingState::Countdown {
        format!(
            "Starting in {}",
            r.countdown
                .as_secs()
                .saturating_add(u64::from(r.countdown.subsec_nanos() > 0))
        )
    } else {
        format!("{:?} - {:.1}s active", r.state, r.elapsed.as_secs_f64())
    });
    if let Some(o) = &r.options {
        ui.label(format!("{} FPS - {:?}", o.fps(), o.dimensions().ok()));
        ui.label(super::audio_sources::source_summary(o.audio()));
    }
    ui.horizontal_wrapped(|ui| {
        for (label, input) in [
            (
                if r.state == RecordingState::Paused {
                    "Resume"
                } else {
                    "Pause"
                },
                RecordingInput::PauseResume,
            ),
            ("Stop", RecordingInput::Stop),
            ("Cancel", RecordingInput::Cancel),
        ] {
            if ui.button(label).clicked() {
                out.push(AppCommand::Recording(input));
            }
        }
    });
    if r.confirmation {
        ui.label("Discard the active recording?");
        for (label, yes) in [("Discard recording", true), ("Keep recording", false)] {
            if ui.button(label).clicked() {
                out.push(AppCommand::Recording(RecordingInput::ConfirmDiscard(yes)));
            }
        }
    }
    out
}
