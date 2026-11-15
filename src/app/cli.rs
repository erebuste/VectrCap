//! A closed, path-only launch boundary. Never accepts origin, token or command text.
use crate::app::error::AppError;
use std::{
    ffi::{OsStr, OsString},
    path::Path,
};
use uuid::Uuid;
#[derive(Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    pub request_id: Uuid,
    pub kind: LaunchRequestKind,
}
#[derive(Clone, PartialEq, Eq)]
pub enum LaunchRequestKind {
    UploadFiles { paths: Vec<OsString> },
    ImportDestination { path: OsString },
}
pub enum LaunchMode {
    Ui,
    Request(LaunchRequest),
    ShellServer,
}
impl LaunchRequest {
    pub fn upload(paths: Vec<OsString>) -> Result<Self, AppError> {
        let r = Self {
            request_id: Uuid::new_v4(),
            kind: LaunchRequestKind::UploadFiles { paths },
        };
        r.validate()?;
        Ok(r)
    }
    pub fn import(path: OsString) -> Result<Self, AppError> {
        let r = Self {
            request_id: Uuid::new_v4(),
            kind: LaunchRequestKind::ImportDestination { path },
        };
        r.validate()?;
        Ok(r)
    }
    pub fn validate(&self) -> Result<(), AppError> {
        if self.request_id.is_nil() {
            return Err(AppError::InvalidInput("missing request identifier"));
        }
        match &self.kind {
            LaunchRequestKind::UploadFiles { paths } => {
                if !(1..=100).contains(&paths.len()) {
                    return Err(AppError::InvalidInput("select 1 to 100 files"));
                }
                for p in paths {
                    upload_path(p)?;
                }
            }
            LaunchRequestKind::ImportDestination { path } => {
                upload_path(path)?;
                if !Path::new(path)
                    .extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("vectrcap"))
                {
                    return Err(AppError::InvalidInput("select a VectrCap destination file"));
                }
            }
        }
        Ok(())
    }
}
pub fn upload_path(path: &OsStr) -> Result<(), AppError> {
    use std::os::windows::ffi::OsStrExt;
    let u: Vec<u16> = path.encode_wide().collect();
    if u.len() < 4
        || u.len() > 32767
        || !((u[0] as u8).is_ascii_alphabetic()
            && u[0] < 128
            && u[1] == 58
            && (u[2] == 92 || u[2] == 47))
        || u[3..].iter().any(|c| {
            *c < 32
                || *c == 58
                || *c == 34
                || *c == 42
                || *c == 63
                || *c == 60
                || *c == 62
                || *c == 124
        })
    {
        return Err(AppError::InvalidInput(
            "only absolute local filesystem paths are accepted",
        ));
    }
    for part in u[3..].split(|c| *c == 92 || *c == 47) {
        if part.is_empty() || part.last().is_some_and(|c| *c == 32 || *c == 46) {
            return Err(AppError::InvalidInput("unsafe filesystem path"));
        }
        let base: Vec<u16> = part
            .iter()
            .take_while(|c| **c != 46)
            .map(|c| if (97..=122).contains(c) { c - 32 } else { *c })
            .collect();
        let name = String::from_utf16_lossy(&base);
        if matches!(
            name.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || ((name.starts_with("COM") || name.starts_with("LPT"))
            && name.len() == 4
            && name.as_bytes()[3].is_ascii_digit())
        {
            return Err(AppError::InvalidInput(
                "device names are not filesystem sources",
            ));
        }
    }
    Ok(())
}
/// Arguments exclude argv[0]. A literal `--` is mandatory before all paths.
pub fn parse_launch_args(args: impl IntoIterator<Item = OsString>) -> Result<LaunchMode, AppError> {
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Ok(LaunchMode::Ui);
    };
    if first == "--shell-server" {
        // LocalServer32 activation appends this fixed COM marker. It is not
        // accepted by UI, upload or destination-import modes.
        let marker = args.next();
        if marker.is_none()
            || marker.as_deref() == Some(OsStr::new("-Embedding")) && args.next().is_none()
        {
            return Ok(LaunchMode::ShellServer);
        }
        return Err(AppError::InvalidInput("invalid COM activation arguments"));
    }
    let import = first == "--import-destination";
    if !(import || first == "--upload") || args.next().as_deref() != Some(OsStr::new("--")) {
        return Err(AppError::InvalidInput("invalid launch arguments"));
    }
    let paths: Vec<_> = args.take(101).collect();
    let request = if import {
        if paths.len() != 1 {
            return Err(AppError::InvalidInput("import requires exactly one path"));
        }
        LaunchRequest::import(paths.into_iter().next().unwrap())?
    } else {
        LaunchRequest::upload(paths)?
    };
    Ok(LaunchMode::Request(request))
}
