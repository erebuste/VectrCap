//! Small egui widgets that draw the mockup's controls with the app theme.
//! Every color comes from the active `Visuals`, so a Windows contrast theme
//! (see `platform::windows::appearance`) still overrides all of them.
use super::{
    icons::{self, Icon},
    tokens::*,
};
use crate::platform::windows::appearance;
use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Frame, InnerResponse, Layout, Margin, Rect,
    Response, RichText, Role, Sense, Stroke, StrokeKind, Ui, WidgetInfo, pos2, vec2,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    Normal,
    Toggled,
    Primary,
    Ghost,
}

/// Width a small button with this icon/label takes; `label: None` is icon-only.
pub fn button_width(ui: &Ui, icon: Option<Icon>, label: Option<&str>) -> f32 {
    let text = label.map_or(0.0, |label| {
        ui.painter()
            .layout_no_wrap(
                label.to_owned(),
                FontId::proportional(SMALL_SIZE),
                Color32::PLACEHOLDER,
            )
            .size()
            .x
    });
    let icon_width = match (icon, label) {
        (Some(_), Some(_)) => ICON_SIZE + 6.0,
        (Some(_), None) => ICON_SIZE,
        _ => 0.0,
    };
    BUTTON_PAD_SMALL * 2.0 + icon_width + text
}

fn button(ui: &mut Ui, look: Look, icon: Option<Icon>, label: &str) -> Response {
    button_with(ui, look, icon, label, true, 0.0)
}

/// A small button at least `width` wide with its content centered, for button
/// grids (the History inspector's action rows). `primary` gives the lavender look.
pub fn wide_button(
    ui: &mut Ui,
    primary: bool,
    icon: Option<Icon>,
    label: &str,
    width: f32,
) -> Response {
    let look = if primary { Look::Primary } else { Look::Normal };
    button_with(ui, look, icon, label, true, width)
}

/// A filter chip (26 high, borderless until toggled on).
pub fn chip(ui: &mut Ui, selected: bool, label: &str) -> Response {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::proportional(BODY_SIZE),
        Color32::PLACEHOLDER,
    );
    let size = vec2(galley.size().x + 20.0, BUTTON_HEIGHT_SMALL);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Button, ui.is_enabled(), selected, label));
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let lit = selected || response.hovered();
        let fill = if lit {
            visuals.widgets.inactive.bg_fill
        } else {
            Color32::TRANSPARENT
        };
        let border = if selected {
            appearance::toggled_border(visuals)
        } else {
            Color32::TRANSPARENT
        };
        let text = if lit {
            visuals.text_color()
        } else {
            visuals.weak_text_color()
        };
        ui.painter().rect(
            rect,
            CornerRadius::same(RADIUS_CONTROL),
            fill,
            Stroke::new(1.0, border),
            StrokeKind::Inside,
        );
        ui.painter().galley(
            pos2(rect.left() + 10.0, rect.center().y - galley.size().y / 2.0),
            galley,
            text,
        );
    }
    response
}

fn button_with(
    ui: &mut Ui,
    look: Look,
    icon: Option<Icon>,
    label: &str,
    show_label: bool,
    min_width: f32,
) -> Response {
    let content = button_width(ui, icon, show_label.then_some(label));
    let width = content.max(min_width);
    let galley = ui.painter().layout_no_wrap(
        if show_label {
            label.to_owned()
        } else {
            String::new()
        },
        FontId::proportional(SMALL_SIZE),
        Color32::PLACEHOLDER,
    );
    let icon_width = match (icon.is_some(), show_label) {
        (true, true) => ICON_SIZE + 6.0,
        (true, false) => ICON_SIZE,
        _ => 0.0,
    };
    let size = vec2(width, BUTTON_HEIGHT_SMALL);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| {
        WidgetInfo::selected(Role::Button, ui.is_enabled(), look == Look::Toggled, label)
    });
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let state = ui.style().interact(&response);
        let (fill, border, text) = match look {
            Look::Primary => (
                visuals.selection.bg_fill,
                visuals.selection.bg_fill,
                visuals.selection.stroke.color,
            ),
            Look::Toggled => (
                visuals.widgets.inactive.bg_fill,
                appearance::toggled_border(visuals),
                visuals.text_color(),
            ),
            Look::Normal => (
                state.weak_bg_fill,
                state.bg_stroke.color,
                visuals.text_color(),
            ),
            Look::Ghost if response.hovered() => (
                visuals.widgets.inactive.bg_fill,
                Color32::TRANSPARENT,
                visuals.text_color(),
            ),
            Look::Ghost => (
                Color32::TRANSPARENT,
                Color32::TRANSPARENT,
                visuals.weak_text_color(),
            ),
        };
        let painter = ui.painter();
        painter.rect(
            rect,
            CornerRadius::same(RADIUS_CONTROL),
            fill,
            Stroke::new(1.0, border),
            StrokeKind::Inside,
        );
        let mut x = rect.left() + (width - content) / 2.0 + BUTTON_PAD_SMALL;
        if let Some(icon) = icon {
            let area = Rect::from_x_y_ranges(x..=rect.right(), rect.y_range());
            icons::paint(painter, icons::slot(area, ICON_SIZE), icon, text);
            x += icon_width;
        }
        painter.galley(
            pos2(x, rect.center().y - galley.size().y / 2.0),
            galley,
            text,
        );
    }
    response
}

