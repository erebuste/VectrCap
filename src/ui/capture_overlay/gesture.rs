use super::*;
use crate::editor::{
    document::MAX_STROKE_POINTS,
    eraser::erase_annotation_hits,
    shapes::{constrained_segment, constrained_shape},
};

pub(super) struct EditGesture {
    tool: AnnotationTool,
    start: PhysicalPoint,
    end: PhysicalPoint,
    points: Vec<PhysicalPoint>,
    shift: bool,
    original: Option<Annotation>,
    blocked: Option<AppError>,
}
impl EditGesture {
    pub(super) fn command(
        &self,
        owner: &CaptureOverlay,
    ) -> Result<Option<EditorCommand>, AppError> {
        if let Some(error) = &self.blocked {
            return Err(error.clone());
        }
        let o = &owner.options;
        let annotation = match self.tool {
            AnnotationTool::Select => {
                return self
                    .original
                    .as_ref()
                    .map(|a| {
                        let dx = i32::try_from(i64::from(self.end.x) - i64::from(self.start.x))
                            .map_err(|_| AppError::InvalidInput("annotation movement overflows"))?;
                        let dy = i32::try_from(i64::from(self.end.y) - i64::from(self.start.y))
                            .map_err(|_| AppError::InvalidInput("annotation movement overflows"))?;
                        Ok(EditorCommand::Modify(a.translated(dx, dy)?))
                    })
                    .transpose();
            }
            AnnotationTool::AnnotationEraser => {
                return Ok(Some(EditorCommand::Erase(erase_annotation_hits(
                    &owner.document,
                    &self.points,
                    o.eraser_width / 2.0,
                )?)));
            }
            AnnotationTool::SmartEraser => owner
                .sampled
                .as_ref()
                .ok_or(AppError::InvalidInput(
                    "sample a frozen source color before painting",
                ))?
                .stroke(self.points.clone().into(), o.eraser_width)?,
            AnnotationTool::Freehand => {
                Annotation::freehand(self.points.clone().into(), o.rgba(), o.width)?
            }
            AnnotationTool::Line => Annotation::line(
                self.start,
                constrained_segment(self.start, self.end, self.shift)?,
                o.rgba(),
                o.width,
            )?,
            AnnotationTool::Arrow => Annotation::arrow(
                self.start,
                constrained_segment(self.start, self.end, self.shift)?,
                o.rgba(),
                o.width,
            )?,
            AnnotationTool::Step => {
                return Ok(Some(EditorCommand::Step {
                    center: self.start,
                    radius: o.step_radius,
                    color: o.rgba(),
                }));
            }
            AnnotationTool::Rectangle => Annotation::rectangle(
                constrained_shape(self.start, self.end, self.shift)?,
                o.style()?,
            )?,
            AnnotationTool::Ellipse => Annotation::ellipse(
                constrained_shape(self.start, self.end, self.shift)?,
                o.style()?,
            )?,
            AnnotationTool::Highlight => Annotation::highlight(
                PhysicalRect::from_points(self.start, self.end)?,
                o.highlight_color,
                o.opacity,
            )?,
            AnnotationTool::Spotlight => {
                Annotation::spotlight(PhysicalRect::from_points(self.start, self.end)?, o.dim)?
            }
            AnnotationTool::Redaction => {
                Annotation::redaction(PhysicalRect::from_points(self.start, self.end)?, o.color)?
            }
            AnnotationTool::Region | AnnotationTool::Text => return Ok(None),
        };
        Ok(Some(EditorCommand::Insert(annotation)))
    }
}
impl CaptureOverlay {
    pub fn sample_smart_eraser(&mut self, p: PhysicalPoint) -> Result<(), AppError> {
        self.editable()?;
        self.sampled = Some(SmartEraser::sample(&self.document, p)?);
        self.sampling = false;
        self.error = None;
        Ok(())
    }
    pub fn pointer_down(&mut self, p: PhysicalPoint) -> Result<(), AppError> {
        self.editable()?;
        if self.sampling {
            return self.sample_smart_eraser(p);
        }
        if self.has_active_edit() {
            return Err(AppError::InUse("complete or cancel the active edit first"));
        }
        crate::editor::sample_source_color(&self.document, p)?;
        self.pointer = Some(p);
        self.finish_visible = false;
        if self.tool == AnnotationTool::Region {
            if let Some(selection) = &mut self.selection {
                let outcome = selection.handle(SelectionInput::PrimaryDown {
                    point: p,
                    context: InputContext::Region,
                });
                self.selection_outcome(outcome, false)?;
                self.region_dragging = true;
            } else {
                self.gesture = Some(EditGesture {
                    tool: AnnotationTool::Region,
                    start: p,
                    end: p,
                    points: vec![p],
                    shift: false,
                    original: None,
                    blocked: None,
                });
            }
        } else if self.tool == AnnotationTool::Text {
            // Empty drafts are valid UI state, but never document commands.
            self.text = Some(Annotation::Text {
                id: uuid::Uuid::new_v4(),
                position: p,
                content: "".into(),
                size: self.options.text_size,
                color: self.options.rgba(),
            });
            self.text_is_modification = false;
            self.text_focus_requested = true;
        } else {
            if self.tool == AnnotationTool::SmartEraser && self.sampled.is_none() {
                return Err(AppError::InvalidInput(
                    "sample a frozen source color before painting",
                ));
            }
            let original = if self.tool == AnnotationTool::Select {
                let id = erase_annotation_hits(&self.document, &[p], 2.0)?
                    .last()
                    .copied();
                self.selected = id;
                self.document
                    .annotations()
                    .iter()
                    .find(|a| Some(a.id()) == id)
                    .cloned()
            } else {
                None
            };
            self.gesture = Some(EditGesture {
                tool: self.tool,
                start: p,
                end: p,
                points: vec![p],
                shift: false,
                original,
                blocked: None,
            });
        }
        self.changed();
        Ok(())
    }
    pub fn pointer_move(&mut self, p: PhysicalPoint, shift: bool) -> Result<(), AppError> {
        self.editable()?;
        self.pointer = Some(p);
        if let Some(selection) = &mut self.selection {
            let outcome = selection.handle(SelectionInput::PointerMove(p));
            self.selection_outcome(outcome, false)?;
        }
        if let Some(g) = &mut self.gesture {
            g.end = p;
            g.shift = shift;
            if matches!(
                g.tool,
                AnnotationTool::Freehand
                    | AnnotationTool::AnnotationEraser
                    | AnnotationTool::SmartEraser
            ) && g.points.last() != Some(&p)
            {
                if g.points.len() >= MAX_STROKE_POINTS {
                    let error = AppError::InvalidInput(
                        "stroke exceeds 20000 points; cancel this gesture and use shorter strokes",
                    );
                    g.blocked = Some(error.clone());
                    self.error = Some(error.clone());
                    return Err(error);
                }
                g.points.push(p);
            }
            self.changed();
        }
        Ok(())
    }
    pub fn pointer_up(&mut self, p: PhysicalPoint, shift: bool) -> Result<(), AppError> {
        self.editable()?;
        if self.region_dragging {
            self.region_dragging = false;
            let outcome = self
                .selection
                .as_mut()
                .expect("region gesture has a selection model")
                .handle(SelectionInput::PrimaryUp(p));
            let selected = matches!(
                outcome,
                SelectionOutcome::Selected(_) | SelectionOutcome::Changed(_)
            );
            self.selection_outcome(outcome, true)?;
            // Releasing a successful screenshot selection is the confirmation.
            // Recording/scrolling keep their explicit Start/countdown boundary.
            if selected && !self.recording_selection {
                self.handle(OverlayInput::Enter)?;
                self.finish_tasks_on_release = self.release_runs_tasks;
            }
            return Ok(());
        }
        if self.gesture.is_some() {
            self.pointer_move(p, shift)?;
            let g = self.gesture.as_ref().expect("gesture remains active");
            let command = if g.tool == AnnotationTool::Region {
                Some(EditorCommand::Crop(Some(PhysicalRect::from_points(
                    g.start, g.end,
                )?)))
            } else {
                g.command(self)?
            };
            if let Some(command) = command {
                self.apply(command)?;
            }
            self.gesture = None;
            self.changed();
        }
        Ok(())
    }
    /// The egui event's logical key resolves through the persisted mapping.
    /// Chrome focus leaves Tab/Enter/Space and text-widget history to egui.
    pub fn handle_key_event(
        &mut self,
        event: &egui::Event,
        saved: &ShortcutSettings,
        focus: ShortcutFocus,
        chrome_focus: bool,
    ) -> Result<bool, AppError> {
        let egui::Event::Key {
            key,
            pressed: true,
            repeat,
            modifiers,
            ..
        } = event
        else {
            return Ok(false);
        };
        self.editable()?;
        if *repeat {
            return Ok(false);
        }
        if focus == ShortcutFocus::ShortcutEntry {
            return Ok(false);
        }
        if *key == egui::Key::Escape {
            self.handle(OverlayInput::Escape)?;
            return Ok(true);
        }
        if *key == egui::Key::Enter && self.text.is_some() && !modifiers.shift {
            self.handle(OverlayInput::Enter)?;
            return Ok(true);
        }
        if focus != ShortcutFocus::Normal || chrome_focus {
            return Ok(false);
        }
        if *key == egui::Key::Enter {
            self.handle(OverlayInput::Enter)?;
            return Ok(true);
        }
        if !self.recording_selection
            && let Some(chord) = native_chord(*key, *modifiers)
            && self.handle_chord(saved, chord, focus, false)?
        {
            return Ok(true);
        }
        if self.has_active_edit() {
            return Ok(false);
        }
        if self.tool == AnnotationTool::Select
            && *key == egui::Key::Delete
            && let Some(id) = self.selected
        {
            self.apply(EditorCommand::Erase(vec![id]))?;
            self.selected = None;
            return Ok(true);
        }
        if self.tool == AnnotationTool::Region
            && !modifiers.ctrl
            && !modifiers.alt
            && !modifiers.mac_cmd
        {
            let (dx, dy) = match key {
                egui::Key::ArrowLeft => (-1, 0),
                egui::Key::ArrowRight => (1, 0),
                egui::Key::ArrowUp => (0, -1),
                egui::Key::ArrowDown => (0, 1),
                _ => return Ok(false),
            };
            if let Some(crop) = self.document.crop() {
                let desired = if modifiers.shift {
                    PhysicalRect::new(
                        crop.left(),
                        crop.top(),
                        crop.width()
                            .checked_add_signed(dx)
                            .ok_or(AppError::InvalidInput("crop width is invalid"))?,
                        crop.height()
                            .checked_add_signed(dy)
                            .ok_or(AppError::InvalidInput("crop height is invalid"))?,
                    )?
                } else {
                    crop.translated(dx, dy)?
                };
                if let Some(selection) = &mut self.selection {
                    let outcome = if modifiers.shift {
                        selection.handle(SelectionInput::ResizeCrop(desired))
                    } else {
                        selection.handle(SelectionInput::MoveCrop { dx, dy })
                    };
                    self.selection_outcome(outcome, true)?;
                } else {
                    self.apply(EditorCommand::Crop(Some(desired)))?;
                }
                return Ok(true);
            }
        }
        Ok(false)
    }
}

