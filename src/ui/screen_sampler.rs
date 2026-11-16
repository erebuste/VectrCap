//! Deferred native utility viewports. Source pixels are immutable; all external
//! actions go through the controller's typed utility command boundary.
use crate::{
    app::{
        desktop::{DesktopClient, DesktopView},
        error::AppError,
        events::{AppCommand, Lifecycle},
        shortcuts::{ShortcutAction, ShortcutFocus, ShortcutScope, ShortcutSettings},
    },
    capture::geometry::PhysicalRect,
    tools::{
        color_result::CopyFeedback,
        sampler::{SamplerState, sample_pixel},
        session::{UtilityInput, UtilityKind, UtilitySession},
    },
    ui::{capture_overlay::OverlayTransform, image_review::TiledImage},
};
use eframe::egui::{self, Color32};
use std::sync::Arc;

pub struct UtilitySurface {
    pub id: uuid::Uuid,
    pub session: UtilitySession,
    pub working: bool,
    pub copy_feedback: CopyFeedback,
    pub(crate) background: Option<TiledImage>,
    pub(crate) bounds: PhysicalRect,
    pub(crate) scale: f32,
    pub(crate) drag: Option<super::ruler_overlay::RulerDrag>,
}
impl UtilitySurface {
    pub fn new(
        id: uuid::Uuid,
        session: UtilitySession,
        bounds: PhysicalRect,
    ) -> Result<Self, AppError> {
        let snapshot = session
            .snapshot()
            .ok_or(AppError::Capture("utility source unavailable"))?;
        let scale = snapshot.monitors()[0].scale_factor() as f32;
        let background = Some(TiledImage::new(snapshot.document().source().clone())?);
        Ok(Self {
            id,
            session,
            working: false,
            copy_feedback: CopyFeedback::None,
            background,
            bounds,
            scale,
            drag: None,
        })
    }
    pub fn release_presentation(&mut self) {
        self.background = None;
        self.drag = None;
    }
    pub fn release(&mut self) {
        self.session.close();
        self.release_presentation();
    }
    fn title(&self) -> &'static str {
        if self.session.result.is_some() {
            "VectrCap sampled color"
        } else if self.session.kind == UtilityKind::Sampler {
            "VectrCap screen sampler"
        } else {
            "VectrCap pixel ruler"
        }
    }
    fn builder(&self) -> egui::ViewportBuilder {
        let builder = egui::ViewportBuilder::default()
            .with_title(self.title())
            .with_window_level(egui::WindowLevel::AlwaysOnTop);
        if self.session.result.is_some() {
            builder
                .with_inner_size([370.0, 230.0])
                .with_resizable(false)
        } else {
            builder
                .with_decorations(false)
                .with_taskbar(false)
                .with_clamp_size_to_monitor_size(false)
                .with_resizable(false)
                .with_position([
                    self.bounds.left() as f32 / self.scale,
                    self.bounds.top() as f32 / self.scale,
                ])
                .with_inner_size([
                    self.bounds.width() as f32 / self.scale,
                    self.bounds.height() as f32 / self.scale,
                ])
        }
    }
    fn show(
        &mut self,
        ui: &mut egui::Ui,
        saved: &ShortcutSettings,
    ) -> Result<Option<UtilityInput>, AppError> {
        if let Some(result) = &self.session.result {
            return Ok(super::color_result::show(ui, result));
        }
        let ctx = ui.ctx().clone();
        let viewport = ctx.viewport_rect();
        let transform = OverlayTransform::new(self.bounds, viewport, ctx.pixels_per_point())?;
        if let Some(background) = &mut self.background {
            background.paint(
                ui.painter(),
                viewport,
                (0.0, 0.0),
                1.0 / f64::from(ctx.pixels_per_point()),
            )?;
        }
        let response = ui.interact(
            viewport,
            egui::Id::unique(("utility-canvas", self.id)),
            egui::Sense::click_and_drag(),
        );
        if (response.hovered() || response.dragged())
            && ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO)
            && let Some(p) = response
                .interact_pointer_pos()
                .or_else(|| ctx.input(|i| i.pointer.hover_pos()))
                .and_then(|p| transform.physical(p))
        {
            self.session.sampler.point = p;
        }
        let mut input = None;
        let scope = if self.session.kind == UtilityKind::Sampler {
            ShortcutScope::ScreenSampler
        } else {
            ShortcutScope::Ruler
        };
        let canvas_id = egui::Id::unique(("utility-canvas", self.id));
        let focus = if !ctx.input(|i| i.focused)
            || ctx.memory(|m| m.focused().is_some_and(|id| id != canvas_id))
        {
            ShortcutFocus::TextEntry
        } else {
            ShortcutFocus::Normal
        };
        for event in ctx.input(|i| i.events.clone()) {
            if let egui::Event::Key {
                key,
                modifiers,
                pressed: true,
                repeat,
                ..
            } = event
                && let Some(chord) = super::capture_overlay::native_chord(key, modifiers)
                && let Some(action) = saved.local_action_for(scope, chord, focus, repeat)
            {
                input = local_input(action);
            }
        }
        if self.session.kind == UtilityKind::Ruler {
            if let Some(ruler) = super::ruler_overlay::show(ui, self, &response, &transform)? {
                input = Some(ruler);
            }
        } else {
            let mut lens = self.session.sampler.readout(
                self.session
                    .snapshot()
                    .ok_or(AppError::Capture("utility source unavailable"))?,
            )?;
            lens.paint_scaled(
                &ctx,
                transform.logical(self.session.sampler.point),
                viewport,
                &SamplerState::preferences(),
                ctx.pixels_per_point(),
            )?;
            if response.clicked_by(egui::PointerButton::Primary) {
                input = Some(UtilityInput::Select);
            }
            if response.clicked_by(egui::PointerButton::Secondary) {
                input = Some(UtilityInput::Close);
            }
            egui::Window::new("Frozen screen sample")
                .resizable(false)
                .default_pos(viewport.min + egui::vec2(12.0, 12.0))
                .show(&ctx, |ui| {
                    ui.label("Frozen source · physical pixels");
                    ui.label("Select a pixel to open its result. Copy is explicit.");
                    if let Some(s) = self.session.snapshot() {
                        match sample_pixel(s, self.session.sampler.point) {
                            Ok(c) => {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(48.0, 24.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(
                                    rect,
                                    2,
                                    Color32::from_rgb(c.rgb[0], c.rgb[1], c.rgb[2]),
                                );
                            }
                            Err(_) => {
                                ui.label("Sample unavailable");
                            }
                        }
                    }
                    if ui.button("Select color").clicked() {
                        input = Some(UtilityInput::Select)
                    }
                    if ui.button("Cancel").clicked() {
                        input = Some(UtilityInput::Close)
                    }
                    if let Some(error) = &self.session.error {
                        ui.colored_label(Color32::LIGHT_RED, error.to_string());
                    }
                });
        }
        Ok(input)
    }
}

