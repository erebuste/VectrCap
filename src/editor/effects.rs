//! Spotlight, highlight and opaque sampled-color painting.
use super::{Annotation, EditorDocument, sample_source_color};
use crate::{app::error::AppError, capture::geometry::PhysicalPoint};
use std::sync::Arc;
pub struct SmartEraser {
    color: [u8; 3],
}
impl SmartEraser {
    pub fn sample(document: &EditorDocument, point: PhysicalPoint) -> Result<Self, AppError> {
        let [r, g, b, _] = sample_source_color(document, point)?;
        Ok(Self { color: [r, g, b] })
    }
    pub fn stroke(&self, points: Arc<[PhysicalPoint]>, width: f32) -> Result<Annotation, AppError> {
        let stroke = Annotation::SmartEraseStroke {
            id: uuid::Uuid::new_v4(),
            points,
            color: self.color,
            width,
        };
        stroke.validate()?;
        Ok(stroke)
    }
}
pub(crate) fn validate_opacity(opacity: f32) -> Result<(), AppError> {
    if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
        return Err(AppError::InvalidInput(
            "effect opacity must be finite and between zero and one",
        ));
    }
    Ok(())
}
pub(crate) fn spotlight(
    canvas: &mut tiny_skia::Pixmap,
    crop: crate::capture::geometry::PhysicalRect,
    annotations: &[Annotation],
) {
    let focuses: Vec<_> = annotations
        .iter()
        .filter_map(|a| {
            if let Annotation::Spotlight { bounds, dim, .. } = a {
                Some((*bounds, *dim))
            } else {
                None
            }
        })
        .collect();
    let dim = focuses.iter().map(|(_, dim)| *dim).fold(0.0, f32::max);
    if dim == 0.0 {
        return;
    }
    let alpha = (dim * 255.0).round() as u32;
    // Rebuild the horizontal union only at focus top/bottom edges. Very tall
    // images must not scan every focus or allocate spans for every source row.
    let mut events = vec![];
    for (id, (bounds, _)) in focuses.iter().enumerate() {
        if let Some(part) = bounds.intersection(crop) {
            let x = (i64::from(part.left()) - i64::from(crop.left())) as u32;
            let y = (i64::from(part.top()) - i64::from(crop.top())) as u32;
            events.push((y, true, id, (x, x + part.width())));
            events.push((y + part.height(), false, id, (x, x + part.width())));
        }
    }
    events.sort_unstable();
    let mut active = std::collections::BTreeMap::new();
    let mut spans: Vec<(u32, u32)> = vec![];
    let mut event_index = 0;
    for y in 0..canvas.height() {
        if events.get(event_index).is_some_and(|e| e.0 == y) {
            while let Some(&(event_y, enter, id, span)) = events.get(event_index) {
                if event_y != y {
                    break;
                }
                if enter {
                    active.insert(id, span);
                } else {
                    active.remove(&id);
                }
                event_index += 1;
            }
            let mut ordered: Vec<_> = active.values().copied().collect();
            ordered.sort_unstable();
            spans.clear();
            for span in ordered {
                if let Some(last) = spans.last_mut()
                    && span.0 <= last.1
                {
                    last.1 = last.1.max(span.1);
                } else {
                    spans.push(span);
                }
            }
        }
        let mut index = 0;
        for x in 0..canvas.width() {
            while index < spans.len() && spans[index].1 <= x {
                index += 1;
            }
            if index < spans.len() && spans[index].0 <= x {
                continue;
            }
            let i = ((u64::from(y) * u64::from(canvas.width()) + u64::from(x)) * 4) as usize;
            let p = &mut canvas.data_mut()[i..i + 4];
            for c in &mut p[..3] {
                *c = ((u32::from(*c) * (255 - alpha) + 127) / 255) as u8;
            }
            p[3] = (alpha + (u32::from(p[3]) * (255 - alpha) + 127) / 255) as u8;
        }
    }
}
