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
    // The overlay spans every monitor, so its corner can be on another screen
    // or in a gap no monitor covers: place the panel beside what is being
    // measured, inside that measurement's monitor.
    let anchor = state
        .drag
        .as_ref()
        .and_then(|drag| drag.rect().ok())
        .or_else(|| state.session.ruler.selected())
        .or_else(|| preview.as_ref().ok().and_then(|run| run.rect().ok()))
        .map(|rect| transform.rect(rect))
        .or_else(|| {
            response
                .hover_pos()
                .map(|p| egui::Rect::from_min_size(p, egui::Vec2::ZERO))
        });
    let screen = anchor
        .and_then(|anchor| {
            snapshot
                .monitors()
                .iter()
                .map(|monitor| transform.rect(monitor.bounds()))
                .find(|bounds| bounds.contains(anchor.center()))
        })
        .unwrap_or_else(|| ctx.viewport_rect());
    let size_key = ui.make_persistent_id("ruler-panel-size");
    let size = ctx
        .data(|d| d.get_temp::<egui::Vec2>(size_key))
        .unwrap_or(egui::vec2(300.0, 220.0));
    let position_key = ui.make_persistent_id("ruler-panel-position");
    let previous = ctx.data(|d| d.get_temp::<egui::Pos2>(position_key));
    // Hold still while the pointer is on the panel: otherwise the preview
    // under the cursor moves the panel away from its own buttons.
    let pointer = ctx.pointer_hover_pos();
    let position = match previous {
        Some(previous)
            if pointer.is_some_and(|p| egui::Rect::from_min_size(previous, size).contains(p)) =>
        {
            previous
        }
        _ => panel_position(
            anchor.unwrap_or(egui::Rect::from_min_size(screen.min, egui::Vec2::ZERO)),
            size,
            screen,
        ),
    };
    ctx.data_mut(|d| d.insert_temp(position_key, position));
    let panel = egui::Window::new("Pixel ruler")
        .resizable(false)
        .collapsible(false)
        .fixed_pos(position)
        .constrain_to(screen)
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
    if let Some(panel) = panel {
        // Next frame places the panel with its real size.
        ctx.data_mut(|d| d.insert_temp(size_key, panel.response.rect.size()));
    }
    Ok(input)
}

/// Top-left for a `size` panel beside `anchor`: to its right, else its left,
/// else below, else above (12 pt away), always kept inside `bounds`.
fn panel_position(anchor: egui::Rect, size: egui::Vec2, bounds: egui::Rect) -> egui::Pos2 {
    const GAP: f32 = 12.0;
    let fits_x = |x: f32| x >= bounds.left() && x + size.x <= bounds.right();
    let fits_y = |y: f32| y >= bounds.top() && y + size.y <= bounds.bottom();
    let (x, y) = if fits_x(anchor.right() + GAP) {
        (anchor.right() + GAP, anchor.top())
    } else if fits_x(anchor.left() - GAP - size.x) {
        (anchor.left() - GAP - size.x, anchor.top())
    } else if fits_y(anchor.bottom() + GAP) {
        (anchor.left(), anchor.bottom() + GAP)
    } else {
        (anchor.left(), anchor.top() - GAP - size.y)
    };
    egui::pos2(
        x.clamp(bounds.left(), (bounds.right() - size.x).max(bounds.left())),
        y.clamp(bounds.top(), (bounds.bottom() - size.y).max(bounds.top())),
    )
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
    use egui::{Rect, pos2, vec2};

    /// Two side-by-side 1000×800 screens; the overlay spans both.
    fn screen() -> Rect {
        Rect::from_min_size(pos2(1000.0, 0.0), vec2(1000.0, 800.0))
    }
    const PANEL: egui::Vec2 = vec2(240.0, 180.0);

    fn inside(panel: Rect, bounds: Rect) -> bool {
        bounds.contains_rect(panel)
    }

    #[test]
    fn panel_sits_right_of_the_selection_on_its_screen() {
        let anchor = Rect::from_min_size(pos2(1200.0, 300.0), vec2(80.0, 40.0));
        let pos = panel_position(anchor, PANEL, screen());
        assert_eq!(pos, pos2(1292.0, 300.0));
        assert!(inside(Rect::from_min_size(pos, PANEL), screen()));
    }

    #[test]
    fn panel_flips_left_near_the_right_edge() {
        let anchor = Rect::from_min_size(pos2(1900.0, 300.0), vec2(60.0, 40.0));
        let pos = panel_position(anchor, PANEL, screen());
        assert_eq!(pos.x, 1900.0 - 12.0 - PANEL.x);
        assert!(inside(Rect::from_min_size(pos, PANEL), screen()));
    }

    #[test]
    fn wide_selection_puts_the_panel_below_it() {
        let anchor = Rect::from_min_size(pos2(1005.0, 100.0), vec2(990.0, 20.0));
        let pos = panel_position(anchor, PANEL, screen());
        assert_eq!(pos.y, 132.0);
        assert!(inside(Rect::from_min_size(pos, PANEL), screen()));
    }

    #[test]
    fn panel_never_leaves_the_selections_screen() {
        for anchor in [
            Rect::from_min_size(pos2(1000.0, 0.0), vec2(1000.0, 800.0)),
            Rect::from_min_size(pos2(1990.0, 790.0), vec2(4.0, 4.0)),
            Rect::from_min_size(pos2(1000.0, 780.0), vec2(30.0, 20.0)),
        ] {
            let pos = panel_position(anchor, PANEL, screen());
            assert!(
                inside(Rect::from_min_size(pos, PANEL), screen()),
                "{anchor:?} -> {pos:?}"
            );
        }
    }

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
