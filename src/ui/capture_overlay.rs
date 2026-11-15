//! Frozen-image editor. Input/confirmation emit typed events; artifact IO and
//! networking remain with the controller. Each completed gesture is one command.
mod gesture;
mod surface;

use super::{
    annotation_toolbar::{AnnotationTool, ToolOptions},
    image_export::ImageExportPane,
};
use crate::{
    app::{
        error::AppError,
        shortcuts::{KeyChord, ShortcutFocus, ShortcutSettings},
    },
    capture::{
        geometry::{PhysicalPoint, PhysicalRect},
        preferences::{CaptureMouseAction, MouseButton, RegionCapturePreferences},
        request::CaptureSnapshot,
        selection::{InputContext, SelectionInput, SelectionModel, SelectionOutcome},
    },
    editor::{Annotation, EditorCommand, EditorDocument, effects::SmartEraser},
};
use eframe::egui;
use gesture::EditGesture;

/// One physical pixel remains one physical pixel at every chrome zoom factor.
/// The native host must place its borderless viewport at the source origin.
#[derive(Clone, Copy)]
pub struct OverlayTransform {
    bounds: PhysicalRect,
    viewport: egui::Rect,
    pixels_per_point: f32,
    source_offset: (f64, f64),
}
impl OverlayTransform {
    pub fn new(
        bounds: PhysicalRect,
        viewport: egui::Rect,
        pixels_per_point: f32,
    ) -> Result<Self, AppError> {
        bounds.rgba_len()?;
        if !viewport.is_finite()
            || !viewport.is_positive()
            || !pixels_per_point.is_finite()
            || !(0.1..=20.0).contains(&pixels_per_point)
        {
            return Err(AppError::InvalidInput("overlay view geometry is invalid"));
        }
        Ok(Self {
            bounds,
            viewport,
            pixels_per_point,
            source_offset: (0.0, 0.0),
        })
    }
    /// Image-review zoom/pan changes presentation only. Source offsets remain
    /// f64 and are subtracted before conversion to egui's f32 coordinates.
    pub fn for_review(
        bounds: PhysicalRect,
        viewport: egui::Rect,
        source_offset: (f64, f64),
        zoom: f64,
    ) -> Result<Self, AppError> {
        bounds.rgba_len()?;
        if !viewport.is_finite()
            || !viewport.is_positive()
            || !zoom.is_finite()
            || !(0.000001..=32.0).contains(&zoom)
            || !source_offset.0.is_finite()
            || !source_offset.1.is_finite()
            || source_offset.0 < 0.0
            || source_offset.1 < 0.0
            || source_offset.0 >= f64::from(bounds.width())
            || source_offset.1 >= f64::from(bounds.height())
        {
            return Err(AppError::InvalidInput(
                "image review view geometry is invalid",
            ));
        }
        Ok(Self {
            bounds,
            viewport,
            source_offset,
            pixels_per_point: (1.0 / zoom) as f32,
        })
    }
    pub fn physical(&self, p: egui::Pos2) -> Option<PhysicalPoint> {
        if !p.x.is_finite() || !p.y.is_finite() || !self.viewport.contains(p) {
            return None;
        }
        let x = f64::from(self.bounds.left())
            + self.source_offset.0
            + f64::from(p.x - self.viewport.left()) * f64::from(self.pixels_per_point);
        let y = f64::from(self.bounds.top())
            + self.source_offset.1
            + f64::from(p.y - self.viewport.top()) * f64::from(self.pixels_per_point);
        if x < f64::from(i32::MIN)
            || x > f64::from(i32::MAX)
            || y < f64::from(i32::MIN)
            || y > f64::from(i32::MAX)
        {
            return None;
        }
        let p = PhysicalPoint {
            x: x.floor() as i32,
            y: y.floor() as i32,
        };
        self.bounds.contains(p).then_some(p)
    }
    pub fn logical(&self, p: PhysicalPoint) -> egui::Pos2 {
        self.viewport.min
            + egui::vec2(
                (((i64::from(p.x) - i64::from(self.bounds.left())) as f64 - self.source_offset.0)
                    / f64::from(self.pixels_per_point)) as f32,
                (((i64::from(p.y) - i64::from(self.bounds.top())) as f64 - self.source_offset.1)
                    / f64::from(self.pixels_per_point)) as f32,
            )
    }
    pub fn rect(&self, r: PhysicalRect) -> egui::Rect {
        egui::Rect::from_min_max(
            self.logical(PhysicalPoint {
                x: r.left(),
                y: r.top(),
            }),
            self.logical(PhysicalPoint {
                x: r.right(),
                y: r.bottom(),
            }),
        )
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAction {
    Continue,
    RunSelectedTasks,
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
pub enum CaptureOverlayEvent {
    RecordingSelected(PhysicalRect),
    Finish(super::image_export::ImageFinish),
    Cancel,
}

pub struct CaptureOverlay {
    recording_selection: bool,
    recording_summary: String,
    selection_title: &'static str,
    tool: AnnotationTool,
    last_drawing_tool: AnnotationTool,
    document: EditorDocument,
    selection: Option<SelectionModel>,
    preferences: RegionCapturePreferences,
    text: Option<Annotation>,
    text_is_modification: bool,
    text_focus_requested: bool,
    gesture: Option<EditGesture>,
    region_dragging: bool,
    hover: Option<PhysicalRect>,
    pointer: Option<PhysicalPoint>,
    selected: Option<uuid::Uuid>,
    sampled: Option<SmartEraser>,
    sampling: bool,
    options: ToolOptions,
    finish_visible: bool,
    finishing: bool,
    cancelled: bool,
    error: Option<AppError>,
    output: ImageExportPane,
    preview: surface::OverlayPreview,
    generation: u64,
    chrome_focused: bool,
}
impl CaptureOverlay {
    pub fn new(document: EditorDocument) -> Self {
        Self {
            recording_selection: false,
            recording_summary: String::new(),
            selection_title: "Select recording area",
            tool: Default::default(),
            last_drawing_tool: AnnotationTool::Freehand,
            document,
            selection: None,
            preferences: Default::default(),
            text: None,
            text_is_modification: false,
            text_focus_requested: false,
            gesture: None,
            region_dragging: false,
            hover: None,
            pointer: None,
            selected: None,
            sampled: None,
            sampling: false,
            options: Default::default(),
            finish_visible: false,
            finishing: false,
            cancelled: false,
            error: None,
            output: Default::default(),
            preview: Default::default(),
            generation: 0,
            chrome_focused: false,
        }
    }
    pub fn from_snapshot(
        snapshot: CaptureSnapshot,
        pointer: PhysicalPoint,
        threshold: (u32, u32),
    ) -> Result<Self, AppError> {
        let preferences = snapshot.preferences().clone();
        // Freeze the inventory first, then transfer the original pixel Vec.
        let selection = SelectionModel::new(&snapshot, preferences.clone(), pointer, threshold)?;
        let mut overlay = Self::new(EditorDocument::new(snapshot)?);
        overlay.selection = Some(selection);
        overlay.preferences = preferences;
        overlay.pointer = Some(pointer);
        Ok(overlay)
    }
    pub fn set_recording_summary(&mut self, summary: String) {
        self.recording_summary = summary;
    }
    pub fn set_scrolling_selection(&mut self) {
        self.set_recording_selection();
        self.selection_title = "Select scrolling viewport";
    }
    pub fn set_recording_selection(&mut self) {
        self.recording_selection = true;
        self.tool = AnnotationTool::Region;
    }
    pub fn document(&self) -> &EditorDocument {
        &self.document
    }
    pub fn tool(&self) -> AnnotationTool {
        self.tool
    }
    pub fn error(&self) -> Option<&AppError> {
        self.error.as_ref()
    }
    pub fn has_active_edit(&self) -> bool {
        self.text.is_some() || self.gesture.is_some() || self.region_dragging || self.sampling
    }
    pub fn finish_visible(&self) -> bool {
        self.finish_visible
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }
    fn editable(&self) -> Result<(), AppError> {
        if self.cancelled {
            Err(AppError::Cancelled)
        } else if self.finishing {
            Err(AppError::InUse("an image finish is already in progress"))
        } else {
            Ok(())
        }
    }
    fn changed(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.finish_visible = false;
    }
    fn sync_selection(&mut self) -> Result<(), AppError> {
        if let Some(selection) = &mut self.selection {
            selection.synchronize_crop(self.document.crop())?;
        }
        self.hover = None;
        Ok(())
    }
    fn apply(&mut self, command: EditorCommand) -> Result<(), AppError> {
        self.document
            .apply(command)
            .inspect_err(|e| self.error = Some(e.clone()))?;
        self.changed();
        self.error = None;
        self.sync_selection()
    }
    pub fn apply_command(&mut self, command: EditorCommand) -> Result<(), AppError> {
        self.editable()?;
        if self.has_active_edit() {
            return Err(AppError::InUse(
                "complete the active edit before applying a command",
            ));
        }
        self.apply(command)
    }
    pub fn set_tool(&mut self, tool: AnnotationTool) -> Result<(), AppError> {
        self.editable()?;
        if self.recording_selection && tool != AnnotationTool::Region {
            return Err(AppError::InvalidInput(
                "recording selection only supports the region tool",
            ));
        }
        if self.has_active_edit() {
            return Err(AppError::InUse("complete or cancel the active edit first"));
        }
        self.tool = tool;
        if !matches!(tool, AnnotationTool::Region | AnnotationTool::Select) {
            self.last_drawing_tool = tool;
        }
        if let Some(selection) = &mut self.selection {
            selection.handle(SelectionInput::Context(if tool == AnnotationTool::Region {
                InputContext::Region
            } else {
                InputContext::Drawing
            }));
        }
        self.hover = None;
        self.finish_visible = false;
        Ok(())
    }
    pub fn clear_crop(&mut self) -> Result<(), AppError> {
        self.editable()?;
        if self.has_active_edit() {
            return Err(AppError::InUse("complete or cancel the active edit first"));
        }
        self.apply(EditorCommand::Crop(None))
    }
    pub fn handle_chord(
        &mut self,
        saved: &ShortcutSettings,
        chord: KeyChord,
        focus: ShortcutFocus,
        repeat: bool,
    ) -> Result<bool, AppError> {
        self.editable()?;
        if focus != ShortcutFocus::Normal || repeat || self.has_active_edit() {
            return Ok(false);
        }
        if chord.ctrl
            && !chord.alt
            && !chord.shift
            && !chord.win
            && matches!(chord.key.vk(), 0x5A | 0x59)
        {
            if chord.key.vk() == 0x5A {
                self.document.undo()?;
            } else {
                self.document.redo()?;
            }
            self.changed();
            self.sync_selection()?;
            if self
                .selected
                .is_some_and(|id| !self.document.annotations().iter().any(|a| a.id() == id))
            {
                self.selected = None;
            }
            return Ok(true);
        }
        if let Some(tool) = saved
            .editor_action_for(chord, focus, repeat)
            .and_then(AnnotationTool::from_shortcut)
        {
            self.set_tool(tool)?;
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
        self.editable()?;
        if self.has_active_edit() {
            return Err(AppError::InUse("complete or cancel the active edit first"));
        }
        self.text = Some(Annotation::text(position, content.into(), size, color)?);
        self.text_is_modification = false;
        self.text_focus_requested = true;
        self.finish_visible = false;
        Ok(())
    }
    pub fn handle(&mut self, input: OverlayInput) -> Result<Option<OverlayAction>, AppError> {
        self.editable()?;
        match input {
            OverlayInput::Escape => {
                if self.has_active_edit() {
                    self.text = None;
                    self.gesture = None;
                    self.region_dragging = false;
                    self.sampling = false;
                    self.changed();
                    self.sync_selection()?;
                    self.error = None;
                    return Ok(None);
                }
                self.cancelled = true;
                self.finish_visible = false;
                Ok(Some(OverlayAction::Cancel))
            }
            OverlayInput::Enter => {
                if let Some(text) = &self.text {
                    let command = if self.text_is_modification {
                        EditorCommand::Modify(text.clone())
                    } else {
                        EditorCommand::Insert(text.clone())
                    };
                    self.apply(command)?;
                    self.text = None;
                    return Ok(None);
                }
                if self.has_active_edit() {
                    return Err(AppError::InUse("complete or cancel the active edit first"));
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
                if self.has_active_edit() {
                    return Err(AppError::InUse("complete or cancel the active edit first"));
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
    /// Failed downstream IO returns to this exact document and output settings.
    pub fn finish_failed(&mut self, error: AppError) {
        self.finishing = false;
        self.output.set_finishing(false);
        self.error = Some(error);
    }
    /// Recheck at the controller boundary, including edits made while a failed
    /// archive was waiting for an explicit retry/continue decision.
    pub fn validate_finish(
        &self,
        finish: &super::image_export::ImageFinish,
    ) -> Result<(), AppError> {
        if self.cancelled
            || self.has_active_edit()
            || finish.screen_location != self.document.screen_location()
            || !finish
                .prepared
                .matches(&self.document, self.output.options())
            || self.output.ready(&self.document).is_none()
        {
            return Err(AppError::InvalidInput(
                "image finish is stale; confirm the current output",
            ));
        }
        Ok(())
    }
    pub fn output_mut(&mut self) -> &mut ImageExportPane {
        &mut self.output
    }
    pub fn prepare_output(&mut self, wake: impl FnOnce() + Send + 'static) {
        self.output.update(&self.document, wake);
    }
    pub fn confirm_finish(
        &mut self,
        action: OverlayAction,
    ) -> Result<super::image_export::ImageFinish, AppError> {
        if self.has_active_edit() || self.cancelled || self.finishing {
            return Err(AppError::InUse("complete the active edit before finishing"));
        }
        let finish = self.output.confirm(&self.document, action)?;
        self.finishing = true;
        self.output.set_finishing(true);
        Ok(finish)
    }
    fn selection_outcome(
        &mut self,
        outcome: SelectionOutcome,
        commit: bool,
    ) -> Result<(), AppError> {
        match outcome {
            SelectionOutcome::Hover(r) => self.hover = r,
            SelectionOutcome::Selected(r) | SelectionOutcome::Changed(r) if commit => {
                if let Err(error) = self.apply(EditorCommand::Crop(Some(r))) {
                    self.sync_selection()?;
                    return Err(error);
                }
            }
            SelectionOutcome::Rejected(error) => {
                self.error = Some(error.clone());
                return Err(error);
            }
            _ => {}
        }
        Ok(())
    }
    fn auxiliary(&mut self, button: MouseButton) -> Result<(), AppError> {
        if self.preferences.mouse.action(button) == CaptureMouseAction::CancelActiveOrCapture {
            self.handle(OverlayInput::Escape)?;
            return Ok(());
        }
        if self.has_active_edit() || self.chrome_focused {
            return Ok(());
        }
        match self.preferences.mouse.action(button) {
            CaptureMouseAction::ToggleSelectionDrawing => {
                self.set_tool(if self.tool == AnnotationTool::Region {
                    self.last_drawing_tool
                } else {
                    AnnotationTool::Region
                })
            }
            _ => {
                if let Some(selection) = &mut self.selection {
                    let outcome = selection.handle(SelectionInput::Auxiliary(button));
                    self.selection_outcome(outcome, true)?;
                }
                Ok(())
            }
        }
    }
}
