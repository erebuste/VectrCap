use super::commands::EditorCommand;
use crate::{
    app::error::AppError,
    capture::{
        geometry::{MonitorGeometry, PhysicalPoint, PhysicalRect},
        request::CaptureSnapshot,
    },
};
use image::RgbaImage;
use std::{
    collections::{HashSet, VecDeque},
    sync::Arc,
};
use uuid::Uuid;

pub const MAX_ANNOTATIONS: usize = 2_000;
pub const MAX_TEXT_SCALARS: usize = 8_192;
pub const MAX_STROKE_POINTS: usize = 20_000;
pub const MAX_DOCUMENT_POINTS: usize = 200_000;
pub const MAX_UNDO_STEPS: usize = 64;
const MAX_HISTORY_POINTS: usize = 1_000_000;

pub(crate) fn validate_width(width: f32) -> Result<(), AppError> {
    if !width.is_finite() || !(0.5..=1024.0).contains(&width) {
        return Err(AppError::InvalidInput(
            "brush width must be finite and between 0.5 and 1024 pixels",
        ));
    }
    Ok(())
}
fn validate_points(points: &[PhysicalPoint]) -> Result<(), AppError> {
    let first = points
        .first()
        .ok_or(AppError::InvalidInput("stroke must not be empty"))?;
    let (mut left, mut top, mut right, mut bottom) = (first.x, first.y, first.x, first.y);
    for p in points {
        left = left.min(p.x);
        top = top.min(p.y);
        right = right.max(p.x);
        bottom = bottom.max(p.y);
    }
    let width = left
        .abs_diff(right)
        .checked_add(1)
        .ok_or(AppError::InvalidInput("stroke geometry overflows"))?;
    let height = top
        .abs_diff(bottom)
        .checked_add(1)
        .ok_or(AppError::InvalidInput("stroke geometry overflows"))?;
    PhysicalRect::new(left, top, width, height)?.rgba_len()?;
    Ok(())
}
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub(crate) stroke: [u8; 4],
    pub(crate) width: f32,
    pub(crate) fill: Option<[u8; 4]>,
}
impl Style {
    pub fn new(stroke: [u8; 4], width: f32, fill: Option<[u8; 4]>) -> Result<Self, AppError> {
        validate_width(width)?;
        Ok(Self {
            stroke,
            width,
            fill,
        })
    }
}
#[derive(Clone, Debug, PartialEq)]
pub enum Annotation {
    Ellipse {
        id: Uuid,
        bounds: PhysicalRect,
        style: Style,
    },
    Line {
        id: Uuid,
        start: PhysicalPoint,
        end: PhysicalPoint,
        color: [u8; 4],
        width: f32,
    },
    Arrow {
        id: Uuid,
        start: PhysicalPoint,
        end: PhysicalPoint,
        color: [u8; 4],
        width: f32,
    },
    Step {
        id: Uuid,
        center: PhysicalPoint,
        radius: f32,
        color: [u8; 4],
        number: u32,
    },
    Highlight {
        id: Uuid,
        bounds: PhysicalRect,
        color: [u8; 3],
        opacity: f32,
    },
    Spotlight {
        id: Uuid,
        bounds: PhysicalRect,
        dim: f32,
    },
    SmartEraseStroke {
        id: Uuid,
        points: Arc<[PhysicalPoint]>,
        color: [u8; 3],
        width: f32,
    },
    Rectangle {
        id: Uuid,
        bounds: PhysicalRect,
        style: Style,
    },
    Freehand {
        id: Uuid,
        points: Arc<[PhysicalPoint]>,
        color: [u8; 4],
        width: f32,
    },
    Text {
        id: Uuid,
        position: PhysicalPoint,
        content: Arc<str>,
        size: f32,
        color: [u8; 4],
    },
    Redaction {
        id: Uuid,
        bounds: PhysicalRect,
        color: [u8; 3],
    },
}
impl Annotation {
    pub fn id(&self) -> Uuid {
        match self {
            Self::Ellipse { id, .. }
            | Self::Line { id, .. }
            | Self::Arrow { id, .. }
            | Self::Step { id, .. }
            | Self::Highlight { id, .. }
            | Self::Spotlight { id, .. }
            | Self::SmartEraseStroke { id, .. } => *id,
            Self::Rectangle { id, .. }
            | Self::Freehand { id, .. }
            | Self::Text { id, .. }
            | Self::Redaction { id, .. } => *id,
        }
    }
    pub fn rectangle(bounds: PhysicalRect, style: Style) -> Result<Self, AppError> {
        Self::checked(Self::Rectangle {
            id: Uuid::new_v4(),
            bounds,
            style,
        })
    }
    pub fn ellipse(bounds: PhysicalRect, style: Style) -> Result<Self, AppError> {
        Self::checked(Self::Ellipse {
            id: Uuid::new_v4(),
            bounds,
            style,
        })
    }
    pub fn line(
        start: PhysicalPoint,
        end: PhysicalPoint,
        color: [u8; 4],
        width: f32,
    ) -> Result<Self, AppError> {
        Self::checked(Self::Line {
            id: Uuid::new_v4(),
            start,
            end,
            color,
            width,
        })
    }
    pub fn arrow(
        start: PhysicalPoint,
        end: PhysicalPoint,
        color: [u8; 4],
        width: f32,
    ) -> Result<Self, AppError> {
        Self::checked(Self::Arrow {
            id: Uuid::new_v4(),
            start,
            end,
            color,
            width,
        })
    }
    pub fn highlight(bounds: PhysicalRect, color: [u8; 3], opacity: f32) -> Result<Self, AppError> {
        Self::checked(Self::Highlight {
            id: Uuid::new_v4(),
            bounds,
            color,
            opacity,
        })
    }
    pub fn spotlight(bounds: PhysicalRect, dim: f32) -> Result<Self, AppError> {
        Self::checked(Self::Spotlight {
            id: Uuid::new_v4(),
            bounds,
            dim,
        })
    }
    pub fn freehand(
        points: Arc<[PhysicalPoint]>,
        color: [u8; 4],
        width: f32,
    ) -> Result<Self, AppError> {
        Self::checked(Self::Freehand {
            id: Uuid::new_v4(),
            points,
            color,
            width,
        })
    }
    pub fn text(
        position: PhysicalPoint,
        content: Arc<str>,
        size: f32,
        color: [u8; 4],
    ) -> Result<Self, AppError> {
        Self::checked(Self::Text {
            id: Uuid::new_v4(),
            position,
            content,
            size,
            color,
        })
    }
    pub fn redaction(bounds: PhysicalRect, color: [u8; 3]) -> Result<Self, AppError> {
        Self::checked(Self::Redaction {
            id: Uuid::new_v4(),
            bounds,
            color,
        })
    }
    fn checked(value: Self) -> Result<Self, AppError> {
        value.validate()?;
        Ok(value)
    }
    pub(crate) fn point_count(&self) -> usize {
        if let Self::Freehand { points, .. } | Self::SmartEraseStroke { points, .. } = self {
            points.len()
        } else {
            0
        }
    }
    pub(crate) fn validate(&self) -> Result<(), AppError> {
        if self.id().is_nil() {
            return Err(AppError::InvalidInput("annotation ID must not be nil"));
        }
        match self {
            Self::Rectangle { bounds, style, .. } | Self::Ellipse { bounds, style, .. } => {
                bounds.rgba_len()?;
                validate_width(style.width)?;
            }
            Self::Redaction { bounds, .. } => {
                bounds.rgba_len()?;
            }
            Self::Line {
                start, end, width, ..
            }
            | Self::Arrow {
                start, end, width, ..
            } => {
                validate_width(*width)?;
                if start == end {
                    return Err(AppError::InvalidInput("line endpoints must be distinct"));
                }
                validate_points(&[*start, *end])?;
            }
            Self::Step {
                center,
                radius,
                number,
                ..
            } => {
                if !radius.is_finite() || !(4.0..=512.0).contains(radius) || *number == 0 {
                    return Err(AppError::InvalidInput("step radius or number is invalid"));
                }
                let extent = radius.ceil() as i64;
                let left = i32::try_from(i64::from(center.x) - extent)
                    .map_err(|_| AppError::InvalidInput("step geometry overflows"))?;
                let top = i32::try_from(i64::from(center.y) - extent)
                    .map_err(|_| AppError::InvalidInput("step geometry overflows"))?;
                PhysicalRect::new(left, top, (extent * 2) as u32, (extent * 2) as u32)?
                    .rgba_len()?;
            }
            Self::Highlight {
                bounds, opacity, ..
            } => {
                bounds.rgba_len()?;
                super::effects::validate_opacity(*opacity)?;
            }
            Self::Spotlight { bounds, dim, .. } => {
                bounds.rgba_len()?;
                super::effects::validate_opacity(*dim)?;
            }
            Self::Freehand { points, width, .. } | Self::SmartEraseStroke { points, width, .. } => {
                validate_width(*width)?;
                if points.is_empty() || points.len() > MAX_STROKE_POINTS {
                    return Err(AppError::InvalidInput(
                        "stroke must contain 1 to 20000 points",
                    ));
                }
                validate_points(points)?;
            }
            Self::Text { content, size, .. } => {
                if !size.is_finite()
                    || !(4.0..=512.0).contains(size)
                    || content.is_empty()
                    || content.chars().count() > MAX_TEXT_SCALARS
                {
                    return Err(AppError::InvalidInput(
                        "text size or length is out of range",
                    ));
                }
                super::text::validate_text(content)?;
            }
        }
        Ok(())
    }
}
#[derive(Clone, PartialEq)]
struct State {
    annotations: Arc<Vec<Annotation>>,
    crop: Option<PhysicalRect>,
    next_step: u32,
}
/// Clones share the immutable source and all stroke/text storage. Each revision
/// is local to this document identity; undo/redo never reuse a prior revision.
pub struct EditorDocument {
    id: Uuid,
    source: Arc<RgbaImage>,
    bounds: PhysicalRect,
    monitors: Option<Arc<[MonitorGeometry]>>,
    state: State,
    undo: VecDeque<State>,
    redo: VecDeque<State>,
    revision: u64,
}
impl Clone for EditorDocument {
    /// An editable fork shares pixels and geometry, but has its own identity so
    /// equal revision counters on divergent forks cannot authorize stale output.
    fn clone(&self) -> Self {
        Self {
            id: Uuid::new_v4(),
            source: self.source.clone(),
            bounds: self.bounds,
            monitors: self.monitors.clone(),
            state: self.state.clone(),
            undo: self.undo.clone(),
            redo: self.redo.clone(),
            revision: self.revision,
        }
    }
}
impl EditorDocument {
    pub fn new(snapshot: CaptureSnapshot) -> Result<Self, AppError> {
        let bounds = snapshot.bounds();
        bounds.rgba_len()?;
        let source = RgbaImage::from_raw(bounds.width(), bounds.height(), snapshot.pixels).ok_or(
            AppError::Capture("snapshot dimensions do not match its pixels"),
        )?;
        Ok(Self::build(
            Arc::new(source),
            bounds,
            Some(snapshot.monitors.into()),
            snapshot.initial_crop,
        ))
    }
    pub fn from_image(source: Arc<RgbaImage>) -> Result<Self, AppError> {
        let bounds = PhysicalRect::new(0, 0, source.width(), source.height())?;
        bounds.rgba_len()?;
        Ok(Self::build(source, bounds, None, Some(bounds)))
    }
    fn build(
        source: Arc<RgbaImage>,
        bounds: PhysicalRect,
        monitors: Option<Arc<[MonitorGeometry]>>,
        crop: Option<PhysicalRect>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            source,
            bounds,
            monitors,
            state: State {
                annotations: Arc::new(vec![]),
                crop,
                next_step: 1,
            },
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            revision: 0,
        }
    }
    pub fn id(&self) -> Uuid {
        self.id
    }
    pub fn source(&self) -> &Arc<RgbaImage> {
        &self.source
    }
    pub fn bounds(&self) -> PhysicalRect {
        self.bounds
    }
    pub fn crop(&self) -> Option<PhysicalRect> {
        self.state.crop
    }
    pub fn annotations(&self) -> &[Annotation] {
        &self.state.annotations
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn next_step(&self) -> u32 {
        self.state.next_step
    }
    pub fn export_revision(&self) -> super::export::DocumentRevision {
        let mut document = self.clone();
        document.id = self.id;
        document.undo.clear();
        document.redo.clear();
        super::export::DocumentRevision { document }
    }
    pub fn screen_location(&self) -> Option<PhysicalPoint> {
        self.monitors
            .as_ref()
            .and(self.crop())
            .map(|r| PhysicalPoint {
                x: r.left(),
                y: r.top(),
            })
    }
    pub(crate) fn available(&self, p: PhysicalPoint) -> bool {
        self.bounds.contains(p)
            && self
                .monitors
                .as_ref()
                .is_none_or(|ms| ms.iter().any(|m| m.bounds().contains(p)))
    }
    fn validate_crop(&self, crop: PhysicalRect) -> Result<(), AppError> {
        crop.rgba_len()?;
        if !self.bounds.contains_rect(crop)
            || self
                .monitors
                .as_ref()
                .is_some_and(|ms| !ms.iter().any(|m| m.bounds().intersection(crop).is_some()))
        {
            return Err(AppError::InvalidInput(
                "crop must fit the frozen image and contain source pixels",
            ));
        }
        Ok(())
    }
    fn next_revision(&self) -> Result<u64, AppError> {
        self.revision
            .checked_add(1)
            .ok_or(AppError::InvalidInput("document revision exhausted"))
    }
    fn trim_history(history: &mut VecDeque<State>) {
        let mut points: usize = history
            .iter()
            .map(|s| {
                s.annotations
                    .iter()
                    .map(Annotation::point_count)
                    .sum::<usize>()
            })
            .sum();
        while history.len() > MAX_UNDO_STEPS || points > MAX_HISTORY_POINTS {
            if let Some(old) = history.pop_front() {
                points -= old
                    .annotations
                    .iter()
                    .map(Annotation::point_count)
                    .sum::<usize>();
            } else {
                break;
            }
        }
    }
    pub fn apply(&mut self, command: EditorCommand) -> Result<(), AppError> {
        let mut next = self.state.clone();
        match command {
            EditorCommand::Step {
                center,
                radius,
                color,
            } => {
                if next.annotations.len() >= MAX_ANNOTATIONS {
                    return Err(AppError::InvalidInput("annotation count exceeded"));
                }
                let annotation = Annotation::Step {
                    id: Uuid::new_v4(),
                    center,
                    radius,
                    color,
                    number: next.next_step,
                };
                annotation.validate()?;
                next.next_step = next
                    .next_step
                    .checked_add(1)
                    .ok_or(AppError::InvalidInput("step counter exhausted"))?;
                Arc::make_mut(&mut next.annotations).push(annotation);
            }
            EditorCommand::Crop(crop) => {
                if let Some(r) = crop {
                    self.validate_crop(r)?;
                }
                next.crop = crop;
            }
            EditorCommand::Insert(annotation) => {
                annotation.validate()?;
                if matches!(annotation, Annotation::Step { .. }) {
                    return Err(AppError::InvalidInput(
                        "insert step markers through the numbered step command",
                    ));
                }
                if next.annotations.len() >= MAX_ANNOTATIONS
                    || next.annotations.iter().any(|a| a.id() == annotation.id())
                {
                    return Err(AppError::InvalidInput(
                        "annotation count exceeded or ID duplicated",
                    ));
                }
                Arc::make_mut(&mut next.annotations).push(annotation);
            }
            EditorCommand::Modify(annotation) => {
                annotation.validate()?;
                let index = next
                    .annotations
                    .iter()
                    .position(|a| a.id() == annotation.id())
                    .ok_or(AppError::InvalidInput("annotation no longer exists"))?;
                let old = &next.annotations[index];
                if std::mem::discriminant(old) != std::mem::discriminant(&annotation)
                    || matches!((old,&annotation),(Annotation::Step{number:a,..},Annotation::Step{number:b,..}) if a!=b)
                {
                    return Err(AppError::InvalidInput(
                        "modification cannot change the annotation kind or step number",
                    ));
                }
                Arc::make_mut(&mut next.annotations)[index] = annotation;
            }
            EditorCommand::Erase(ids) => {
                if ids.len() > MAX_ANNOTATIONS {
                    return Err(AppError::InvalidInput("too many annotation IDs"));
                }
                let ids: HashSet<_> = ids.into_iter().collect();
                if ids
                    .iter()
                    .any(|id| !next.annotations.iter().any(|a| a.id() == *id))
                {
                    return Err(AppError::InvalidInput("annotation no longer exists"));
                }
                Arc::make_mut(&mut next.annotations).retain(|a| !ids.contains(&a.id()));
            }
        }
        if next
            .annotations
            .iter()
            .map(Annotation::point_count)
            .sum::<usize>()
            > MAX_DOCUMENT_POINTS
        {
            return Err(AppError::InvalidInput(
                "document exceeds 200000 stored stroke points",
            ));
        }
        if next == self.state {
            return Ok(());
        }
        let revision = self.next_revision()?;
        self.undo
            .push_back(std::mem::replace(&mut self.state, next));
        Self::trim_history(&mut self.undo);
        self.redo.clear();
        self.revision = revision;
        Ok(())
    }
    pub fn undo(&mut self) -> Result<bool, AppError> {
        if self.undo.is_empty() {
            return Ok(false);
        }
        let revision = self.next_revision()?;
        let previous = self.undo.pop_back().expect("history was checked");
        self.redo
            .push_back(std::mem::replace(&mut self.state, previous));
        Self::trim_history(&mut self.redo);
        self.revision = revision;
        Ok(true)
    }
    pub fn redo(&mut self) -> Result<bool, AppError> {
        if self.redo.is_empty() {
            return Ok(false);
        }
        let revision = self.next_revision()?;
        let next = self.redo.pop_back().expect("history was checked");
        self.undo
            .push_back(std::mem::replace(&mut self.state, next));
        Self::trim_history(&mut self.undo);
        self.revision = revision;
        Ok(true)
    }
}
