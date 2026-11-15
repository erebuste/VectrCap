//! Explicit, one-shot clipboard reads. No listener, polling or subscription.
use crate::app::{error::AppError, upload_sources::*};
use std::{ffi::OsString, os::windows::ffi::OsStringExt, sync::Arc};
const INVALID: AppError = AppError::InvalidInput("clipboard has no supported file or image source");
pub const MAX_INPUT_BYTES: usize = 128 * 1024 * 1024;
pub trait ClipboardSourceReader {
    fn snapshot(&self) -> Result<UploadSource, AppError>;
}
pub fn read_for_action(
    reader: &dyn ClipboardSourceReader,
    focus: SourceFocus,
) -> Result<SourceAction, AppError> {
    if matches!(focus, SourceFocus::TextField | SourceFocus::Elsewhere) {
        return Ok(SourceAction::KeepTextPaste);
    }
    route_source(reader.snapshot()?, focus)
}
pub fn snapshot_formats(
    files: Option<Vec<OsString>>,
    image: Option<Arc<image::RgbaImage>>,
) -> Result<UploadSource, AppError> {
    if let Some(files) = files {
        return UploadSource::files(files);
    }
    let image = image.ok_or(INVALID)?;
    validate_bitmap_budget(
        image.width(),
        image.height(),
        image.width() as usize * 4,
        image.as_raw().len(),
    )?;
    Ok(UploadSource::ClipboardImage(image))
}
pub fn validate_bitmap_budget(
    width: u32,
    height: u32,
    stride: usize,
    input: usize,
) -> Result<(), AppError> {
    let pixels = u64::from(width) * u64::from(height);
    let min_row = (width as usize).checked_mul(4).ok_or(INVALID)?;
    if width == 0
        || height == 0
        || pixels > 64_000_000
        || input > MAX_INPUT_BYTES
        || stride < min_row
        || stride
            .checked_mul(height as usize)
            .is_none_or(|n| n > input)
    {
        return Err(INVALID);
    }
    Ok(())
}
pub fn decode_file_list(bytes: &[u8]) -> Result<Vec<OsString>, AppError> {
    if !(20..=super::ipc::MAX_PAYLOAD).contains(&bytes.len()) {
        return Err(INVALID);
    }
    let offset = u32::from_le_bytes(bytes[..4].try_into().map_err(|_| INVALID)?) as usize;
    if offset < 20
        || offset > bytes.len()
        || !offset.is_multiple_of(2)
        || bytes[16..20] != 1u32.to_le_bytes()
        || !(bytes.len() - offset).is_multiple_of(2)
    {
        return Err(INVALID);
    }
    let units: Vec<u16> = bytes[offset..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let mut paths = vec![];
    let mut start = 0;
    for (i, u) in units.iter().enumerate() {
        if *u != 0 {
            continue;
        }
        if i == start {
            if paths.is_empty() || units[i..].iter().any(|u| *u != 0) {
                return Err(INVALID);
            }
            crate::app::cli::LaunchRequest::upload(paths.clone())?;
            return Ok(paths);
        }
        if i - start > 32767 || paths.len() >= 100 {
            return Err(INVALID);
        }
        paths.push(OsString::from_wide(&units[start..i]));
        start = i + 1;
    }
    Err(INVALID)
}
pub struct WindowsClipboardSources;
impl ClipboardSourceReader for WindowsClipboardSources {
    #[allow(unsafe_code)]
    fn snapshot(&self) -> Result<UploadSource, AppError> {
        use windows_sys::Win32::{
            Foundation::HANDLE,
            System::{DataExchange::*, Memory::*},
        };
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return Err(AppError::InUse("clipboard is busy; try again"));
            }
            struct Guard;
            impl Drop for Guard {
                fn drop(&mut self) {
                    unsafe {
                        CloseClipboard();
                    }
                }
            }
            let _guard = Guard;
            let files = GetClipboardData(15);
            let (handle, is_files) = if !files.is_null() {
                (files, true)
            } else {
                let dib = GetClipboardData(17);
                (
                    if dib.is_null() {
                        GetClipboardData(8)
                    } else {
                        dib
                    },
                    false,
                )
            };
            if handle.is_null() {
                return Err(INVALID);
            }
            let size = GlobalSize(handle);
            if size == 0
                || size
                    > if is_files {
                        super::ipc::MAX_PAYLOAD
                    } else {
                        MAX_INPUT_BYTES
                    }
            {
                return Err(INVALID);
            }
            let pointer = GlobalLock(handle);
            if pointer.is_null() {
                return Err(INVALID);
            }
            struct Locked(HANDLE);
            impl Drop for Locked {
                fn drop(&mut self) {
                    unsafe {
                        GlobalUnlock(self.0);
                    }
                }
            }
            let _locked = Locked(handle);
            let bytes = std::slice::from_raw_parts(pointer.cast::<u8>(), size);
            if is_files {
                return UploadSource::files(decode_file_list(bytes)?);
            }
            if size < 40 {
                return Err(INVALID);
            }
            let width = i32::from_le_bytes(bytes[4..8].try_into().map_err(|_| INVALID)?);
            let height = i32::from_le_bytes(bytes[8..12].try_into().map_err(|_| INVALID)?);
            if width <= 0 || height == i32::MIN {
                return Err(INVALID);
            }
            let height = height.unsigned_abs();
            let bpp = u16::from_le_bytes([bytes[14], bytes[15]]);
            let stride = ((width as usize).checked_mul(bpp as usize).ok_or(INVALID)?)
                .div_ceil(32)
                .checked_mul(4)
                .ok_or(INVALID)?;
            if u64::from(width as u32) * u64::from(height) > 64_000_000
                || stride.checked_mul(height as usize).is_none_or(|n| n > size)
            {
                return Err(INVALID);
            }
            let layout = crate::pin::source::DibLayout::parse(&bytes[..size.min(124)], size)?;
            let image = layout.decode(bytes)?;
            Ok(UploadSource::ClipboardImage(image.pixels().clone()))
        }
    }
}
