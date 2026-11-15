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
    tool: super::annotation_toolbar::AnnotationTool,
    document: EditorDocument,
    text: Option<Annotation>,
    finish_visible: bool,
    cancelled: bool,
}
impl CaptureOverlay {
    pub fn new(document: EditorDocument) -> Self {
        Self {
            tool: Default::default(),
            document,
            text: None,
            finish_visible: false,
            cancelled: false,
        }
    }
    pub fn document(&self) -> &EditorDocument {
        &self.document
    }
    pub fn tool(&self) -> super::annotation_toolbar::AnnotationTool {
        self.tool
    }
    pub fn handle_chord(
        &mut self,
        saved: &crate::app::shortcuts::ShortcutSettings,
        chord: crate::app::shortcuts::KeyChord,
        focus: crate::app::shortcuts::ShortcutFocus,
        repeat: bool,
    ) -> Result<bool, AppError> {
        use crate::app::shortcuts::ShortcutFocus;
        if self.cancelled {
            return Err(AppError::Cancelled);
        }
        if focus != ShortcutFocus::Normal || repeat || self.text.is_some() {
            return Ok(false);
        }
        if chord.ctrl && !chord.alt && !chord.shift && !chord.win {
            match chord.key.vk() {
                0x5A => {
                    self.document.undo()?;
                    self.finish_visible = false;
                    return Ok(true);
                }
                0x59 => {
                    self.document.redo()?;
                    self.finish_visible = false;
                    return Ok(true);
                }
                _ => {}
            }
        }
        if let Some(tool) = saved
            .editor_action_for(chord, focus, repeat)
            .and_then(super::annotation_toolbar::AnnotationTool::from_shortcut)
        {
            self.tool = tool;
            self.finish_visible = false;
            return Ok(true);
        }
        Ok(false)
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
