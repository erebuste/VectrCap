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
/// The recording controls: one-row dock, like the capture tools (logical points).
pub const CONTROLS_SIZE: (f32, f32) = (560.0, 44.0);
/// Like the capture dock: top centre of the monitor, else bottom centre, else
/// beside the selection. Never over the recorded pixels.
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
    let centre = monitor.left() + (monitor.width() - w) as i32 / 2;
    for (cx, cy) in [
        (centre, monitor.top().saturating_add(8)),
        (centre, monitor.bottom().saturating_sub(h as i32 + 8)),
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
    let mut push = |input| out.push(AppCommand::Recording(input));
    let visuals = ui.visuals().clone();
    egui::Frame::new()
        .fill(visuals.window_fill)
        .stroke(visuals.widgets.noninteractive.bg_stroke)
        .inner_margin(egui::Margin::symmetric(10, 0))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if r.confirmation {
                    ui.label("Discard this recording?");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Keep recording").clicked() {
                            push(RecordingInput::ConfirmDiscard(false));
                        }
                        if ui.button("Discard").clicked() {
                            push(RecordingInput::ConfirmDiscard(true));
                        }
                    });
                    return;
                }
                let (dot, status) = match r.state {
                    RecordingState::Countdown => (
                        visuals.warn_fg_color,
                        format!(
                            "Starting in {}",
                            r.countdown
                                .as_secs()
                                .saturating_add(u64::from(r.countdown.subsec_nanos() > 0))
                        ),
                    ),
                    RecordingState::Paused => (
                        visuals.weak_text_color(),
                        format!("Paused {}", clock(r.elapsed)),
                    ),
                    RecordingState::Finalizing => (visuals.weak_text_color(), "Finishing…".into()),
                    RecordingState::Interrupted => (visuals.error_fg_color, "Interrupted".into()),
                    _ => (egui::Color32::from_rgb(232, 64, 64), clock(r.elapsed)),
                };
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 5.0, dot);
                ui.label(egui::RichText::new(status).strong().monospace());
                if let Some(o) = &r.options {
                    let size = o
                        .dimensions()
                        .map(|(w, h)| format!("{w}×{h} · "))
                        .unwrap_or_default();
                    let audio = o.audio().map_or_else(
                        || "no audio".into(),
                        |a| format!("{:?} audio", a.mode).to_lowercase(),
                    );
                    ui.label(
                        egui::RichText::new(format!("{size}{} fps · {audio}", o.fps()))
                            .small()
                            .color(visuals.weak_text_color()),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled_ui(r.state != RecordingState::Finalizing, |ui| {
                        if ui.button("×").on_hover_text("Cancel recording").clicked() {
                            push(RecordingInput::Cancel);
                        }
                        if ui.button("Stop").clicked() {
                            push(RecordingInput::Stop);
                        }
                        let pause = if r.state == RecordingState::Paused {
                            "Resume"
                        } else {
                            "Pause"
                        };
                        if r.state != RecordingState::Countdown && ui.button(pause).clicked() {
                            push(RecordingInput::PauseResume);
                        }
                    });
                });
            });
        });
    out
}
/// 0:04.3 style elapsed time.
fn clock(t: std::time::Duration) -> String {
    let s = t.as_secs_f64();
    format!("{}:{:04.1}", (s / 60.0) as u64, s % 60.0)
}
