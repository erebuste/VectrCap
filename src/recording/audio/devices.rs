use crate::{
    app::error::AppError,
    recording::options::{AudioMode, AudioOptions, EndpointSelection},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    System,
    Microphone,
}
/// This is the only factory-dispatch boundary. None/Off cannot invoke a device factory.
pub fn open_selected<T>(
    options: Option<&AudioOptions>,
    mut open: impl FnMut(SourceKind, &EndpointSelection) -> Result<T, AppError>,
) -> Result<Vec<T>, AppError> {
    let Some(o) = options else {
        return Ok(Vec::new());
    };
    o.validate()?;
    let mut out = Vec::with_capacity(2);
    if matches!(o.mode, AudioMode::System | AudioMode::Both) {
        out.push(open(SourceKind::System, &o.system)?);
    }
    if matches!(o.mode, AudioMode::Microphone | AudioMode::Both) {
        out.push(open(SourceKind::Microphone, &o.microphone)?);
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioEndpoint {
    pub id: String,
    pub name: String,
    pub kind: crate::recording::audio::devices::SourceKind,
}