/// A small bordered button; `selected` gives it the muted toggled-on look.
pub fn toggle_button(ui: &mut Ui, selected: bool, icon: Option<Icon>, label: &str) -> Response {
    button(
        ui,
        if selected {
            Look::Toggled
        } else {
            Look::Normal
        },
        icon,
        label,
    )
}

/// The one lavender action of a view.
pub fn primary_button(ui: &mut Ui, icon: Option<Icon>, label: &str) -> Response {
    button(ui, Look::Primary, icon, label)
}

/// A small bordered button with a leading icon.
pub fn icon_button(ui: &mut Ui, icon: Icon, label: &str) -> Response {
    button(ui, Look::Normal, Some(icon), label)
}

/// A borderless, muted button for secondary row and header actions.
pub fn ghost_button(ui: &mut Ui, icon: Option<Icon>, label: &str) -> Response {
    button(ui, Look::Ghost, icon, label)
}

/// An icon-only ghost button; `name` is its tooltip and screen-reader name.
pub fn ghost_icon_button(ui: &mut Ui, icon: Icon, name: &str) -> Response {
    button_with(ui, Look::Ghost, Some(icon), name, false, 0.0).on_hover_text(name)
}

/// An on/off switch (36×20), announced to screen readers as a checkbox named `label`.
pub fn toggle_switch(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let (rect, mut response) =
        ui.allocate_exact_size(vec2(SWITCH_WIDTH, SWITCH_HEIGHT), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| WidgetInfo::selected(Role::CheckBox, ui.is_enabled(), *on, label));
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let visuals = ui.visuals();
        let (track, knob) = if *on {
            (visuals.selection.bg_fill, visuals.selection.stroke.color)
        } else {
            (visuals.widgets.hovered.bg_fill, visuals.text_color())
        };
        let radius = SWITCH_HEIGHT / 2.0;
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(radius as u8), track);
        let knob_radius = radius - 2.0;
        let x = egui::lerp(
            (rect.left() + 2.0 + knob_radius)..=(rect.right() - 2.0 - knob_radius),
            t,
        );
        painter.circle_filled(pos2(x, rect.center().y), knob_radius, knob);
    }
    response
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PillTone {
    Neutral,
    Success,
    Warning,
    /// The default connection: lavender text and border.
    Default,
}

/// A small rounded status label, e.g. "Default" or "Token verified".
pub fn pill(ui: &mut Ui, text: &str, tone: PillTone) -> Response {
    let visuals = ui.visuals();
    let color = match tone {
        PillTone::Neutral => visuals.weak_text_color(),
        PillTone::Success => appearance::success(visuals),
        PillTone::Warning => appearance::warning(visuals),
        PillTone::Default => visuals.selection.bg_fill,
    };
    let border = if tone == PillTone::Default {
        color.gamma_multiply(0.4)
    } else {
        visuals.widgets.noninteractive.bg_stroke.color
    };
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), FontId::proportional(META_SIZE), color);
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley.size().x + 14.0, PILL_HEIGHT), Sense::hover());
    let painter = ui.painter();
    painter.rect_stroke(
        rect,
        CornerRadius::same((PILL_HEIGHT / 2.0) as u8),
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    painter.galley(rect.center() - galley.size() / 2.0, galley, color);
    response
}

fn rows_key(ui: &Ui) -> egui::Id {
    ui.scope_id().with("setting-rows")
}

/// A bordered surface (card fill, radius 10). Setting rows inside it get separators.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    let visuals = ui.visuals();
    Frame::new()
        .fill(visuals.widgets.noninteractive.bg_fill)
        .stroke(visuals.widgets.noninteractive.bg_stroke)
        .corner_radius(CornerRadius::same(RADIUS_SURFACE))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.set_width(ui.available_width());
            let key = rows_key(ui);
            ui.data_mut(|d| d.insert_temp(key, 0_usize));
            add(ui)
        })
}