pub fn local_input(action: ShortcutAction) -> Option<UtilityInput> {
    use ShortcutAction::*;
    let (dx, dy, shift) = match action {
        SamplerLeft | RulerLeft => (-1, 0, false),
        SamplerRight | RulerRight => (1, 0, false),
        SamplerUp | RulerUp => (0, -1, false),
        SamplerDown | RulerDown => (0, 1, false),
        SamplerFastLeft | RulerFastLeft => (-1, 0, true),
        SamplerFastRight | RulerFastRight => (1, 0, true),
        SamplerFastUp | RulerFastUp => (0, -1, true),
        SamplerFastDown | RulerFastDown => (0, 1, true),
        SamplerSelect | RulerSelect => return Some(UtilityInput::Select),
        SamplerClose | RulerClose => return Some(UtilityInput::Close),
        RulerHorizontal => {
            return Some(UtilityInput::SetAxis(crate::tools::ruler::Axis::Horizontal));
        }
        RulerVertical => return Some(UtilityInput::SetAxis(crate::tools::ruler::Axis::Vertical)),
        RulerToggleAxis => return Some(UtilityInput::ToggleAxis),
        RulerCopy => return Some(UtilityInput::CopySelected),
        RulerClear => return Some(UtilityInput::Clear),
        RulerRemove => return Some(UtilityInput::RemoveOrClose),
        _ => return None,
    };
    Some(UtilityInput::Move { dx, dy, shift })
}

