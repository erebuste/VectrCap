//! Editor tool identities and saved-action mapping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnnotationTool {
    #[default]
    Region,
    Select,
    Rectangle,
    Ellipse,
    Line,
    Arrow,
    Freehand,
    Text,
    Step,
    Highlight,
    Spotlight,
    Redaction,
    AnnotationEraser,
    SmartEraser,
}
impl AnnotationTool {
    pub fn all() -> &'static [Self] {
        use AnnotationTool::*;
        &[
            Region,
            Select,
            Rectangle,
            Ellipse,
            Line,
            Arrow,
            Freehand,
            Text,
            Step,
            Highlight,
            Spotlight,
            Redaction,
            AnnotationEraser,
            SmartEraser,
        ]
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Region => "Region / crop",
            Self::Select => "Select / move",
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Line => "Line",
            Self::Arrow => "Arrow",
            Self::Freehand => "Freehand",
            Self::Text => "Text",
            Self::Step => "Step",
            Self::Highlight => "Highlight",
            Self::Spotlight => "Spotlight",
            Self::Redaction => "Solid redaction",
            Self::AnnotationEraser => "Annotation eraser",
            Self::SmartEraser => "Smart eraser",
        }
    }
    pub fn from_shortcut(action: crate::app::shortcuts::ShortcutAction) -> Option<Self> {
        use crate::app::shortcuts::ShortcutAction as A;
        Some(match action {
            A::ToolRegion => Self::Region,
            A::ToolRectangle => Self::Rectangle,
            A::ToolEllipse => Self::Ellipse,
            A::ToolLine => Self::Line,
            A::ToolArrow => Self::Arrow,
            A::ToolFreehand => Self::Freehand,
            A::ToolText => Self::Text,
            A::ToolStep => Self::Step,
            A::ToolHighlight => Self::Highlight,
            A::ToolSpotlight => Self::Spotlight,
            A::ToolAnnotationEraser => Self::AnnotationEraser,
            A::ToolSmartEraser => Self::SmartEraser,
            _ => return None,
        })
    }
}

pub(super) struct ToolOptions {
    pub color: [u8; 3],
    pub fill_color: [u8; 3],
    pub fill: bool,
    pub width: f32,
    pub text_size: f32,
    pub step_radius: f32,
    pub highlight_color: [u8; 3],
    pub opacity: f32,
    pub dim: f32,
    pub eraser_width: f32,
}
impl Default for ToolOptions {
    fn default() -> Self {
        Self {
            color: [255, 60, 60],
            fill_color: [255, 60, 60],
            fill: false,
            width: 3.0,
            text_size: 24.0,
            step_radius: 16.0,
            highlight_color: [255, 255, 0],
            opacity: 0.35,
            dim: 0.6,
            eraser_width: 20.0,
        }
    }
}
/// Quick colors for drawings, with names for tooltips and screen readers.
pub(super) const PALETTE: [[u8; 3]; 8] = [
    [255, 60, 60],
    [255, 150, 40],
    [255, 220, 0],
    [60, 200, 90],
    [50, 130, 255],
    [180, 100, 255],
    [255, 255, 255],
    [20, 20, 20],
];
const PALETTE_NAMES: [&str; 8] = [
    "Red", "Orange", "Yellow", "Green", "Blue", "Purple", "White", "Black",
];

/// The preset swatches plus a custom color picker for `color`.
/// Returns whether the color changed.
pub(super) fn color_palette(ui: &mut eframe::egui::Ui, color: &mut [u8; 3]) -> bool {
    use eframe::egui::{self, Stroke, StrokeKind};
    let mut changed = false;
    // Always left to right (red first), even inside a right-to-left column.
    // One row high: `with_layout` would take all the height there is (the
    // whole screen inside a capture overlay popup) and centre the row in it.
    let row = egui::vec2(ui.available_width(), ui.spacing().interact_size.y.max(20.0));
    let layout = egui::Layout::left_to_right(egui::Align::Center);
    ui.allocate_ui_with_layout(row, layout, |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (&preset, name) in PALETTE.iter().zip(PALETTE_NAMES) {
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
            let current = *color == preset;
            response.widget_info(|| {
                egui::WidgetInfo::selected(egui::Role::Button, true, current, name)
            });
            let border = if current {
                Stroke::new(2.0, ui.visuals().text_color())
            } else {
                ui.visuals().widgets.noninteractive.bg_stroke
            };
            ui.painter().rect(
                rect,
                egui::CornerRadius::same(4),
                egui::Color32::from_rgb(preset[0], preset[1], preset[2]),
                border,
                StrokeKind::Outside,
            );
            if response.on_hover_text(name).clicked() && !current {
                *color = preset;
                changed = true;
            }
        }
        ui.add_space(4.0);
        changed |= ui
            .color_edit_button_srgb(color)
            .on_hover_text("Custom color")
            .changed();
    });
    changed
}

