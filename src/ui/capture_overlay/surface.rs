use super::super::{image_review::TiledImage, magnifier::CaptureReadout};
use super::*;
use std::sync::{
    Arc,
    mpsc::{self, Receiver, TryRecvError},
};

type PreviewResult = (u64, Result<Arc<image::RgbaImage>, AppError>);
#[derive(Default)]
pub(super) struct OverlayPreview {
    pending: Option<Receiver<PreviewResult>>,
    attempted: Option<u64>,
    tiled: Option<TiledImage>,
    accepted: Option<u64>,
}
impl OverlayPreview {
    fn update(
        &mut self,
        ctx: &egui::Context,
        document: &EditorDocument,
        generation: u64,
        command: Option<EditorCommand>,
    ) -> Result<(), AppError> {
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok((key, result)) => {
                    self.pending = None;
                    if key == generation {
                        self.tiled = Some(TiledImage::new(result?)?);
                        self.accepted = Some(key);
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.pending = None;
                    return Err(AppError::Encoder("annotation preview worker stopped"));
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if self.pending.is_none() && self.attempted != Some(generation) {
            self.attempted = Some(generation);
            // This snapshot drops history references and shares immutable source.
            let mut preview = document.export_revision().document;
            if let Some(command) = command {
                preview.apply(command)?;
            }
            if preview.annotations().is_empty() {
                self.tiled = Some(TiledImage::new(preview.source().clone())?);
                self.accepted = Some(generation);
            } else {
                let (tx, rx) = mpsc::sync_channel(1);
                let ctx = ctx.clone();
                std::thread::Builder::new()
                    .name("vectrcap-overlay-preview".into())
                    .spawn(move || {
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            crate::editor::render::render_preview(&preview).map(Arc::new)
                        }))
                        .unwrap_or(Err(AppError::Encoder("annotation preview worker failed")));
                        let _ = tx.send((generation, result));
                        ctx.request_repaint();
                    })
                    .map_err(|e| AppError::io("starting annotation preview", &e))?;
                self.pending = Some(rx);
            }
        }
        Ok(())
    }
}
impl CaptureOverlay {
    /// Borderless frozen desktop surface. The host supplies the native monitor
    /// scale, excluding egui's user chrome zoom, before creating the viewport.
    pub fn viewport_builder(
        &self,
        native_pixels_per_point: f32,
    ) -> Result<egui::ViewportBuilder, AppError> {
        if !native_pixels_per_point.is_finite() || !(0.1..=10.0).contains(&native_pixels_per_point)
        {
            return Err(AppError::InvalidInput("native overlay scale is invalid"));
        }
        let b = self.document.bounds();
        Ok(egui::ViewportBuilder::default()
            .with_title("VectrCap capture")
            .with_decorations(false)
            .with_resizable(false)
            .with_clamp_size_to_monitor_size(false)
            .with_taskbar(false)
            .with_window_level(egui::WindowLevel::AlwaysOnTop)
            .with_position([
                b.left() as f32 / native_pixels_per_point,
                b.top() as f32 / native_pixels_per_point,
            ])
            .with_inner_size([
                b.width() as f32 / native_pixels_per_point,
                b.height() as f32 / native_pixels_per_point,
            ]))
    }
    pub(super) fn note(&mut self, result: Result<(), AppError>) {
        if let Err(error) = result {
            self.error = Some(error);
        }
    }
    fn text_editor(&mut self, ctx: &egui::Context) {
        let mut commit = false;
        let mut cancel = false;
        if let Some(Annotation::Text {
            content,
            size,
            color,
            ..
        }) = &mut self.text
        {
            egui::Window::new("Edit annotation text").id(egui::Id::unique(("capture-text",self.document.id())))
                .resizable(true).collapsible(false).show(ctx, |ui| {
                    let mut draft=content.to_string();
                    let response=ui.add(egui::TextEdit::multiline(&mut draft).return_key(egui::KeyboardShortcut::new(egui::Modifiers::SHIFT,egui::Key::Enter)).char_limit(crate::editor::document::MAX_TEXT_SCALARS).desired_rows(3).hint_text("Annotation text"));
                    if self.text_focus_requested { response.request_focus(); self.text_focus_requested=false; }
                    if response.changed() { *content=draft.into(); }
                    ui.add(egui::Slider::new(size,4.0..=512.0).text("Text size (physical px)"));
                    ui.label("Text color"); ui.color_edit_button_srgba_unmultiplied(color);
                    ui.label("Enter commits; Shift+Enter adds a line. Unsupported glyphs keep this edit open.");
                    ui.horizontal(|ui| {commit=ui.button("Commit text").clicked();cancel=ui.button("Discard text edit").clicked();});
                });
        }
        if commit {
            let r = self.handle(OverlayInput::Enter).map(|_| ());
            self.note(r);
        }
        if cancel {
            let r = self.handle(OverlayInput::Escape).map(|_| ());
            self.note(r);
        }
    }
    /// Real egui event loop shared by native fullscreen hosting and headless UI
    /// tests. All temporary chrome is painted after the CPU annotation layer.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        saved: &ShortcutSettings,
    ) -> Option<CaptureOverlayEvent> {
        let transform = match OverlayTransform::new(
            self.document.bounds(),
            ui.max_rect(),
            ui.ctx().pixels_per_point(),
        ) {
            Ok(transform) => transform,
            Err(error) => {
                self.error = Some(error);
                return None;
            }
        };
        self.show_transformed(ui, saved, transform)
    }
    pub fn show_review(
        &mut self,
        ui: &mut egui::Ui,
        saved: &ShortcutSettings,
        viewport: &mut super::super::image_review::ImageViewport,
    ) -> Option<CaptureOverlayEvent> {
        let b = self.document.bounds();
        let canvas = viewport.canvas(ui, (b.width(), b.height()));
        let transform =
            match OverlayTransform::for_review(b, canvas.rect, canvas.offset, canvas.zoom) {
                Ok(transform) => transform,
                Err(error) => {
                    self.error = Some(error);
                    return None;
                }
            };
        self.show_transformed(ui, saved, transform)
    }
    fn show_transformed(
        &mut self,
        ui: &mut egui::Ui,
        saved: &ShortcutSettings,
        transform: OverlayTransform,
    ) -> Option<CaptureOverlayEvent> {
        if self.cancelled {
            return Some(CaptureOverlayEvent::Cancel);
        }
        let ctx = ui.ctx().clone();
        let viewport = transform.viewport;
        let events = ctx.input(|i| i.events.clone());
        // Resolve non-text shortcuts before widgets so a canvas Tab can select
        // the saved Region tool; an already focused toolbar keeps native Tab.
        let mut consumed = Vec::new();
        let mut canvas_tab = false;
        if self.text.is_none() && !self.finishing {
            for (index, event) in events.iter().enumerate() {
                let focus = if self.chrome_focused {
                    ShortcutFocus::TextEntry
                } else {
                    ShortcutFocus::Normal
                };
                match self.handle_key_event(event, saved, focus, self.chrome_focused) {
                    Ok(true) => {
                        canvas_tab |= matches!(
                            event,
                            egui::Event::Key {
                                key: egui::Key::Tab,
                                ..
                            }
                        );
                        consumed.push(index);
                    }
                    Ok(false) => {}
                    Err(error) => {
                        self.error = Some(error);
                        consumed.push(index);
                    }
                }
                if self.cancelled {
                    return Some(CaptureOverlayEvent::Cancel);
                }
            }
            ctx.input_mut(|i| {
                let mut index = 0;
                i.events.retain(|_| {
                    let keep = !consumed.contains(&index);
                    index += 1;
                    keep
                });
            });
        }
        if !self.toolbar_hidden {
            self.toolbar(&ctx, transform);
        }
        let editing_text = self.text.is_some();
        self.text_editor(&ctx);
        // Let TextEdit first process Text/Paste/IME; then Enter commits the
        // updated draft, rather than losing the final characters of this frame.
        if editing_text && self.text.is_some() && !self.finishing {
            for event in &events {
                if matches!(
                    event,
                    egui::Event::Key {
                        key: egui::Key::Enter | egui::Key::Escape,
                        pressed: true,
                        ..
                    }
                ) {
                    let r = self
                        .handle_key_event(event, saved, ShortcutFocus::TextEntry, true)
                        .map(|_| ());
                    self.note(r);
                }
            }
        }
        if self.recording_selection
            && self.finish_visible
            && let Some(rect) = self.document.crop()
        {
            self.finishing = true;
            return Some(CaptureOverlayEvent::RecordingSelected(rect));
        }
        if self.finish_tasks_on_release && !self.finishing && !self.has_active_edit() {
            let wake = ctx.clone();
            self.prepare_output(move || wake.request_repaint());
            if let Some(error) = self.output.error().cloned() {
                // Retain the source and expose the existing preview/retry controls.
                self.finish_tasks_on_release = false;
                self.error = Some(error);
            } else if self.output.ready(&self.document).is_some() {
                let result = self.confirm_finish(OverlayAction::RunSelectedTasks);
                self.finish_tasks_on_release = false;
                match result {
                    Ok(finish) => {
                        self.finished_on_release = true;
                        self.finish_visible = false;
                        return Some(CaptureOverlayEvent::Finish(finish));
                    }
                    Err(error) => self.error = Some(error),
                }
            } else {
                egui::Area::new(egui::Id::unique("preparing-screenshot"))
                    .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                    .show(&ctx, |ui| {
                        ui.label("Preparing screenshot…");
                    });
            }
        }
        let mut finish = None;
        if self.finish_visible && !self.finish_tasks_on_release && !self.has_active_edit() {
            let mut open = true;
            egui::Window::new("Final image output")
                .id(egui::Id::unique(("capture-finish", self.document.id())))
                .open(&mut open)
                .default_size([700.0, 600.0])
                .vscroll(true)
                .show(&ctx, |ui| {
                    if let Some(error) = &self.error {
                        ui.colored_label(ui.visuals().error_fg_color, error.to_string());
                    }
                    finish = self.output.draw(ui, &self.document);
                });
            if !open && !self.finishing {
                self.finish_visible = false;
            }
        }
        if let Some(output) = finish {
            self.finished_on_release = false;
            // draw() can emit only a ready revision. Do not admit edits until
            // the owner acknowledges success or calls finish_failed.
            self.finishing = true;
            return Some(CaptureOverlayEvent::Finish(output));
        }
        if !self.finishing && !self.finish_tasks_on_release {
            for event in &events {
                let result = match event {
                    egui::Event::PointerMoved(pos) => {
                        let p = transform.physical(*pos);
                        if let Some(p) = p {
                            if self.gesture.is_some()
                                || self.region_dragging
                                || over_image(&ctx, ui, *pos)
                            {
                                let modifiers = ctx.input(|i| i.modifiers);
                                if let Some(selection) = &mut self.selection {
                                    selection.handle(SelectionInput::Modifiers {
                                        alt: modifiers.alt,
                                        shift: modifiers.shift,
                                        ctrl: modifiers.ctrl,
                                    });
                                }
                                self.pointer_move(p, modifiers.shift)
                            } else {
                                self.hover = None;
                                Ok(())
                            }
                        } else {
                            self.hover = None;
                            Ok(())
                        }
                    }
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers,
                    } => {
                        if let Some(p) = transform.physical(*pos) {
                            if *pressed && over_image(&ctx, ui, *pos) {
                                ctx.memory_mut(|m| {
                                    if let Some(id) = m.focused() {
                                        m.surrender_focus(id);
                                    }
                                });
                                self.chrome_focused = false;
                                if let Some(selection) = &mut self.selection {
                                    selection.handle(SelectionInput::Modifiers {
                                        alt: modifiers.alt,
                                        shift: modifiers.shift,
                                        ctrl: modifiers.ctrl,
                                    });
                                }
                                self.pointer_down(p)
                            } else if !pressed && (self.gesture.is_some() || self.region_dragging) {
                                self.pointer_up(p, modifiers.shift)
                            } else {
                                Ok(())
                            }
                        } else if !pressed && (self.gesture.is_some() || self.region_dragging) {
                            // A release outside the viewport must not strand a
                            // gesture. Preserve its last valid physical point.
                            self.pointer_up(
                                self.pointer.expect("active pointer gesture"),
                                modifiers.shift,
                            )
                        } else {
                            Ok(())
                        }
                    }
                    egui::Event::PointerButton {
                        pos,
                        button,
                        pressed: true,
                        ..
                    } if over_image(&ctx, ui, *pos) => {
                        let button = match button {
                            egui::PointerButton::Secondary => Some(MouseButton::Right),
                            egui::PointerButton::Middle => Some(MouseButton::Middle),
                            egui::PointerButton::Extra1 => Some(MouseButton::Button4),
                            egui::PointerButton::Extra2 => Some(MouseButton::Button5),
                            _ => None,
                        };
                        button.map_or(Ok(()), |button| {
                            self.auxiliary(button, transform.physical(*pos))
                        })
                    }
                    egui::Event::MouseWheel {
                        delta, modifiers, ..
                    } if self.tool == AnnotationTool::Region
                        && delta.y != 0.0
                        && ctx
                            .input(|i| i.pointer.hover_pos())
                            .is_some_and(|pos| over_image(&ctx, ui, pos)) =>
                    {
                        self.magnifier
                            .scroll(delta.y > 0.0, modifiers.ctrl, &self.preferences);
                        Ok(())
                    }
                    _ => Ok(()),
                };
                self.note(result);
                if self.cancelled {
                    return Some(CaptureOverlayEvent::Cancel);
                }
            }
        }
        // egui computes Tab traversal before this callback receives events.
        // Removing a consumed canvas Tab from InputState alone cannot undo the
        // focus that its widgets acquired during this pass.
        if canvas_tab {
            ctx.memory_mut(|memory| {
                if let Some(id) = memory.focused() {
                    memory.surrender_focus(id);
                }
            });
        }
        self.chrome_focused = ctx.memory(|m| m.focused().is_some());
        let command = self
            .gesture
            .as_ref()
            .and_then(|g| g.command(self).ok().flatten())
            .filter(|_| self.tool != AnnotationTool::Region);
        let result = self
            .preview
            .update(&ctx, &self.document, self.generation, command);
        self.note(result);
        let paint = ui.painter().with_clip_rect(viewport);
        paint.rect_filled(viewport, 0, egui::Color32::BLACK);
        if let Some(tiled) = &mut self.preview.tiled {
            let result = tiled.paint(
                &paint,
                viewport,
                transform.source_offset,
                1.0 / f64::from(transform.pixels_per_point),
            );
            self.note(result);
        }
        self.paint_crop(&paint, transform);
        if let Some(point) = self.pointer
            && let Some(pos) = ctx.input(|i| i.pointer.hover_pos())
            && over_image(&ctx, ui, pos)
        {
            let prefs = self.magnifier.apply(&self.preferences);
            match CaptureReadout::sample(&self.document, point, &prefs) {
                Ok(mut readout) => {
                    let r = readout.paint(&ctx, pos, viewport, &prefs);
                    self.note(r);
                }
                Err(error) => self.error = Some(error),
            }
        }
        if self.preview.pending.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        None
    }
    fn paint_crop(&self, painter: &egui::Painter, t: OverlayTransform) {
        let preview = self
            .selection
            .as_ref()
            .and_then(|s| self.pointer.and_then(|p| s.preview(p).ok().flatten()));
        let crop = preview.or(self.document.crop()).or(self.hover);
        // ShareX's "marching ants": a white line with black 5/5 dashes, one
        // physical pixel wide, readable on any screen content and on the dim.
        let color = egui::Color32::WHITE;
        let px = 1.0 / t.pixels_per_point;
        let ants = |r: egui::Rect| {
            let r = r.shrink(px / 2.0);
            let corners = [
                r.left_top(),
                r.right_top(),
                r.right_bottom(),
                r.left_bottom(),
            ];
            for i in 0..4 {
                let line = [corners[i], corners[(i + 1) % 4]];
                painter.line_segment(line, egui::Stroke::new(px, color));
                painter.extend(egui::Shape::dashed_line(
                    &line,
                    egui::Stroke::new(px, egui::Color32::from_black_alpha(230)),
                    5.0 * px,
                    5.0 * px,
                ));
            }
        };
        if let Some(crop) = crop {
            let r = t.rect(crop).intersect(t.viewport);
            let v = t.viewport;
            let dim = egui::Color32::from_black_alpha(
                (f32::from(self.preferences.dim_percent) * 2.55).round() as u8,
            );
            for outside in [
                egui::Rect::from_min_max(v.min, egui::pos2(v.right(), r.top())),
                egui::Rect::from_min_max(egui::pos2(v.left(), r.bottom()), v.max),
                egui::Rect::from_min_max(
                    egui::pos2(v.left(), r.top()),
                    egui::pos2(r.left(), r.bottom()),
                ),
                egui::Rect::from_min_max(
                    egui::pos2(r.right(), r.top()),
                    egui::pos2(v.right(), r.bottom()),
                ),
            ] {
                painter.rect_filled(outside, 0, dim);
            }
            ants(r);
            if self.document.crop().is_some() && self.tool == AnnotationTool::Region {
                // Same five-physical-pixel inside edge zones used by SelectionModel.
                let side = 5.0 / t.pixels_per_point;
                for pos in [
                    r.left_top(),
                    r.center_top(),
                    r.right_top(),
                    r.left_center(),
                    r.right_center(),
                    r.left_bottom(),
                    r.center_bottom(),
                    r.right_bottom(),
                ] {
                    let min = egui::pos2(
                        pos.x.clamp(r.left(), (r.right() - side).max(r.left())),
                        pos.y.clamp(r.top(), (r.bottom() - side).max(r.top())),
                    );
                    painter.rect_filled(
                        egui::Rect::from_min_size(min, egui::Vec2::splat(side)),
                        0,
                        color,
                    );
                }
            }
        }
        if let Some(id) = self.selected
            && let Some(a) = self.document.annotations().iter().find(|a| a.id() == id)
            && let Some(bounds) = annotation_bounds(a)
        {
            ants(t.rect(bounds).expand(px));
        }
    }
}
fn over_image(ctx: &egui::Context, ui: &egui::Ui, pos: egui::Pos2) -> bool {
    ctx.layer_id_at(pos)
        .is_none_or(|layer| layer == ui.layer_id())
}
fn annotation_bounds(a: &Annotation) -> Option<PhysicalRect> {
    match a {
        Annotation::Rectangle { bounds, .. }
        | Annotation::Ellipse { bounds, .. }
        | Annotation::Highlight { bounds, .. }
        | Annotation::Spotlight { bounds, .. }
        | Annotation::Redaction { bounds, .. } => Some(*bounds),
        Annotation::Line { start, end, .. } | Annotation::Arrow { start, end, .. } => {
            PhysicalRect::new(
                start.x.min(end.x),
                start.y.min(end.y),
                start.x.abs_diff(end.x).max(1),
                start.y.abs_diff(end.y).max(1),
            )
            .ok()
        }
        Annotation::Step { center, radius, .. } => PhysicalRect::new(
            center.x.checked_sub(radius.ceil() as i32)?,
            center.y.checked_sub(radius.ceil() as i32)?,
            (radius.ceil() as u32) * 2,
            (radius.ceil() as u32) * 2,
        )
        .ok(),
        Annotation::Text {
            position,
            content,
            size,
            ..
        } => {
            let (w, h) = crate::editor::text::measure(content, *size).ok()?;
            PhysicalRect::new(
                position.x,
                position.y,
                (w.ceil() as u32).max(1),
                (h.ceil() as u32).max(1),
            )
            .ok()
        }
        Annotation::Freehand { points, .. } | Annotation::SmartEraseStroke { points, .. } => {
            PhysicalRect::new(
                points.iter().map(|p| p.x).min()?,
                points.iter().map(|p| p.y).min()?,
                points
                    .iter()
                    .map(|p| p.x)
                    .max()?
                    .abs_diff(points.iter().map(|p| p.x).min()?)
                    .max(1),
                points
                    .iter()
                    .map(|p| p.y)
                    .max()?
                    .abs_diff(points.iter().map(|p| p.y).min()?)
                    .max(1),
            )
            .ok()
        }
    }
}
