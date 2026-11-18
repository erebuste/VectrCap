//! Fixed capture chrome: source geometry and output rendering never include this bar.
use super::*;
use egui::{Color32, Pos2, Rect, Stroke, Vec2};

const BAR_HEIGHT: f32 = 40.0;
const ICON_SIZE: f32 = 28.0;

/// Wide enough for every tool, both swatches, Options, Output and ×.
const DOCK_WIDTH: f32 = 740.0;
/// Gap between the dock and the top edge of the screen.
const DOCK_MARGIN: f32 = 8.0;

/// A compact dock, top centre of the launch monitor (or of the canvas),
/// never wider than the space it sits in.
fn bar_rect(viewport: Rect, monitor: Option<Rect>) -> Rect {
    let bounds = monitor
        .map(|m| m.intersect(viewport))
        .filter(|r| r.is_positive())
        .unwrap_or(viewport);
    let margin = DOCK_MARGIN.min((bounds.height() - BAR_HEIGHT).max(0.0));
    let size = Vec2::new(
        DOCK_WIDTH.min((bounds.width() - 2.0 * DOCK_MARGIN).max(bounds.width().min(1.0))),
        BAR_HEIGHT.min(bounds.height()),
    );
    Rect::from_min_size(
        Pos2::new(bounds.center().x - size.x / 2.0, bounds.top() + margin),
        size,
    )
}

/// The dock gets out of the way of a selection that reaches it, and comes
/// back when the pointer goes to it (except while dragging the selection),
/// so the tools stay reachable over a full-screen selection.
fn dock_hidden(dock: Rect, selection: Option<Rect>, pointer: Option<Pos2>, dragging: bool) -> bool {
    selection.is_some_and(|s| s.intersects(dock))
        && (dragging || !pointer.is_some_and(|p| dock.expand(4.0).contains(p)))
}

pub(super) fn tool_button(
    ui: &mut egui::Ui,
    tool: AnnotationTool,
    selected: bool,
) -> egui::Response {
    let response = ui.add(
        egui::Button::new("")
            .min_size(Vec2::splat(ICON_SIZE))
            .selected(selected),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::Button, ui.is_enabled(), selected, tool.label())
    });
    let visuals = ui.style().interact_selectable(&response, selected);
    let color = visuals.fg_stroke.color;
    let painter = ui.painter();
    let r = response.rect.shrink(6.0);
    let stroke = Stroke::new(1.5, color);
    let line = |a: Pos2, b: Pos2| {
        painter.line_segment([a, b], stroke);
    };
    use AnnotationTool::*;
    match tool {
        Region => {
            for (corner, dx, dy) in [
                (r.left_top(), 1.0, 1.0),
                (r.right_top(), -1.0, 1.0),
                (r.left_bottom(), 1.0, -1.0),
                (r.right_bottom(), -1.0, -1.0),
            ] {
                line(corner, corner + Vec2::new(dx * 6.0, 0.0));
                line(corner, corner + Vec2::new(0.0, dy * 6.0));
            }
        }
        Select => {
            let at = |x: f32, y: f32| r.min + Vec2::new(r.width() * x, r.height() * y);
            painter.add(egui::Shape::convex_polygon(
                vec![at(0.0, 0.0), at(0.0, 1.0), at(0.3, 0.65), at(1.0, 0.55)],
                color,
                Stroke::NONE,
            ));
            painter.add(egui::Shape::convex_polygon(
                vec![at(0.28, 0.55), at(0.5, 1.0), at(0.72, 0.87), at(0.48, 0.45)],
                color,
                Stroke::NONE,
            ));
        }
        Rectangle => {
            painter.rect_stroke(r, 0, stroke, egui::StrokeKind::Inside);
        }
        Ellipse => {
            painter.circle_stroke(r.center(), r.width() * 0.5, stroke);
        }
        Line | Arrow => {
            line(r.left_bottom(), r.right_top());
            if tool == Arrow {
                line(r.right_top(), r.right_top() + Vec2::new(-7.0, 1.0));
                line(r.right_top(), r.right_top() + Vec2::new(-1.0, 7.0));
            }
        }
        Freehand => {
            painter.add(egui::Shape::line(
                vec![
                    r.left_bottom(),
                    r.left_center(),
                    r.center() + Vec2::new(-2.0, -5.0),
                    r.center() + Vec2::new(2.0, 5.0),
                    r.right_center(),
                    r.right_top(),
                ],
                stroke,
            ));
        }
        Text => {
            painter.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                "A",
                egui::FontId::proportional(19.0),
                color,
            );
        }
        Step => {
            painter.circle_stroke(r.center(), 9.0, stroke);
            painter.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                "1",
                egui::FontId::proportional(13.0),
                color,
            );
        }
        Highlight => {
            painter.line_segment(
                [r.left_bottom(), r.right_top()],
                Stroke::new(6.0, color.gamma_multiply(0.65)),
            );
        }
        Spotlight => {
            painter.rect_filled(r, 0, color.gamma_multiply(0.25));
            painter.circle_stroke(r.center(), 5.0, stroke);
        }
        Redaction => {
            painter.rect_filled(r.shrink2(Vec2::new(0.0, 3.0)), 1, color);
        }
        AnnotationEraser | SmartEraser => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    r.left_center(),
                    r.center_top(),
                    r.right_center(),
                    r.center_bottom(),
                ],
                Color32::TRANSPARENT,
                stroke,
            ));
            line(r.center_top(), r.center_bottom());
            if tool == SmartEraser {
                painter.circle_filled(r.right_top() + Vec2::new(1.0, -1.0), 2.0, color);
            }
        }
    }
    response.on_hover_text(tool.label())
}