/// Vectr's SettingGroup: title, optional description, then a card of rows.
pub fn setting_group(
    ui: &mut Ui,
    title: &str,
    description: Option<&str>,
    add_rows: impl FnOnce(&mut Ui),
) {
    let strong = ui.visuals().text_color();
    let weak = ui.visuals().weak_text_color();
    ui.label(RichText::new(title).font(semibold(BODY_SIZE)).color(strong));
    if let Some(description) = description {
        ui.label(RichText::new(description).size(BODY_SIZE).color(weak));
    }
    ui.add_space(10.0);
    card(ui, add_rows);
}

/// Vectr's SettingRow: label and description on the left, the control in a
/// 240-wide column on the right; stacked below `STACK_BELOW`.
pub fn setting_row(
    ui: &mut Ui,
    label: &str,
    description: Option<&str>,
    add_control: impl FnOnce(&mut Ui),
) -> Response {
    let strong = ui.visuals().text_color();
    let weak = ui.visuals().weak_text_color();
    setting_row_with(
        ui,
        |ui| {
            ui.label(RichText::new(label).font(medium(BODY_SIZE)).color(strong));
            if let Some(description) = description {
                ui.label(RichText::new(description).size(BODY_SIZE).color(weak));
            }
        },
        add_control,
    )
}

/// Whether this is the card's first block (no separator above it).
fn first_in_card(ui: &mut Ui) -> bool {
    let key = rows_key(ui);
    ui.data_mut(|d| {
        let seen = d.get_temp_mut_or(key, 0_usize);
        *seen += 1;
        *seen == 1
    })
}

fn separate_unless_first(ui: &Ui, first: bool, rect: Rect) {
    if !first {
        let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
        ui.painter().hline(rect.x_range(), rect.top(), stroke);
    }
}

/// A setting row whose left side is drawn by `text` (e.g. a name with pills).
pub fn setting_row_with(
    ui: &mut Ui,
    text: impl FnOnce(&mut Ui),
    add_control: impl FnOnce(&mut Ui),
) -> Response {
    let first = first_in_card(ui);
    let stacked = ui.available_width() < STACK_BELOW;
    let text = |ui: &mut Ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            text(ui);
        });
    };
    let response = Frame::new()
        .inner_margin(Margin::symmetric(ROW_PAD_X as i8, ROW_PAD_Y as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if stacked {
                text(ui);
                ui.add_space(10.0);
                ui.horizontal_wrapped(add_control);
            } else {
                ui.horizontal(|ui| {
                    let text_width = (ui.available_width() - CONTROL_COLUMN - 24.0).max(0.0);
                    // Plain vertical + with_layout: allocate_ui_with_layout pads
                    // each column, making rows ~10 pt taller than the mockup.
                    ui.vertical(|ui| {
                        ui.set_width(text_width);
                        text(ui);
                    });
                    ui.add_space(24.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), add_control);
                });
            }
        })
        .response;
    separate_unless_first(ui, first, response.rect);
    response
}

/// Free-form content inside a card, padded like a row and separated from the
/// block above it (forms, previews, multi-line results).
pub fn card_block<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let first = first_in_card(ui);
    let inner = Frame::new()
        .inner_margin(Margin::symmetric(ROW_PAD_X as i8, ROW_PAD_Y as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            add(ui)
        });
    separate_unless_first(ui, first, inner.response.rect);
    inner.inner
}

/// A card's action row: top border, 10×16 padding, controls left to right.
pub fn card_footer(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let inner = Frame::new()
        .inner_margin(Margin::symmetric(ROW_PAD_X as i8, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                add(ui);
            });
        });
    // A footer always follows rows, so it always gets the border.
    separate_unless_first(ui, false, inner.response.rect);
}

/// Muted footer/hint text (12 pt).
pub fn hint(ui: &mut Ui, text: &str) -> Response {
    let weak = ui.visuals().weak_text_color();
    ui.label(RichText::new(text).size(SMALL_SIZE).color(weak))
}

