use super::document::Annotation;
use crate::capture::geometry::{PhysicalPoint, PhysicalRect};
use uuid::Uuid;
#[derive(Clone, Debug)]
pub enum EditorCommand {
    Insert(Annotation),
    Modify(Annotation),
    Erase(Vec<Uuid>),
    Crop(Option<PhysicalRect>),
    Step {
        center: PhysicalPoint,
        radius: f32,
        color: [u8; 4],
    },
}