/// A dock button showing `color`; clicking it opens the palette to change it.
/// `name` is its tooltip, screen-reader name and the palette's title.
fn color_swatch(ui: &mut egui::Ui, name: &str, color: &mut [u8; 3]) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(ICON_SIZE), egui::Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), name));
    let visuals = ui.style().interact(&response);
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(4),
        visuals.weak_bg_fill,
        visuals.bg_stroke,
        egui::StrokeKind::Inside,
    );
    ui.painter().rect(
        rect.shrink(6.0),
        egui::CornerRadius::same(3),
        Color32::from_rgb(color[0], color[1], color[2]),
        Stroke::new(1.0, Color32::from_black_alpha(140)),
        egui::StrokeKind::Outside,
    );
    let response = response.on_hover_text(name);
    egui::Popup::from_toggle_button_response(&response)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.label(name);
            super::super::annotation_toolbar::color_palette(ui, color);
            ui.label(
                egui::RichText::new("Kept for your next captures")
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
        });
    response
}

impl CaptureOverlay {
    pub(super) fn toolbar(&mut self, ctx: &egui::Context, transform: OverlayTransform) {
        let rect = bar_rect(
            transform.viewport,
            self.toolbar_monitor.map(|m| transform.rect(m)),
        );
        let menu_open = egui::Popup::is_any_open(ctx);
        if !menu_open
            && dock_hidden(
                rect,
                self.document.crop().map(|c| transform.rect(c)),
                ctx.pointer_hover_pos(),
                self.region_dragging,
            )
        {
            // Errors still show below where the dock would be.
            self.toolbar_error(ctx, rect);
            return;
        }
        egui::Area::new(egui::Id::unique(("capture-tools", self.document.id())))
            .fixed_pos(rect.min)
            .movable(false)
            .constrain_to(rect)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_width(rect.width());
                egui::Frame::new()
                    .fill(ui.visuals().window_fill)
                    .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                    .corner_radius(egui::CornerRadius::same(8))
                    .shadow(ui.visuals().popup_shadow)
                    .inner_margin(egui::Margin::symmetric(6, 4))
                    .show(ui, |ui| {
                        ui.set_width((rect.width() - 12.0).max(1.0));
                        ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
                        if self.recording_selection {
                            ui.horizontal(|ui| {
                                ui.label(self.selection_title)
                                    .on_hover_text(&self.recording_summary);
                                if ui
                                    .button("Start")
                                    .on_hover_text("Start countdown (Enter)")
                                    .clicked()
                                {
                                    let result = self.handle(OverlayInput::Enter).map(|_| ());
                                    self.note(result);
                                }
                                if ui.button("Cancel").clicked() {
                                    let result = self.handle(OverlayInput::Escape).map(|_| ());
                                    self.note(result);
                                }
                            });
                        } else {
                            ui.add_enabled_ui(
                                !self.finishing && !self.finish_tasks_on_release,
                                |ui| {
                                    ui.horizontal(|ui| {
                                        // Room for the two color swatches, Options, Output and ×.
                                        let tool_width = (ui.available_width() - 242.0).max(28.0);
                                        egui::ScrollArea::horizontal()
                                            .id_salt(("capture-tool-scroll", self.document.id()))
                                            .max_width(tool_width)
                                            .auto_shrink([false, true])
                                            .show(ui, |ui| {
                                                ui.add_enabled_ui(!self.has_active_edit(), |ui| {
                                                    ui.horizontal(|ui| {
                                                        for (index, &tool) in
                                                            AnnotationTool::all().iter().enumerate()
                                                        {
                                                            if matches!(index, 2 | 9 | 12) {
                                                                ui.separator();
                                                            }
                                                            if tool_button(
                                                                ui,
                                                                tool,
                                                                self.tool == tool,
                                                            )
                                                            .clicked()
                                                            {
                                                                let result = self.set_tool(tool);
                                                                self.note(result);
                                                            }
                                                        }
                                                    });
                                                });
                                            });
                                        ui.add_enabled_ui(!self.has_active_edit(), |ui| {
                                            self.color_button(ui)
                                        });
                                        ui.menu_button("Options", |ui| self.toolbar_options(ui));
                                        if ui
                                            .add_enabled(
                                                !self.has_active_edit()
                                                    && self.document.crop().is_some(),
                                                egui::Button::new("Output"),
                                            )
                                            .on_hover_text("Image output options (Enter)")
                                            .clicked()
                                        {
                                            let result =
                                                self.handle(OverlayInput::Enter).map(|_| ());
                                            self.note(result);
                                        }
                                        let cancel = ui
                                            .add(
                                                egui::Button::new("×")
                                                    .min_size(Vec2::splat(ICON_SIZE)),
                                            )
                                            .on_hover_text("Cancel capture (Esc)");
                                        cancel.widget_info(|| {
                                            egui::WidgetInfo::labeled(
                                                egui::Role::Button,
                                                true,
                                                "Cancel capture",
                                            )
                                        });
                                        if cancel.clicked() {
                                            let result =
                                                self.handle(OverlayInput::Escape).map(|_| ());
                                            self.note(result);
                                        }
                                    });
                                },
                            );
                        }
                    });
            });
        self.toolbar_error(ctx, rect);
    }
    fn toolbar_error(&self, ctx: &egui::Context, rect: Rect) {
        if let Some(error) = &self.error {
            egui::Area::new(egui::Id::unique(("capture-tool-error", self.document.id())))
                .fixed_pos(rect.left_bottom() + Vec2::new(8.0, 6.0))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width((rect.width() - 16.0).max(100.0));
                        ui.colored_label(ui.visuals().error_fg_color, error.to_string());
                    });
                });
        }
    }
    /// The drawing and highlight color swatches, always in the dock whatever
    /// tool is active; each opens its palette.
    fn color_button(&mut self, ui: &mut egui::Ui) {
        color_swatch(ui, "Drawing color", &mut self.options.color);
        color_swatch(ui, "Highlight color", &mut self.options.highlight_color);
    }
    fn toolbar_options(&mut self, ui: &mut egui::Ui) {
        ui.set_max_width(340.0);
        ui.label(self.tool.label());
        ui.add_enabled_ui(!self.has_active_edit(), |ui| {
            self.options.draw(ui, self.tool);
            ui.separator();
            ui.horizontal(|ui| {
                for (label, vk) in [("Undo", 0x5A), ("Redo", 0x59)] {
                    if ui.button(label).clicked() {
                        let result = self
                            .handle_chord(
                                &ShortcutSettings::default(),
                                KeyChord {
                                    ctrl: true,
                                    alt: false,
                                    shift: false,
                                    win: false,
                                    key: crate::app::shortcuts::KeyCode::from_vk(vk)
                                        .expect("history key"),
                                },
                                ShortcutFocus::Normal,
                                false,
                            )
                            .map(|_| ());
                        self.note(result);
                    }
                }
                if ui.button("Clear crop").clicked() {
                    let result = self.clear_crop();
                    self.note(result);
                }
            });
            if self.tool == AnnotationTool::SmartEraser {
                if ui.button("Sample source on next click").clicked() {
                    self.sampling = true;
                    ui.close();
                }
                if self.sampled.is_none() {
                    ui.label("Sample a background color before painting.");
                }
            }
            if self.tool == AnnotationTool::Select
                && let Some(id) = self.selected
            {
                if ui.button("Delete selected").clicked() {
                    let result = self.apply(EditorCommand::Erase(vec![id]));
                    self.note(result);
                    self.selected = None;
                }
                if let Some(text @ Annotation::Text { .. }) =
                    self.document.annotations().iter().find(|a| a.id() == id)
                    && ui.button("Edit selected text").clicked()
                {
                    self.text = Some(text.clone());
                    self.text_is_modification = true;
                    self.text_focus_requested = true;
                    ui.close();
                }
            }
        });
        if self.has_active_edit() && ui.button("Cancel active edit").clicked() {
            let result = self.handle(OverlayInput::Escape).map(|_| ());
            self.note(result);
        }
        ui.separator();
        ui.small("Release selection: run configured tasks. Enter: output options. Esc: cancel edit, then capture.");
        ui.small("Tab: region tool. Shift+Tab: focus toolbar. Arrow keys: move region; Shift+arrows: resize.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pinned_bar_is_a_compact_dock_top_centre_of_the_launch_monitor() {
        let desktop = Rect::from_min_size(Pos2::new(-1920.0, -200.0), Vec2::new(5760.0, 2360.0));
        let monitor = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(3840.0, 2160.0));
        let bar = bar_rect(desktop, Some(monitor));
        assert_eq!(bar.width(), DOCK_WIDTH);
        assert_eq!(bar.center().x, monitor.center().x);
        assert_eq!(bar.top(), monitor.top() + DOCK_MARGIN);
        assert_eq!(bar.height(), BAR_HEIGHT);
        assert!(monitor.contains_rect(bar));
    }
    #[test]
    fn the_dock_hides_under_the_selection_unless_the_pointer_reaches_for_it() {
        let dock = Rect::from_min_size(Pos2::new(100.0, 8.0), Vec2::new(700.0, BAR_HEIGHT));
        let below = Rect::from_min_size(Pos2::new(200.0, 300.0), Vec2::splat(100.0));
        let over = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(900.0, 600.0));
        let on_dock = Some(dock.center());
        assert!(!dock_hidden(dock, None, None, false));
        assert!(!dock_hidden(dock, Some(below), None, false));
        assert!(dock_hidden(dock, Some(over), None, false));
        assert!(dock_hidden(
            dock,
            Some(over),
            Some(Pos2::new(400.0, 400.0)),
            false
        ));
        assert!(!dock_hidden(dock, Some(over), on_dock, false));
        // Not while the selection itself is being dragged across it.
        assert!(dock_hidden(dock, Some(over), on_dock, true));
    }
    #[test]
    fn launch_monitor_anchor_tracks_physical_geometry_at_native_dpi() {
        let source = PhysicalRect::new(-1920, -200, 5760, 2360).unwrap();
        let monitor = PhysicalRect::new(0, 0, 3840, 2160).unwrap();
        for scale in [1.0, 1.25, 2.0] {
            let viewport =
                Rect::from_min_size(Pos2::ZERO, Vec2::new(5760.0 / scale, 2360.0 / scale));
            let transform = OverlayTransform::new(source, viewport, scale).unwrap();
            let monitor_rect = transform.rect(monitor);
            let bar = bar_rect(viewport, Some(monitor_rect));
            assert_eq!(bar.center().x, monitor_rect.center().x);
            assert_eq!(bar.top(), 200.0 / scale + DOCK_MARGIN);
            assert!(monitor_rect.contains_rect(bar));
            let physical = PhysicalPoint { x: 100, y: 100 };
            assert_eq!(
                transform.physical(transform.logical(physical)),
                Some(physical)
            );
        }
    }
    #[test]
    fn pinned_bar_stays_bounded_in_narrow_review_canvases() {
        for width in [240.0, 360.0, 960.0] {
            let viewport = Rect::from_min_size(Pos2::new(17.0, 83.0), Vec2::new(width, 400.0));
            let bar = bar_rect(viewport, None);
            assert!(viewport.contains_rect(bar), "{bar:?} in {viewport:?}");
            assert!(bar.width() <= DOCK_WIDTH);
            assert_eq!(bar.center().x, viewport.center().x);
            assert!(bar.height() <= 40.0);
        }
    }

    /// Two frames per step; `click` presses and releases at a point first.
    fn frame(
        ctx: &egui::Context,
        click: Option<Pos2>,
        draw: &mut dyn FnMut(&mut egui::Ui),
    ) -> egui::FullOutput {
        let mut last = None;
        for pressed in click.map_or(vec![None], |_| vec![Some(true), Some(false)]) {
            let mut input = egui::RawInput::default();
            if let (Some(at), Some(pressed)) = (click, pressed) {
                input.events = vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ];
            }
            let mut output = ctx.run_ui(input, |ui| draw(ui));
            output.textures_delta.clear();
            last = Some(output);
        }
        last.expect("a frame ran")
    }
    fn named(output: &egui::FullOutput, name: &str) -> bool {
        output
            .platform_output
            .accesskit_update
            .iter()
            .flat_map(|update| &update.nodes)
            .any(|(_, node)| node.label() == Some(name))
    }

    #[test]
    fn highlight_swatch_opens_its_palette_and_sets_the_color() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut color = [255, 255, 0];
        let swatch = std::cell::Cell::new(Rect::NOTHING);
        let mut draw = |ui: &mut egui::Ui| {
            swatch.set(color_swatch(ui, "Highlight color", &mut color).rect);
        };
        let output = frame(&ctx, None, &mut draw);
        assert!(named(&output, "Highlight color"));
        assert!(!named(&output, "Red"), "palette starts closed");
        let at = swatch.get().center();
        frame(&ctx, Some(at), &mut draw);
        // A new popup spends its first frame sizing itself, unseen.
        let output = frame(&ctx, None, &mut draw);
        assert!(named(&output, "Red"), "click opens the palette");
        // Its accessibility bounds (points at scale 1): the popup fades in, so
        // the painted fill isn't the exact red yet.
        let red = output
            .platform_output
            .accesskit_update
            .iter()
            .flat_map(|update| &update.nodes)
            .find(|(_, node)| node.label() == Some("Red"))
            .and_then(|(_, node)| node.bounds())
            .map(|b| Pos2::new(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32))
            .expect("red swatch has bounds");
        frame(&ctx, Some(red), &mut draw);
        assert_eq!(color, [255, 60, 60]);
    }
}
