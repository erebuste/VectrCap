use super::{capture_overlay::OverlayTransform, screen_sampler::UtilitySurface};
use crate::{
    app::error::AppError,
    capture::geometry::{PhysicalPoint, PhysicalRect},
    tools::{
        ruler::{RectHandle, measure_rect, measure_run, rect_from_endpoints, resize_rect},
        session::UtilityInput,
    },
};
use eframe::egui::{self, Color32, Stroke};

pub(crate) struct RulerDrag {
    start: PhysicalPoint,
    end: PhysicalPoint,
    edit: Option<(usize, PhysicalRect, Option<RectHandle>)>,
}
impl RulerDrag {
    fn rect(&self) -> Result<PhysicalRect, AppError> {
        match self.edit {
            Some((_, rect, Some(handle))) => resize_rect(rect, handle, self.end),
            Some((_, rect, None)) => {
                let dx = i32::try_from(i64::from(self.end.x) - i64::from(self.start.x))
                    .map_err(|_| AppError::InvalidInput("ruler drag overflows"))?;
                let dy = i32::try_from(i64::from(self.end.y) - i64::from(self.start.y))
                    .map_err(|_| AppError::InvalidInput("ruler drag overflows"))?;
                rect.translated(dx, dy)
            }
            None => rect_from_endpoints(self.start, self.end),
        }
    }
}
fn handle_positions(
    transform: &OverlayTransform,
    rect: PhysicalRect,
) -> [(RectHandle, egui::Pos2); 8] {
    use RectHandle::*;
    let outer = transform.rect(rect);
    let half_pixel = egui::vec2(
        outer.width() / rect.width() as f32 / 2.0,
        outer.height() / rect.height() as f32 / 2.0,
    );
    // Handles target the inclusive endpoint pixel centers, rather than the
    // exclusive geometric border one pixel past the measured source.
    let r = egui::Rect::from_min_max(
        transform.logical(PhysicalPoint {
            x: rect.left(),
            y: rect.top(),
        }) + half_pixel,
        transform.logical(PhysicalPoint {
            x: rect.right() - 1,
            y: rect.bottom() - 1,
        }) + half_pixel,
    );
    [
        (TopLeft, r.left_top()),
        (Top, r.center_top()),
        (TopRight, r.right_top()),
        (Right, r.right_center()),
        (BottomRight, r.right_bottom()),
        (Bottom, r.center_bottom()),
        (BottomLeft, r.left_bottom()),
        (Left, r.left_center()),
    ]
}
fn hit(
    state: &UtilitySurface,
    transform: &OverlayTransform,
    p: egui::Pos2,
) -> Option<(usize, PhysicalRect, Option<RectHandle>)> {
    if let Some(index) = state.session.ruler.selected_index()
        && let Some(rect) = state.session.ruler.selected()
    {
        for (handle, position) in handle_positions(transform, rect) {
            if position.distance(p) <= 6.0 {
                return Some((index, rect, Some(handle)));
            }
        }
    }
    state
        .session
        .ruler
        .measurements()
        .iter()
        .enumerate()
        .rev()
        .find(|(_, r)| transform.rect(**r).expand(3.0).contains(p))
        .map(|(i, r)| (i, *r, None))
}
pub(crate) fn show(
    ui: &mut egui::Ui,
    state: &mut UtilitySurface,
    response: &egui::Response,
    transform: &OverlayTransform,
) -> Result<Option<UtilityInput>, AppError> {
    let ctx = ui.ctx().clone();
    let mut input = None;
    if response.hovered() {
        let scroll = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| {
                    if let egui::Event::MouseWheel { delta, .. } = e {
                        Some(delta.y)
                    } else {
                        None
                    }
                })
                .sum::<f32>()
        });
        if scroll != 0.0 {
            input = Some(UtilityInput::Tolerance(if scroll > 0.0 { 1 } else { -1 }));
        }
        if response.clicked_by(egui::PointerButton::Middle) {
            input = Some(UtilityInput::ToggleAxis);
        }
    }
    let point = response
        .interact_pointer_pos()
        .and_then(|p| transform.physical(p));
    if response.drag_started_by(egui::PointerButton::Primary)
        && let Some(start) = ctx
            .input(|i| i.pointer.press_origin())
            .and_then(|p| transform.physical(p))
    {
        let edit = ctx
            .input(|i| i.pointer.press_origin())
            .and_then(|p| hit(state, transform, p));
        if let Some((index, _, _)) = edit {
            state.session.ruler.select_index(Some(index))?;
        }
        state.drag = Some(RulerDrag {
            start,
            end: point.unwrap_or(start),
            edit,
        });
    }
    if let Some(drag) = &mut state.drag
        && let Some(point) = point
    {
        drag.end = point;
    }
    if response.drag_stopped_by(egui::PointerButton::Primary)
        && let Some(drag) = state.drag.take()
    {
        let rect = drag.rect()?;
        input = Some(match drag.edit {
            Some((index, _, _)) => UtilityInput::ReplaceRect { index, rect },
            None => UtilityInput::AddRect(rect),
        });
    }
    if response.clicked_by(egui::PointerButton::Primary) {
        let selected = response
            .interact_pointer_pos()
            .and_then(|p| hit(state, transform, p))
            .map(|(i, _, _)| i);
        state.session.ruler.select_index(selected)?;
        if selected.is_none() {
            input = Some(UtilityInput::Select);
        }
    }
    if response.clicked_by(egui::PointerButton::Secondary) {
        let selected = response
            .interact_pointer_pos()
            .and_then(|p| hit(state, transform, p))
            .map(|(i, _, _)| i);
        state.session.ruler.select_index(selected)?;
        input = Some(UtilityInput::RemoveOrClose);
    }
    let snapshot = state
        .session
        .snapshot()
        .ok_or(AppError::Capture("ruler source unavailable"))?;
    let preview = measure_run(
        snapshot,
        state.session.sampler.point,
        state.session.ruler.axis,
        state.session.ruler.tolerance_percent,
    );
    if let Ok(run) = &preview {
        paint_valid(
            ui,
            state,
            transform,
            run.rect()?,
            Color32::LIGHT_GREEN,
            false,
        );
    }
    for (index, &rect) in state.session.ruler.measurements().iter().enumerate() {
        let selected = state.session.ruler.selected_index() == Some(index);
        paint_valid(
            ui,
            state,
            transform,
            rect,
            if selected {
                Color32::YELLOW
            } else {
                Color32::LIGHT_BLUE
            },
            selected,
        );
    }
    if let Some(drag) = &state.drag {
        paint_valid(ui, state, transform, drag.rect()?, Color32::WHITE, true);
    }
    egui::Window::new("Pixel ruler")
        .resizable(false)
        .default_pos(ctx.viewport_rect().min + egui::vec2(12.0, 12.0))
        .show(&ctx, |ui| {
            ui.label("Frozen source · physical pixels");
            ui.label(format!(
                "Axis: {:?} · Tolerance: {}% · {}/256 measurements",
                state.session.ruler.axis,
                state.session.ruler.tolerance_percent,
                state.session.ruler.measurements().len()
            ));
            if ui.button("Toggle H/V").clicked() {
                input = Some(UtilityInput::ToggleAxis)
            }
            if let Some(rect) = state.session.ruler.selected() {
                if let Ok(metrics) = measure_rect(rect) {
                    ui.monospace(metrics.text());
                }
                if ui.button("Copy selected text").clicked() {
                    input = Some(UtilityInput::CopySelected);
                }
            } else {
                match &preview {
                    Ok(run) => {
                        ui.label(format!("Preview: {} physical pixels", run.length));
                    }
                    Err(_) => {
                        ui.label("Source sample unavailable");
                    }
                }
            }
            super::color_result::feedback(ui, &state.copy_feedback);
            ui.label("Click: retain run · Drag: rectangle · Wheel: tolerance");
            if ui.button("Clear all").clicked() {
                input = Some(UtilityInput::Clear)
            }
            if ui.button("Close").clicked() {
                input = Some(UtilityInput::Close)
            }
            if let Some(error) = &state.session.error {
                ui.colored_label(Color32::LIGHT_RED, error.to_string());
            }
        });
    Ok(input)
}
fn paint_valid(
    ui: &egui::Ui,
    state: &UtilitySurface,
    transform: &OverlayTransform,
    rect: PhysicalRect,
    color: Color32,
    handles: bool,
) {
    if let Some(snapshot) = state.session.snapshot() {
        for monitor in snapshot.monitors() {
            if monitor.bounds().intersection(rect).is_none() {
                continue;
            }
            let painter = ui
                .painter()
                .with_clip_rect(transform.rect(monitor.bounds()).intersect(ui.clip_rect()));
            paint_rect(&painter, transform, rect, color, handles);
        }
    }
}
fn paint_rect(
    painter: &egui::Painter,
    transform: &OverlayTransform,
    rect: PhysicalRect,
    color: Color32,
    handles: bool,
) {
    let r = transform.rect(rect);
    let stroke = Stroke::new(1.0, color);
    painter.rect_stroke(r, 0, stroke, egui::StrokeKind::Inside);
    // Ticks are presentation only; metrics always use the physical endpoints.
    for (length, horizontal) in [(rect.width(), true), (rect.height(), false)] {
        let stride = (length.div_ceil(256) as usize).max(10);
        for n in (0..length).step_by(stride) {
            let p = if horizontal {
                PhysicalPoint {
                    x: (i64::from(rect.left()) + i64::from(n)) as i32,
                    y: rect.top(),
                }
            } else {
                PhysicalPoint {
                    x: rect.left(),
                    y: (i64::from(rect.top()) + i64::from(n)) as i32,
                }
            };
            let p = transform.logical(p);
            let offset = if horizontal {
                egui::vec2(0.0, 5.0)
            } else {
                egui::vec2(5.0, 0.0)
            };
            painter.line_segment([p, p + offset], stroke);
        }
    }
    if handles {
        for (_, p) in handle_positions(transform, rect) {
            painter.rect_filled(
                egui::Rect::from_center_size(p, egui::vec2(6.0, 6.0)),
                0,
                color,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn handle_center_targets_inclusive_endpoint_pixel() {
        let bounds = PhysicalRect::new(-100, -100, 800, 600).unwrap();
        let rect = PhysicalRect::new(10, 20, 5, 6).unwrap();
        for ppp in [1.0, 1.5, 2.0] {
            let transform = OverlayTransform::new(
                bounds,
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0 / ppp, 600.0 / ppp)),
                ppp,
            )
            .unwrap();
            let (_, point) = handle_positions(&transform, rect)
                .into_iter()
                .find(|(handle, _)| *handle == RectHandle::BottomRight)
                .unwrap();
            assert_eq!(
                transform.physical(point),
                Some(PhysicalPoint { x: 14, y: 25 })
            );
        }
    }
}