impl ToolOptions {
    pub fn from_preferences(t: &crate::persistence::settings::ToolPreferences) -> Self {
        Self {
            color: t.color,
            fill_color: t.fill_color,
            fill: t.fill,
            width: f32::from(t.width_tenths) / 10.0,
            text_size: f32::from(t.text_size),
            step_radius: f32::from(t.step_radius),
            highlight_color: t.highlight_color,
            opacity: f32::from(t.opacity_percent) / 100.0,
            dim: f32::from(t.dim_percent) / 100.0,
            eraser_width: f32::from(t.eraser_width),
        }
    }
    /// The saved form of these options (integer units, as settings store them).
    pub fn to_preferences(&self) -> crate::persistence::settings::ToolPreferences {
        let units =
            |value: f32, scale: f32| (value * scale).round().clamp(0.0, f32::from(u16::MAX)) as u16;
        let percent = |value: f32| (value * 100.0).round().clamp(0.0, 100.0) as u8;
        crate::persistence::settings::ToolPreferences {
            color: self.color,
            fill_color: self.fill_color,
            fill: self.fill,
            width_tenths: units(self.width, 10.0),
            text_size: units(self.text_size, 1.0),
            step_radius: units(self.step_radius, 1.0),
            highlight_color: self.highlight_color,
            opacity_percent: percent(self.opacity),
            dim_percent: percent(self.dim),
            eraser_width: units(self.eraser_width, 1.0),
        }
    }
    pub fn rgba(&self) -> [u8; 4] {
        [self.color[0], self.color[1], self.color[2], 255]
    }
    pub fn style(&self) -> Result<crate::editor::Style, crate::app::error::AppError> {
        crate::editor::Style::new(
            self.rgba(),
            self.width,
            self.fill.then_some([
                self.fill_color[0],
                self.fill_color[1],
                self.fill_color[2],
                255,
            ]),
        )
    }
    pub fn draw(&mut self, ui: &mut eframe::egui::Ui, tool: AnnotationTool) {
        use AnnotationTool::*;
        use eframe::egui::Slider;
        ui.horizontal_wrapped(|ui| {
            if !matches!(
                tool,
                Region | Select | Spotlight | Highlight | AnnotationEraser | SmartEraser
            ) {
                ui.label("Color");
                ui.color_edit_button_srgb(&mut self.color);
            }
            if matches!(tool, Rectangle | Ellipse | Line | Arrow | Freehand) {
                ui.add(Slider::new(&mut self.width, 0.5..=64.0).text("Width (px)"));
            }
            if matches!(tool, Rectangle | Ellipse) {
                ui.checkbox(&mut self.fill, "Fill");
                if self.fill {
                    ui.label("Fill color");
                    ui.color_edit_button_srgb(&mut self.fill_color);
                }
            }
            match tool {
                Text => {
                    ui.add(Slider::new(&mut self.text_size, 4.0..=512.0).text("Text size (px)"));
                }
                Step => {
                    ui.add(
                        Slider::new(&mut self.step_radius, 4.0..=512.0).text("Marker radius (px)"),
                    );
                }
                Highlight => {
                    ui.label("Highlight color");
                    ui.color_edit_button_srgb(&mut self.highlight_color);
                    ui.add(Slider::new(&mut self.opacity, 0.0..=1.0).text("Opacity"));
                }
                Spotlight => {
                    ui.add(Slider::new(&mut self.dim, 0.0..=1.0).text("Outside dimming"));
                }
                AnnotationEraser | SmartEraser => {
                    ui.add(
                        Slider::new(&mut self.eraser_width, 1.0..=1024.0).text("Brush size (px)"),
                    );
                }
                _ => {}
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::settings::ToolPreferences;
    use eframe::egui;

    #[test]
    fn options_round_trip_to_saved_defaults() {
        let saved = ToolPreferences {
            color: [10, 20, 30],
            width_tenths: 45,
            ..Default::default()
        };
        assert_eq!(
            ToolOptions::from_preferences(&saved).to_preferences(),
            saved
        );
        assert_eq!(
            ToolOptions::default().to_preferences(),
            ToolPreferences::default()
        );
    }

    #[test]
    fn the_palette_is_one_row_high_however_tall_the_space() {
        let ctx = egui::Context::default();
        let mut height = 0.0;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let mut color = [0, 0, 0];
            height = ui
                .scope(|ui| color_palette(ui, &mut color))
                .response
                .rect
                .height();
        });
        output.textures_delta.clear();
        assert!(height < 40.0, "palette is {height} pt tall");
    }

    #[test]
    fn clicking_a_swatch_picks_its_color() {
        let ctx = egui::Context::default();
        let mut color = [0, 0, 0];
        let mut swatch = egui::Rect::NOTHING;
        let mut changed = false;
        for pressed in [None, Some(true), Some(false)] {
            let mut input = egui::RawInput::default();
            if let Some(pressed) = pressed {
                let at = swatch.center();
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
            let mut output = ctx.run_ui(input, |ui| {
                changed |= color_palette(ui, &mut color);
            });
            output.textures_delta.clear();
            // Find the Orange swatch where it was painted.
            let orange = egui::Color32::from_rgb(PALETTE[1][0], PALETTE[1][1], PALETTE[1][2]);
            if let Some(rect) = output
                .shapes
                .iter()
                .find_map(|clipped| match &clipped.shape {
                    egui::Shape::Rect(shape) if shape.fill == orange => Some(shape.rect),
                    _ => None,
                })
            {
                swatch = rect;
            }
        }
        assert!(changed);
        assert_eq!(color, PALETTE[1]);
    }
}
