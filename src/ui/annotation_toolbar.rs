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
impl ToolOptions {
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
