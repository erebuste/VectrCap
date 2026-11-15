//! In-memory interaction state; output actions are emitted, never executed here.
use crate::{
    app::error::AppError,
    capture::geometry::PhysicalPoint,
    editor::{Annotation, EditorCommand, EditorDocument},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAction {
    Continue,
    Upload,
    Copy,
    Save,
    Pin,
    Cancel,
}
pub enum OverlayInput {
    Enter,
    Escape,
    Finish(OverlayAction),
}
pub struct CaptureOverlay {
    document: EditorDocument,
    text: Option<Annotation>,
    finish_visible: bool,
    cancelled: bool,
}
impl CaptureOverlay {
    pub fn new(document: EditorDocument) -> Self {
        Self {
            document,
            text: None,
            finish_visible: false,
            cancelled: false,
        }
    }
    pub fn document(&self) -> &EditorDocument {
        &self.document
    }
    pub fn begin_text(
        &mut self,
        position: PhysicalPoint,
        content: String,
        size: f32,
        color: [u8; 4],
    ) -> Result<(), AppError> {
        if self.cancelled {
            return Err(AppError::Cancelled);
        }
        self.text = Some(Annotation::text(position, content.into(), size, color)?);
        self.finish_visible = false;
        Ok(())
    }
    pub fn finish_visible(&self) -> bool {
        self.finish_visible
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }
    pub fn handle(&mut self, input: OverlayInput) -> Result<Option<OverlayAction>, AppError> {
        if self.cancelled {
            return Err(AppError::Cancelled);
        }
        match input {
            OverlayInput::Escape => {
                if self.text.take().is_some() {
                    return Ok(None);
                }
                self.cancelled = true;
                self.finish_visible = false;
                Ok(Some(OverlayAction::Cancel))
            }
            OverlayInput::Enter => {
                if let Some(text) = &self.text {
                    self.document.apply(EditorCommand::Insert(text.clone()))?;
                    self.text = None;
                    return Ok(None);
                }
                if self.document.crop().is_none() {
                    return Err(AppError::InvalidInput(
                        "select an output region before finishing",
                    ));
                }
                self.finish_visible = true;
                Ok(Some(OverlayAction::Continue))
            }
            OverlayInput::Finish(OverlayAction::Cancel) => self.handle(OverlayInput::Escape),
            OverlayInput::Finish(action) => {
                if self.text.is_some() {
                    return Err(AppError::InUse("commit or cancel the active edit first"));
                }
                if self.document.crop().is_none() {
                    return Err(AppError::InvalidInput(
                        "select an output region before finishing",
                    ));
                }
                Ok(Some(action))
            }
        }
    }
}