/// A Settings section entry: 32 high, muted text, muted fill when current.
pub fn section_button(ui: &mut Ui, selected: bool, label: &str, width: f32) -> Response {
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), medium(BODY_SIZE), Color32::PLACEHOLDER);
    let width = width.max(galley.size().x + 20.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 32.0), Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Button, ui.is_enabled(), selected, label));
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let lit = selected || response.hovered();
        if lit {
            ui.painter().rect_filled(
                rect,
                CornerRadius::same(RADIUS_CONTROL),
                visuals.widgets.inactive.bg_fill,
            );
        }
        let color = if lit {
            visuals.text_color()
        } else {
            visuals.weak_text_color()
        };
        ui.painter().galley(
            pos2(rect.left() + 10.0, rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::icons::Icon;
    use eframe::egui::{
        self, Color32, Context, Event, FullOutput, Pos2, RawInput, Rect, Shape, Visuals,
        output::OutputEvent,
    };

    fn screen(width: f32) -> RawInput {
        RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(width, 600.0))),
            ..Default::default()
        }
    }
    fn frame(ctx: &Context, input: RawInput, draw: impl FnMut(&mut egui::Ui)) -> FullOutput {
        let mut draw = draw;
        let mut output = ctx.run_ui(input, |ui| draw(ui));
        output.textures_delta.clear();
        output
    }
    fn click(ctx: &Context, at: Pos2, mut draw: impl FnMut(&mut egui::Ui)) -> FullOutput {
        let mut last = None;
        for pressed in [true, false] {
            let mut input = screen(600.0);
            input.events = vec![
                Event::PointerMoved(at),
                Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ];
            last = Some(frame(ctx, input, &mut draw));
        }
        last.expect("two frames ran")
    }
    fn rect_fills(output: &FullOutput) -> Vec<Color32> {
        output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                Shape::Rect(rect) => Some(rect.fill),
                _ => None,
            })
            .collect()
    }
    fn themed() -> Context {
        let ctx = Context::default();
        ctx.set_visuals(crate::platform::windows::appearance::visuals());
        ctx
    }

    #[test]
    fn toggle_switch_flips_on_click() {
        let ctx = themed();
        let mut on = false;
        let mut at = Rect::NOTHING;
        frame(&ctx, screen(600.0), |ui| {
            at = toggle_switch(ui, &mut on, "Copy the link").rect;
        });
        click(&ctx, at.center(), |ui| {
            toggle_switch(ui, &mut on, "Copy the link");
        });
        assert!(on);
    }

    #[test]
    fn toggle_switch_is_named_for_screen_readers() {
        let ctx = themed();
        let mut on = false;
        let mut at = Rect::NOTHING;
        frame(&ctx, screen(600.0), |ui| {
            at = toggle_switch(ui, &mut on, "Copy the link").rect;
        });
        let output = click(&ctx, at.center(), |ui| {
            toggle_switch(ui, &mut on, "Copy the link");
        });
        assert!(output.platform_output.events.iter().any(|event| matches!(
            event,
            OutputEvent::Clicked(info)
                if info.label.as_deref() == Some("Copy the link")
                    && info.role == egui::Role::CheckBox
        )));
    }

    #[test]
    fn selected_toggle_button_is_muted_not_lavender() {
        let ctx = themed();
        let visuals = crate::platform::windows::appearance::visuals();
        let output = frame(&ctx, screen(600.0), |ui| {
            toggle_button(ui, true, Some(Icon::Grid), "Gallery");
        });
        let fills = rect_fills(&output);
        assert!(fills.contains(&visuals.widgets.inactive.bg_fill));
        assert!(!fills.contains(&visuals.selection.bg_fill));
    }

    #[test]
    fn contrast_visuals_drive_icon_and_switch_colors() {
        let ctx = Context::default();
        let mut visuals = Visuals::dark();
        visuals.override_text_color = Some(Color32::RED);
        ctx.set_visuals(visuals);
        let mut on = false;
        let output = frame(&ctx, screen(600.0), |ui| {
            toggle_switch(ui, &mut on, "Copy the link");
            icon_button(ui, Icon::Copy, "Copy link");
        });
        let red_circle = output.shapes.iter().any(|clipped| {
            matches!(&clipped.shape, Shape::Circle(circle) if circle.fill == Color32::RED)
        });
        let red_stroke = output.shapes.iter().any(|clipped| match &clipped.shape {
            Shape::LineSegment { stroke, .. } => stroke.color == Color32::RED,
            Shape::Path(path) => {
                matches!(path.stroke.color, egui::epaint::ColorMode::Solid(c) if c == Color32::RED)
            }
            Shape::Circle(circle) => circle.stroke.color == Color32::RED,
            _ => false,
        });
        assert!(red_circle, "switch knob follows the text color");
        assert!(red_stroke, "icon strokes follow the text color");
    }

    #[test]
    fn setting_row_stacks_when_narrow() {
        let ctx = themed();
        for (width, stacked) in [(400.0, true), (900.0, false)] {
            let mut row = Rect::NOTHING;
            let mut control = Rect::NOTHING;
            frame(&ctx, screen(width), |ui| {
                row = setting_row(
                    ui,
                    "Delay before capture",
                    Some("Counts down first."),
                    |ui| {
                        control = ui.button("No delay").rect;
                    },
                )
                .rect;
            });
            let at_left = control.left() < row.left() + 40.0;
            assert_eq!(
                at_left, stacked,
                "width {width}: row {row:?} control {control:?}"
            );
        }
    }
}
