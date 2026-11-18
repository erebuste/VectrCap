//! Independent pin presentation. Pixels/preview are prepared before reaching UI.
use crate::{
    app::error::AppError,
    pin::{PinInput, PinSnapshot},
};
use eframe::egui::{self, Color32, Key, PointerButton, Rect, Vec2};

fn hover_control(ui: &mut egui::Ui, icon: &str, label: &str, hint: &str) -> egui::Response {
    let response = ui.small_button(icon).on_hover_text(hint);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), label));
    response
}
pub fn viewport_title(id: crate::pin::PinId) -> String {
    format!("VectrCap pin {id:?}")
}
pub fn viewport_builder(pin: &PinSnapshot, scale: f32) -> Result<egui::ViewportBuilder, AppError> {
    if !scale.is_finite() || !(0.1..=10.0).contains(&scale) {
        return Err(AppError::InvalidInput("pin display scale is invalid"));
    }
    let (w, h) = pin.display_size();
    if w.max(h) > 32760 {
        return Err(AppError::InvalidInput(
            "pin zoom exceeds the native viewport size limit",
        ));
    }
    let p = pin.state.position();
    Ok(egui::ViewportBuilder::default()
        .with_title(viewport_title(pin.id))
        .with_decorations(false)
        .with_resizable(false)
        .with_taskbar(false)
        .with_transparent(true)
        .with_visible(false)
        .with_clamp_size_to_monitor_size(false)
        .with_window_level(if pin.state.options().always_on_top {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        })
        .with_inner_size([w.max(96) as f32 / scale, h.max(96) as f32 / scale])
        .with_position([p.x as f32 / scale, p.y as f32 / scale]))
}
pub fn keyboard_inputs(ctx: &egui::Context, focused: bool, editing: bool) -> Vec<PinInput> {
    if !focused || editing {
        return vec![];
    }
    ctx.input_mut(|i| {
        let mut inputs = Vec::new();
        let mut copied = false;
        i.events.retain(|event| {
            let input = match event {
                egui::Event::Copy => Some(PinInput::Copy),
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } if !modifiers.alt && !modifiers.mac_cmd => {
                    let steps = if *key == Key::Minus { -1 } else { 1 };
                    match key {
                        Key::Plus | Key::Equals | Key::Minus => Some(if modifiers.ctrl {
                            PinInput::Opacity(steps)
                        } else {
                            PinInput::Zoom(steps)
                        }),
                        Key::C if modifiers.ctrl => Some(PinInput::Copy),
                        Key::Escape => Some(PinInput::Close),
                        Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight
                            if !modifiers.ctrl =>
                        {
                            let amount = if modifiers.shift { 10 } else { 1 };
                            Some(PinInput::Move {
                                dx: if *key == Key::ArrowLeft {
                                    -amount
                                } else if *key == Key::ArrowRight {
                                    amount
                                } else {
                                    0
                                },
                                dy: if *key == Key::ArrowUp {
                                    -amount
                                } else if *key == Key::ArrowDown {
                                    amount
                                } else {
                                    0
                                },
                            })
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some(input) = input {
                if matches!(input, PinInput::Copy) {
                    if copied {
                        return false;
                    }
                    copied = true;
                }
                inputs.push(input);
                false
            } else {
                true
            }
        });
        inputs
    })
}

/// Zoom after dragging the corner grip by `delta` physical pixels, starting
/// from a pin `start` pixels big at `start_zoom`. The larger stretch wins, so
/// the picture keeps its shape.
pub fn grip_zoom(start_zoom: u16, start: (u32, u32), delta: (i32, i32)) -> u16 {
    let ratio = |len: u32, d: i32| (f64::from(len) + f64::from(d)) / f64::from(len.max(1));
    let ratio = ratio(start.0, delta.0).max(ratio(start.1, delta.1));
    (f64::from(start_zoom) * ratio).round().clamp(20.0, 500.0) as u16
}

/// The pin's size at `zoom`, whatever zoom the app state has now.
fn size_at(pin: &PinSnapshot, zoom: u16) -> (u32, u32) {
    let mut state = pin.state.clone();
    state.zoom = zoom;
    state.display_size(&pin.image)
}

/// Where the pin is shown while it is moved or resized. Moves reach the app
/// state through the controller queue a little later; the window follows the
/// pointer at once and keeps this place until the state has caught up.
#[derive(Clone, Copy)]
struct Hold {
    position: crate::capture::geometry::PhysicalPoint,
    zoom: u16,
    released: Option<f64>,
}

pub struct PinWindow {
    texture: Option<egui::TextureHandle>,
    smooth: Option<bool>,
    pub options_open: bool,
    options_id: crate::pin::PinId,
    drag_remainder: Vec2,
    drag_pointer: Option<crate::capture::geometry::PhysicalPoint>,
    wheel_remainder: f32,
    wheel_ctrl: bool,
    hold: Option<Hold>,
    /// Pointer, pin size and zoom when the corner grip was grabbed.
    grip: Option<(crate::capture::geometry::PhysicalPoint, (u32, u32), u16)>,
}
impl Default for PinWindow {
    fn default() -> Self {
        Self {
            texture: None,
            smooth: None,
            options_open: false,
            options_id: crate::pin::PinId::new(),
            drag_remainder: Vec2::ZERO,
            drag_pointer: None,
            wheel_remainder: 0.0,
            wheel_ctrl: false,
            hold: None,
            grip: None,
        }
    }
}
impl PinWindow {
    /// Position and zoom the window shows ahead of the app state, while the
    /// pin is being moved or resized.
    pub fn held(&self) -> Option<(crate::capture::geometry::PhysicalPoint, u16)> {
        self.hold.map(|h| (h.position, h.zoom))
    }
    pub fn show_at_pointer(
        &mut self,
        ui: &mut egui::Ui,
        pin: &PinSnapshot,
        pointer: crate::capture::geometry::PhysicalPoint,
    ) -> Vec<PinInput> {
        self.show_inner(ui, pin, Some(pointer))
    }
    pub fn show(&mut self, ui: &mut egui::Ui, pin: &PinSnapshot) -> Vec<PinInput> {
        self.show_inner(ui, pin, None)
    }
    fn show_inner(
        &mut self,
        ui: &mut egui::Ui,
        pin: &PinSnapshot,
        pointer: Option<crate::capture::geometry::PhysicalPoint>,
    ) -> Vec<PinInput> {
        let focused = ui.ctx().input(|i| i.viewport().focused.unwrap_or(false));
        let mut inputs = keyboard_inputs(ui.ctx(), focused, ui.ctx().text_edit_focused());
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            return vec![PinInput::Close];
        }
        let options = pin.state.options();
        let filter = if options.smooth {
            egui::TextureOptions::LINEAR
        } else {
            egui::TextureOptions::NEAREST
        };
        if self.texture.is_none() {
            let pixels = pin.image.display_pixels();
            self.texture = Some(ui.ctx().load_texture(
                "pin-image",
                egui::ColorImage::from_rgba_unmultiplied(
                    [pixels.width() as usize, pixels.height() as usize],
                    pixels.as_raw(),
                ),
                filter,
            ));
        } else if self.smooth != Some(options.smooth)
            && let Some(texture) = &mut self.texture
        {
            let pixels = pin.image.display_pixels();
            texture.set(
                egui::ColorImage::from_rgba_unmultiplied(
                    [pixels.width() as usize, pixels.height() as usize],
                    pixels.as_raw(),
                ),
                filter,
            );
        }
        self.smooth = Some(options.smooth);
        let now = ui.input(|i| i.time);
        if let Some(h) = self.hold
            && let Some(released) = h.released
            && ((pin.state.position() == h.position && pin.state.zoom_percent() == h.zoom)
                || now - released > 1.0)
        {
            self.hold = None;
        }
        let zoom = self.hold.map_or(pin.state.zoom_percent(), |h| h.zoom);
        let scale = ui.ctx().pixels_per_point();
        let (w, h) = size_at(pin, zoom);
        let size = egui::vec2(w as f32 / scale, h as f32 / scale);
        let rect = if pin.state.minimized() {
            Rect::from_center_size(ui.max_rect().center(), size)
        } else {
            Rect::from_min_size(ui.max_rect().min, size)
        };
        let mut painter = ui.painter().clone();
        painter.set_opacity(f32::from(pin.state.opacity_percent()) / 100.0);
        if let Some(color) = options.background {
            painter.rect_filled(
                rect,
                0,
                Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]),
            );
        } else {
            // At most 128×128 checks even for extreme display sizes.
            let side = (8.0 / scale).max(rect.width().max(rect.height()) / 128.0);
            let nx = (rect.width() / side).ceil() as u32;
            let ny = (rect.height() / side).ceil() as u32;
            for y in 0..ny {
                for x in 0..nx {
                    let tile = Rect::from_min_size(
                        rect.min + egui::vec2(x as f32 * side, y as f32 * side),
                        Vec2::splat(side),
                    )
                    .intersect(rect);
                    painter.rect_filled(
                        tile,
                        0,
                        Color32::from_gray(if (x + y) % 2 == 0 { 160 } else { 200 }),
                    );
                }
            }
        }
        if let Some(texture) = &self.texture {
            painter.image(
                texture.id(),
                rect,
                Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        if options.border_width() > 0 {
            let c = options.border_color;
            painter.rect_stroke(
                rect,
                0,
                egui::Stroke::new(
                    f32::from(options.border_width()) / scale,
                    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]),
                ),
                egui::StrokeKind::Inside,
            );
        }
        let response = ui.interact(
            ui.max_rect(),
            ui.scope_id().with("pin-image"),
            egui::Sense::click_and_drag(),
        );
        let dragging = response.dragged_by(PointerButton::Primary);
        if !dragging {
            self.drag_pointer =
                if response.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_down()) {
                    pointer
                } else {
                    None
                };
        }
        let hover = response.hovered() || self.options_open || ui.ctx().is_pointer_over_egui();
        let mut toolbar = None;
        if hover {
            toolbar = Some(
                egui::Area::new(ui.scope_id().with("pin-hover-controls"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(ui.max_rect().min + egui::vec2(2.0, 2.0))
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(1.0, 1.0);
                            egui::Grid::new("pin-controls")
                                .num_columns(2)
                                .spacing([1.0, 1.0])
                                .show(ui, |ui| {
                                    if hover_control(ui, "C", "Copy image", "Copy image (Ctrl+C)")
                                        .clicked()
                                    {
                                        inputs.push(PinInput::Copy);
                                    }
                                    if hover_control(
                                        ui,
                                        "R",
                                        "Reset",
                                        "Reset zoom and opacity (middle click)",
                                    )
                                    .clicked()
                                    {
                                        inputs.push(PinInput::Reset);
                                    }
                                    ui.end_row();
                                    if hover_control(ui, "⋯", "Options", "Options").clicked() {
                                        self.options_open = !self.options_open;
                                    }
                                    if hover_control(
                                        ui,
                                        "×",
                                        "Close",
                                        "Close (Escape / right click)",
                                    )
                                    .clicked()
                                    {
                                        inputs.push(PinInput::Close);
                                    }
                                });
                        });
                    })
                    .response
                    .rect,
            );
        }
        let over_chrome = toolbar.is_some_and(|rect| {
            ui.ctx()
                .pointer_hover_pos()
                .is_some_and(|p| rect.contains(p))
        });
        if !over_chrome || dragging {
            if response.double_clicked_by(PointerButton::Primary) {
                inputs.push(PinInput::ToggleMinimized);
            } else if dragging {
                let (dx, dy) = if let Some(pointer) = pointer {
                    let previous = self.drag_pointer.replace(pointer).unwrap_or(pointer);
                    (
                        pointer.x.checked_sub(previous.x).unwrap_or(0),
                        pointer.y.checked_sub(previous.y).unwrap_or(0),
                    )
                } else {
                    // Headless/embedded surfaces do not move an HWND. Native
                    // hosts supply physical screen coordinates above, avoiding
                    // feedback when movement changes local pointer positions.
                    self.drag_remainder += response.drag_delta() * scale;
                    let delta = (
                        self.drag_remainder.x.trunc() as i32,
                        self.drag_remainder.y.trunc() as i32,
                    );
                    self.drag_remainder -= egui::vec2(delta.0 as f32, delta.1 as f32);
                    delta
                };
                if dx != 0 || dy != 0 {
                    inputs.push(PinInput::Move { dx, dy });
                    if pointer.is_some() {
                        let hold = self.hold.get_or_insert(Hold {
                            position: pin.state.position(),
                            zoom,
                            released: None,
                        });
                        hold.position.x = hold.position.x.saturating_add(dx);
                        hold.position.y = hold.position.y.saturating_add(dy);
                        hold.released = None;
                    }
                }
            }
            if response.clicked_by(PointerButton::Middle) {
                inputs.push(PinInput::Reset);
            }
            if response.clicked_by(PointerButton::Secondary) {
                inputs.push(PinInput::Close);
            }
            if response.hovered() {
                ui.ctx().input(|i| {
                    for event in &i.events {
                        if let egui::Event::MouseWheel {
                            unit,
                            delta,
                            modifiers,
                            ..
                        } = event
                        {
                            if modifiers.ctrl != self.wheel_ctrl {
                                self.wheel_remainder = 0.0;
                                self.wheel_ctrl = modifiers.ctrl;
                            }
                            self.wheel_remainder += delta.y
                                / if *unit == egui::MouseWheelUnit::Point {
                                    40.0
                                } else {
                                    1.0
                                };
                            let steps = self.wheel_remainder.trunc().clamp(-100.0, 100.0) as i32;
                            self.wheel_remainder -= steps as f32;
                            if steps != 0 {
                                inputs.push(if modifiers.ctrl {
                                    PinInput::Opacity(steps)
                                } else {
                                    PinInput::Zoom(steps)
                                });
                            }
                        }
                    }
                });
            }
        }
        // Corner grip: drag to resize (the zoom follows, the shape is kept).
        let mut gripping = false;
        if let Some(pointer) = pointer
            && !pin.state.minimized()
        {
            let side = 16.0;
            let grip_rect = Rect::from_min_size(rect.max - Vec2::splat(side), Vec2::splat(side))
                .intersect(ui.max_rect());
            let grip = ui.interact(
                grip_rect,
                ui.scope_id().with("pin-grip"),
                egui::Sense::drag(),
            );
            grip.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, "Resize (drag)")
            });
            gripping = grip.dragged();
            if gripping {
                let (start, size, from) = *self.grip.get_or_insert((pointer, (w, h), zoom));
                let next = grip_zoom(
                    from,
                    size,
                    (
                        pointer.x.saturating_sub(start.x),
                        pointer.y.saturating_sub(start.y),
                    ),
                );
                let hold = self.hold.get_or_insert(Hold {
                    position: pin.state.position(),
                    zoom,
                    released: None,
                });
                hold.released = None;
                if hold.zoom != next {
                    hold.zoom = next;
                    inputs.push(PinInput::SetZoom(next));
                }
            } else {
                self.grip = None;
            }
            if hover || gripping {
                let stroke = egui::Stroke::new(1.5, Color32::from_white_alpha(220));
                let shadow = egui::Stroke::new(3.0, Color32::from_black_alpha(140));
                for inset in [4.0, 8.0, 12.0] {
                    let a = egui::pos2(grip_rect.max.x - 2.0, grip_rect.max.y - inset);
                    let b = egui::pos2(grip_rect.max.x - inset, grip_rect.max.y - 2.0);
                    ui.painter().line_segment([a, b], shadow);
                    ui.painter().line_segment([a, b], stroke);
                }
            }
            if grip.hovered() || gripping {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
            }
        }
        if let Some(h) = &mut self.hold {
            if !dragging && !gripping && h.released.is_none() {
                h.released = Some(now);
            }
            ui.ctx().request_repaint();
        }
        inputs
    }
}