pub fn show_host(ctx: &egui::Context, client: &DesktopClient, view: &DesktopView) {
    let Some(surface) = &view.utility_surface else {
        return;
    };
    let Ok(state) = surface.try_lock() else {
        ctx.request_repaint_after(std::time::Duration::from_millis(30));
        return;
    };
    let result = state.session.result.is_some();
    let id = state.id;
    let builder = state.builder();
    drop(state);
    let surface = surface.clone();
    let client = client.clone();
    ctx.show_viewport_deferred(
        egui::ViewportId::from_hash_of(("utility", id, result)),
        builder,
        move |ui, _| {
            let view = client.snapshot();
            if view.lifecycle != Lifecycle::Running
                || view
                    .utility_surface
                    .as_ref()
                    .is_none_or(|live| !Arc::ptr_eq(live, &surface))
            {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            let Ok(mut state) = surface.try_lock() else {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(30));
                return;
            };
            if result != state.session.result.is_some()
                || state.session.snapshot().is_none() && !result
            {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            if state.working {
                return;
            }
            if !result
                && let Err(error) = crate::platform::windows::recording_events::place_overlay(
                    state.title(),
                    state.bounds,
                )
            {
                client.report_error(error);
                drop(state);
                if let Err(e) = client.submit(AppCommand::Utility {
                    session: id,
                    input: UtilityInput::Close,
                }) {
                    client.report_error(e)
                }
                return;
            }
            let input = if ui.ctx().input(|i| i.viewport().close_requested()) {
                Some(UtilityInput::Close)
            } else if let Some(settings) = &view.settings {
                match state.show(ui, &settings.shortcuts) {
                    Ok(input) => input,
                    Err(error) => {
                        state.session.error = Some(error.clone());
                        client.report_error(error);
                        Some(UtilityInput::Close)
                    }
                }
            } else {
                None
            };
            if let Some(input) = input {
                state.working = true;
                drop(state);
                if let Err(error) = client.submit(AppCommand::Utility { session: id, input }) {
                    client.report_error(error)
                }
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{capture::geometry::MonitorGeometry, tools::UtilitySnapshot};
    #[test]
    fn canvas_click_keeps_remapped_ruler_keys_active() {
        let b = PhysicalRect::new(0, 0, 800, 600).unwrap();
        let source = UtilitySnapshot::new(
            [20, 40, 60, 255].repeat(800 * 600),
            b,
            vec![MonitorGeometry::new("fixture".into(), b, 1.0).unwrap()],
            1,
        )
        .unwrap();
        let mut state = UtilitySurface::new(
            uuid::Uuid::new_v4(),
            UtilitySession::new(
                UtilityKind::Ruler,
                source,
                crate::capture::geometry::PhysicalPoint { x: 500, y: 400 },
            ),
            b,
        )
        .unwrap();
        let ctx = egui::Context::default();
        let mut saved = ShortcutSettings::default();
        saved
            .bindings
            .iter_mut()
            .find(|b| b.action == ShortcutAction::RulerRight)
            .unwrap()
            .chord = super::super::capture_overlay::native_chord(egui::Key::P, Default::default());
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        for _ in 0..2 {
            let mut output = ctx.run_ui(raw.clone(), |ui| {
                state.show(ui, &saved).unwrap();
            });
            output.textures_delta.clear();
        }
        for pressed in [true, false] {
            let mut raw = raw.clone();
            let pos = egui::pos2(500.0, 400.0);
            raw.events = vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ];
            let mut output = ctx.run_ui(raw, |ui| {
                state.show(ui, &saved).unwrap();
            });
            output.textures_delta.clear();
        }
        ctx.memory_mut(|m| m.request_focus(egui::Id::unique(("utility-canvas", state.id))));
        let mut raw = raw.clone();
        raw.events = vec![egui::Event::Key {
            key: egui::Key::P,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }];
        let mut routed = false;
        let mut output = ctx.run_ui(raw, |ui| {
            routed = matches!(
                state.show(ui, &saved).unwrap(),
                Some(UtilityInput::Move {
                    dx: 1,
                    dy: 0,
                    shift: false
                })
            );
        });
        output.textures_delta.clear();
        assert!(routed, "canvas focus must allow its saved local shortcuts");
    }
}
