//! Frozen-image editor. Input/confirmation emit typed events; artifact IO and
//! networking remain with the controller. Each completed gesture is one command.
mod gesture;
mod surface;
mod toolbar;

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
pub(crate) use gesture::native_chord;

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
    toolbar_monitor: Option<PhysicalRect>,
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
    /// The drawing defaults this capture started from (None in tests/fixtures).
    saved_tools: Option<crate::persistence::settings::ToolPreferences>,
    finish_visible: bool,
    finish_tasks_on_release: bool,
    release_runs_tasks: bool,
    finished_on_release: bool,
    finishing: bool,
    cancelled: bool,
    error: Option<AppError>,
    output: ImageExportPane,
    preview: surface::OverlayPreview,
    generation: u64,
    chrome_focused: bool,
    toolbar_hidden: bool,
    /// Wheel changes to the magnifier for this capture only (ShareX-style).
    magnifier: MagnifierWheel,
}
/// Wheel = magnifier size, Ctrl+wheel = zoom. Shrinking past the smallest
/// size hides it; scrolling up brings it back.
#[derive(Default, Clone, Copy)]
pub(crate) struct MagnifierWheel {
    pub hidden: bool,
    pub scale: Option<u8>,
    pub count: Option<u8>,
}
impl MagnifierWheel {
    pub(crate) fn scroll(&mut self, up: bool, ctrl: bool, prefs: &RegionCapturePreferences) {
        if self.hidden {
            self.hidden = !up;
            return;
        }
        // As in ShareX: the wheel shows more/fewer pixels at the same pixel size
        // (the lens grows/shrinks); Ctrl+wheel zooms within the same lens size.
        let count = self.count.unwrap_or(prefs.lens_source_count);
        let scale = self.scale.unwrap_or(prefs.lens_scale);
        if ctrl {
            let next = if up { count - 2 } else { count + 2 }.clamp(9, 35);
            let size = f32::from(count) * f32::from(scale);
            self.count = Some(next);
            self.scale = Some((size / f32::from(next)).round().clamp(4.0, 24.0) as u8);
        } else if !up && count <= 9 {
            self.hidden = true;
        } else {
            self.count = Some(if up { count + 2 } else { count - 2 }.clamp(9, 35));
        }
    }
    /// The preferences the magnifier draws with.
    pub(crate) fn apply(&self, prefs: &RegionCapturePreferences) -> RegionCapturePreferences {
        let mut p = prefs.clone();
        p.show_magnifier &= !self.hidden;
        p.lens_scale = self.scale.unwrap_or(p.lens_scale);
        p.lens_source_count = self.count.unwrap_or(p.lens_source_count);
        p
    }
}
impl CaptureOverlay {
    pub fn new(document: EditorDocument) -> Self {
        Self {
            recording_selection: false,
            recording_summary: String::new(),
            toolbar_monitor: None,
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
            saved_tools: None,
            finish_visible: false,
            finish_tasks_on_release: false,
            release_runs_tasks: true,
            finished_on_release: false,
            finishing: false,
            cancelled: false,
            error: None,
            output: Default::default(),
            preview: Default::default(),
            generation: 0,
            chrome_focused: false,
            toolbar_hidden: false,
            magnifier: MagnifierWheel::default(),
        }
    }
    pub fn initialize_preferences(
        &mut self,
        settings: &crate::persistence::settings::Settings,
    ) -> Result<(), AppError> {
        settings.tools.validate()?;
        *self.output.options_mut() = settings.image.options()?;
        self.options = ToolOptions::from_preferences(&settings.tools);
        self.saved_tools = Some(settings.tools.clone());
        Ok(())
    }
    /// The drawing defaults to save when this capture ends, if the user
    /// changed any (e.g. picked another color) and they are valid.
    pub fn changed_tool_preferences(
        &self,
    ) -> Option<crate::persistence::settings::ToolPreferences> {
        let saved = self.saved_tools.as_ref()?;
        let now = self.options.to_preferences();
        (*saved != now && now.validate().is_ok()).then_some(now)
    }
    pub fn from_snapshot(
        snapshot: CaptureSnapshot,
        pointer: PhysicalPoint,
        threshold: (u32, u32),
    ) -> Result<Self, AppError> {
        let preferences = snapshot.preferences().clone();
        let toolbar_monitor = snapshot
            .monitors()
            .iter()
            .find(|m| m.bounds().contains(pointer))
            .and_then(|m| m.bounds().intersection(snapshot.bounds()));
        // Freeze the inventory first, then transfer the original pixel Vec.
        let selection = SelectionModel::new(&snapshot, preferences.clone(), pointer, threshold)?;
        let mut overlay = Self::new(EditorDocument::new(snapshot)?);
        overlay.selection = Some(selection);
        overlay.toolbar_monitor = toolbar_monitor;
        overlay.preferences = preferences;
        overlay.pointer = Some(pointer);
        Ok(overlay)
    }
    /// Pin-from-screen keeps explicit image output choices instead of invoking screenshot tasks.
    pub fn finished_on_release(&self) -> bool {
        self.finished_on_release
    }
    pub fn retain_editor_after_selection(&mut self) {
        self.release_runs_tasks = false;
    }
    pub fn set_recording_summary(&mut self, summary: String) {
        self.recording_summary = summary;
    }
    pub fn set_scrolling_selection(&mut self) {
        self.set_recording_selection();
        self.selection_title = "Select the part of a window to scroll";
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
        self.finish_tasks_on_release = false;
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
                self.finish_tasks_on_release = false;
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
                // Like ShareX: clear a selected annotation before closing.
                if self.tool == AnnotationTool::Select && self.selected.take().is_some() {
                    self.changed();
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
            OverlayInput::Finish(OverlayAction::Cancel) => {
                // The Cancel button closes; only Esc/right-click deselect first.
                self.selected = None;
                self.handle(OverlayInput::Escape)
            }
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
        self.finish_tasks_on_release = false;
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
    /// Releasing a drawn region, Space or `~` confirms a new selection.
    /// Recording/scrolling keep their explicit Start/countdown boundary.
    fn confirm_selection(&mut self, outcome: SelectionOutcome) -> Result<(), AppError> {
        let selected = matches!(
            outcome,
            SelectionOutcome::Selected(_) | SelectionOutcome::Changed(_)
        );
        self.selection_outcome(outcome, true)?;
        if selected && !self.recording_selection {
            self.handle(OverlayInput::Enter)?;
            self.finish_tasks_on_release = self.release_runs_tasks;
        }
        Ok(())
    }
    fn auxiliary(
        &mut self,
        button: MouseButton,
        at: Option<PhysicalPoint>,
    ) -> Result<(), AppError> {
        if self.preferences.mouse.action(button) == CaptureMouseAction::CancelActiveOrCapture {
            // ShareX-style right-click: cancel the active edit, else remove what's
            // under the pointer (the region or an annotation), else close.
            if !self.has_active_edit()
                && !self.chrome_focused
                && let Some(p) = at
            {
                if self.tool == AnnotationTool::Region {
                    if self.document.crop().is_some_and(|c| c.contains(p)) {
                        return self.apply(EditorCommand::Crop(None));
                    }
                } else if let Some(id) =
                    crate::editor::eraser::erase_annotation_hits(&self.document, &[p], 2.0)?
                        .last()
                        .copied()
                {
                    self.apply(EditorCommand::Erase(vec![id]))?;
                    if self.selected == Some(id) {
                        self.selected = None;
                    }
                    return Ok(());
                }
            }
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
            action => {
                // Like ShareX, whole-desktop/monitor buttons take the shot; the
                // monitor is the one under the pointer, not the launch monitor.
                let input = match (action, at) {
                    (CaptureMouseAction::SelectActiveMonitor, Some(p)) => {
                        SelectionInput::SelectMonitorAt(p)
                    }
                    _ => SelectionInput::Auxiliary(button),
                };
                if let Some(selection) = &mut self.selection {
                    let outcome = selection.handle(input);
                    self.confirm_selection(outcome)?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_resizes_ctrl_wheel_zooms_and_shrinking_past_the_minimum_hides() {
        let prefs = RegionCapturePreferences::default();
        let mut m = MagnifierWheel::default();
        // Wheel up: two more pixels, same pixel size.
        m.scroll(true, false, &prefs);
        let p = m.apply(&prefs);
        assert_eq!(
            (p.lens_source_count, p.lens_scale),
            (prefs.lens_source_count + 2, prefs.lens_scale)
        );
        // Ctrl+wheel up: fewer, bigger pixels in about the same lens size.
        m.scroll(true, true, &prefs);
        let q = m.apply(&prefs);
        assert_eq!(q.lens_source_count, p.lens_source_count - 2);
        assert!(q.lens_scale > p.lens_scale);
        let (before, after) = (
            u32::from(p.lens_source_count) * u32::from(p.lens_scale),
            u32::from(q.lens_source_count) * u32::from(q.lens_scale),
        );
        assert!(before.abs_diff(after) <= u32::from(q.lens_source_count));
        for _ in 0..20 {
            m.scroll(false, false, &prefs);
        }
        assert!(!m.apply(&prefs).show_magnifier);
        m.scroll(true, false, &prefs);
        assert!(m.apply(&prefs).show_magnifier);
        assert!(m.apply(&prefs).validate().is_ok());
    }
}