#[derive(Default)]
pub struct PinHost {
    views:
        std::collections::HashMap<crate::pin::PinId, std::sync::Arc<std::sync::Mutex<PinWindow>>>,
    refreshed: Option<std::time::Instant>,
}
impl PinHost {
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        client: &crate::app::desktop::DesktopClient,
        view: &crate::app::desktop::DesktopView,
    ) {
        use crate::{
            app::events::{AppCommand, Lifecycle},
            platform::windows::windows as native,
        };
        self.views.retain(|id, window| {
            let keep =
                view.lifecycle == Lifecycle::Running && view.pins.iter().any(|pin| pin.id == *id);
            if !keep {
                native::unregister_pin_window(*id);
                ctx.send_viewport_cmd_to(
                    egui::ViewportId::from_hash_of(("pin", *id)),
                    egui::ViewportCommand::Close,
                );
                if let Ok(window) = window.lock() {
                    native::unregister_pin_window(window.options_id);
                    ctx.send_viewport_cmd_to(
                        egui::ViewportId::from_hash_of(("pin-options", *id)),
                        egui::ViewportCommand::Close,
                    );
                }
            }
            keep
        });
        if view.lifecycle != Lifecycle::Running {
            return;
        }
        if !view.pins.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            if self
                .refreshed
                .is_none_or(|time| time.elapsed() >= std::time::Duration::from_secs(2))
            {
                match client.submit(AppCommand::RefreshPinWorkAreas) {
                    Ok(()) => self.refreshed = Some(std::time::Instant::now()),
                    Err(error) => client.report_error(error),
                }
            }
        }
        for pin in &view.pins {
            let state = self.views.entry(pin.id).or_default().clone();
            // While the pin is moved or resized, its window leads the app
            // state; build and place it where the window says, not behind it.
            let held = state.lock().ok().and_then(|w| w.held());
            let mut shown = pin.clone();
            if let Some((position, zoom)) = held {
                shown.state.position = position;
                shown.state.zoom = zoom;
            }
            let pin = &shown;
            let native_scale = native::pin_window_scale(pin.id).unwrap_or_else(|| {
                ctx.input(|i| i.viewport().native_pixels_per_point.unwrap_or(1.0))
            });
            let builder = match viewport_builder(pin, native_scale) {
                Ok(builder) => builder,
                Err(error) => {
                    client.report_error(error);
                    continue;
                }
            };
            let (w, h) = pin.display_size();
            if let Err(error) = native::sync_pin_window(
                pin.id,
                pin.state.position(),
                (w.max(96), h.max(96)),
                pin.visible,
                pin.state.options().always_on_top,
            ) {
                client.report_error(error);
            }
            let id = pin.id;
            let window = state.clone();
            let live_client = client.clone();
            ctx.show_viewport_deferred(
                egui::ViewportId::from_hash_of(("pin", id)),
                builder,
                move |ui, _| {
                    let live = live_client.snapshot();
                    let Some(pin) = live
                        .pins
                        .iter()
                        .find(|p| p.id == id)
                        .filter(|_| live.lifecycle == Lifecycle::Running)
                    else {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        return;
                    };
                    if !pin.visible || native::pins_suspended() {
                        return;
                    }
                    let Ok(mut window) = window.lock() else {
                        return;
                    };
                    let pointer = match native::physical_pointer() {
                        Ok(pointer) => pointer,
                        Err(error) => {
                            live_client.report_error(error);
                            return;
                        }
                    };
                    for input in window.show_at_pointer(ui, pin, pointer) {
                        if let Err(error) = live_client.submit(AppCommand::Pin { id, input }) {
                            live_client.report_error(error);
                        }
                    }
                    // Follow the pointer now, not when the main window next
                    // repaints (up to 100 ms later).
                    if let Some((position, zoom)) = window.held() {
                        let (w, h) = size_at(pin, zoom);
                        if let Err(error) = native::sync_pin_window(
                            id,
                            position,
                            (w.max(96), h.max(96)),
                            true,
                            pin.state.options().always_on_top,
                        ) {
                            live_client.report_error(error);
                        }
                    }
                    if let Some(error) = &live.error
                        && ui.ctx().input(|i| i.viewport().focused.unwrap_or(false))
                    {
                        egui::Area::new(egui::Id::unique(("pin-error", id)))
                            .order(egui::Order::Foreground)
                            .fixed_pos(egui::pos2(2.0, (ui.max_rect().bottom() - 60.0).max(2.0)))
                            .show(ui.ctx(), |ui| {
                                ui.set_max_width(360.0);
                                egui::Frame::popup(ui.style()).show(ui, |ui| {
                                    ui.colored_label(
                                        ui.visuals().error_fg_color,
                                        error.to_string(),
                                    );
                                });
                            });
                    }
                },
            );
            let (options_open, options_id) = match state.lock() {
                Ok(s) => (s.options_open, s.options_id),
                Err(_) => continue,
            };
            if options_open && pin.visible && !native::pins_suspended() {
                let position = pin.state.position();
                let client = client.clone();
                let options_state = state.clone();
                let scale = native::pin_window_scale(options_id).unwrap_or(native_scale);
                if let Err(error) =
                    native::sync_pin_window(options_id, position, (420, 340), true, true)
                {
                    client.report_error(error);
                }
                ctx.show_viewport_deferred(egui::ViewportId::from_hash_of(("pin-options", id)), egui::ViewportBuilder::default().with_title(viewport_title(options_id)).with_visible(false).with_taskbar(false).with_inner_size([420.0 / scale, 340.0 / scale]).with_position([position.x as f32 / scale, position.y as f32 / scale]).with_resizable(false).with_window_level(egui::WindowLevel::AlwaysOnTop), move |ui, _| {
                    let live = client.snapshot();
                    let Some(pin) = live.pins.iter().find(|p| p.id == id).filter(|_| live.lifecycle == Lifecycle::Running) else { ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close); return; };
                    if !pin.visible || native::pins_suspended() { return; }
                    ui.heading("Pin options");
                    if let Some(error) = &live.error { ui.colored_label(ui.visuals().error_fg_color, error.to_string()); }
                    ui.label(format!("Zoom {}% · Opacity {}%", pin.state.zoom_percent(), pin.state.opacity_percent()));
                    if let Some(label) = pin.image.source_label() { ui.label(label); }
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if let Some(input) = super::pin_options::draw(ui, pin.state.options()) && let Err(error) = client.submit(AppCommand::Pin { id, input }) { client.report_error(error); }
                        ui.small("Drag the corner or use wheel / + / −: zoom; Ctrl: opacity. Arrows: move 1 px; Shift: 10 px. Middle click: reset. Double click: preview.");
                        ui.small("Copy uses the original full-resolution image, including its alpha.");
                    });
                    if ui.button("Done").clicked() || ui.ctx().input(|i| i.viewport().close_requested() || i.key_pressed(Key::Escape)) {
                        if let Ok(mut state) = options_state.lock() { state.options_open = false; }
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            } else {
                native::unregister_pin_window(options_id);
            }
        }
    }
}
