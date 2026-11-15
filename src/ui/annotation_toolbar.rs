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
