use super::document::Annotation;
use crate::capture::geometry::PhysicalRect;
use uuid::Uuid;
#[derive(Clone, Debug)]
pub enum EditorCommand {
    Insert(Annotation),
    Modify(Annotation),
    Erase(Vec<Uuid>),
    Crop(Option<PhysicalRect>),
}