pub(crate) fn native_chord(key: egui::Key, modifiers: egui::Modifiers) -> Option<KeyChord> {
    use egui::Key::*;
    // egui keys are logical, and numpad digits intentionally normalize to the
    // same chord as their main-row equivalents. No drawing key lives here.
    let vk = match key {
        k if (A..=Z).contains(&k) => 0x41 + key as u32 - A as u32,
        k if (Num0..=Num9).contains(&k) => 0x30 + key as u32 - Num0 as u32,
        k if (F1..=F24).contains(&k) => 0x70 + key as u32 - F1 as u32,
        ArrowLeft => 0x25,
        ArrowUp => 0x26,
        ArrowRight => 0x27,
        ArrowDown => 0x28,
        Escape => 0x1B,
        Tab => 0x09,
        Enter => 0x0D,
        Space => 0x20,
        Backspace => 0x08,
        Insert => 0x2D,
        Delete => 0x2E,
        Home => 0x24,
        End => 0x23,
        PageUp => 0x21,
        PageDown => 0x22,
        Semicolon | Colon => 0xBA,
        Equals | Plus => 0xBB,
        Comma => 0xBC,
        Minus => 0xBD,
        Period => 0xBE,
        Slash | Questionmark => 0xBF,
        Backtick => 0xC0,
        OpenBracket | OpenCurlyBracket => 0xDB,
        Backslash | Pipe => 0xDC,
        CloseBracket | CloseCurlyBracket => 0xDD,
        Quote => 0xDE,
        _ => return None,
    };
    Some(KeyChord {
        ctrl: modifiers.ctrl,
        alt: modifiers.alt,
        shift: modifiers.shift,
        win: modifiers.mac_cmd,
        key: crate::app::shortcuts::KeyCode::from_vk(vk).ok()?,
    })
}
