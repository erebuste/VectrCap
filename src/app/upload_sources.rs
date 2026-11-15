use crate::app::{cli::LaunchRequest, error::AppError};
use std::{ffi::OsString, sync::Arc};
#[derive(Clone)]
pub enum UploadSource {
    Files(Vec<OsString>),
    ClipboardImage(Arc<image::RgbaImage>),
}
impl UploadSource {
    pub fn files(paths: Vec<OsString>) -> Result<Self, AppError> {
        LaunchRequest::upload(paths.clone())?;
        Ok(Self::Files(paths))
    }
}
#[derive(Clone, Copy)]
pub enum SourceFocus {
    UploadZone,
    ExplicitAction,
    TextField,
    Elsewhere,
}
pub enum SourceAction {
    UploadFiles(Vec<OsString>),
    ReviewImage(Arc<image::RgbaImage>),
    KeepTextPaste,
}
pub fn route_source(source: UploadSource, focus: SourceFocus) -> Result<SourceAction, AppError> {
    if matches!(focus, SourceFocus::TextField | SourceFocus::Elsewhere) {
        return Ok(SourceAction::KeepTextPaste);
    }
    match source {
        UploadSource::Files(paths) => {
            LaunchRequest::upload(paths.clone())?;
            Ok(SourceAction::UploadFiles(paths))
        }
        UploadSource::ClipboardImage(image) => {
            crate::platform::windows::clipboard_sources::validate_bitmap_budget(
                image.width(),
                image.height(),
                image.width() as usize * 4,
                image.as_raw().len(),
            )?;
            Ok(SourceAction::ReviewImage(image))
        }
    }
}
