//! Windows clipboard writes for completion-notice "Copy link" actions.
use crate::app::error::AppError;
use windows_sys::Win32::{
    Foundation::{GlobalFree, HANDLE, HWND},
    System::{
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
    },
    UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE},
};

pub trait ClipboardWriter: Send + Sync {
    fn read_image(&self) -> Result<crate::pin::PinnedImage, AppError> {
        Err(AppError::InvalidInput(
            "clipboard does not contain a supported image",
        ))
    }
    fn write_file(&self, _path: &std::path::Path) -> Result<(), AppError> {
        Err(AppError::Persistence("file clipboard is unavailable"))
    }
    fn write_text(&self, text: &str) -> Result<(), AppError>;
    fn write_image(&self, _pixels: &image::RgbaImage) -> Result<(), AppError> {
        Err(AppError::Persistence("image clipboard is unavailable"))
    }
}

pub struct WindowsClipboard;

fn error() -> AppError {
    AppError::Persistence("clipboard action failed")
}

/// Closes the clipboard on every exit path, success or error.
struct OpenGuard;
impl Drop for OpenGuard {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}

/// A message-only owner created and destroyed on the caller's thread.
struct Owner(HWND);
impl Drop for Owner {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.0);
        }
    }
}
struct Allocation(HANDLE);
impl Drop for Allocation {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        unsafe {
            GlobalFree(self.0);
        }
    }
}
impl ClipboardWriter for WindowsClipboard {
    fn write_file(&self, path: &std::path::Path) -> Result<(), AppError> {
        set_clipboard(15, &file_drop_bytes(path)?)
    }

    #[allow(unsafe_code)]
    fn read_image(&self) -> Result<crate::pin::PinnedImage, AppError> {
        use windows_sys::Win32::System::{
            DataExchange::{GetClipboardData, RegisterClipboardFormatW},
            Memory::GlobalSize,
        };
        // Keep the clipboard open and global memory locked through validation
        // and decode. Another app cannot replace the data between the two.
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return Err(error());
            }
            let _clipboard = OpenGuard;
            let mut handle = GetClipboardData(17);
            if handle.is_null() {
                handle = GetClipboardData(8);
            }
            let mut png = false;
            if handle.is_null() {
                let format = RegisterClipboardFormatW(windows_sys::core::w!("PNG"));
                if format != 0 {
                    handle = GetClipboardData(format);
                    png = !handle.is_null();
                }
            }
            if handle.is_null() {
                return Err(AppError::InvalidInput(
                    "clipboard does not contain an image; text and URLs cannot be pinned",
                ));
            }
            let size = GlobalSize(handle);
            let valid_size = if png {
                size > 0 && size <= crate::editor::export::MAX_ENCODED_BYTES
            } else {
                (40..=crate::capture::geometry::MAX_CAPTURE_PIXELS as usize * 4 + 1024 * 1024)
                    .contains(&size)
            };
            if !valid_size {
                return Err(AppError::InvalidInput(
                    "clipboard image exceeds the decoded-image budget",
                ));
            }
            let pointer = GlobalLock(handle);
            if pointer.is_null() {
                return Err(error());
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
            if png {
                return crate::pin::source::load_pin_bytes(std::slice::from_raw_parts(
                    pointer.cast::<u8>(),
                    size,
                ));
            }
            let header = std::slice::from_raw_parts(pointer.cast::<u8>(), size.min(124));
            let layout = crate::pin::source::DibLayout::parse(header, size)?;
            layout.decode(std::slice::from_raw_parts(pointer.cast::<u8>(), size))
        }
    }
    #[allow(unsafe_code)]
    fn write_text(&self, text: &str) -> Result<(), AppError> {
        if text.contains('\0') {
            return Err(AppError::InvalidInput("clipboard text contains a NUL"));
        }
        let mut units: Vec<u16> = text.encode_utf16().collect();
        units.push(0);
        let bytes = units.len().checked_mul(size_of::<u16>()).ok_or(error())?;
        let mut data = Vec::with_capacity(bytes);
        for unit in units {
            data.extend_from_slice(&unit.to_le_bytes());
        }
        set_clipboard(13, &data)
    }
    fn write_image(&self, pixels: &image::RgbaImage) -> Result<(), AppError> {
        set_clipboard(17, &dibv5_bytes(pixels)?)
    }
}

/// CF_DIBV5 uses a top-down 32-bit bitmap with explicit RGBA masks and sRGB.
/// Only the provided finalized pixels enter this serialization.
pub fn dibv5_bytes(pixels: &image::RgbaImage) -> Result<Vec<u8>, AppError> {
    crate::capture::geometry::PhysicalRect::new(0, 0, pixels.width(), pixels.height())?
        .rgba_len()?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(124 + pixels.as_raw().len())
        .map_err(|_| error())?;
    bytes.resize(124, 0);
    for (offset, value) in [
        (0, 124u32),
        (4, pixels.width()),
        (8, (-(pixels.height() as i32)) as u32),
        (16, 3),
        (20, pixels.as_raw().len() as u32),
        (40, 0x00ff0000),
        (44, 0x0000ff00),
        (48, 0x000000ff),
        (52, 0xff000000),
        (56, 0x73524742),
        (108, 4),
    ] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
    bytes[14..16].copy_from_slice(&32u16.to_le_bytes());
    for p in pixels.pixels() {
        bytes.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
    }
    Ok(bytes)
}

#[allow(unsafe_code)]
fn set_clipboard(format: u32, data: &[u8]) -> Result<(), AppError> {
    unsafe {
        // Prepare memory before emptying the clipboard. Every failure before
        // ownership transfer frees it, including a failed GlobalLock.
        let allocation = Allocation(GlobalAlloc(GMEM_MOVEABLE, data.len()));
        if allocation.0.is_null() {
            return Err(error());
        }
        let target = GlobalLock(allocation.0);
        if target.is_null() {
            return Err(error());
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), target.cast::<u8>(), data.len());
        GlobalUnlock(allocation.0);
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let owner = Owner(CreateWindowExW(
            0,
            class.as_ptr(),
            std::ptr::null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        ));
        if owner.0.is_null() {
            return Err(error());
        }
        if OpenClipboard(owner.0) == 0 {
            return Err(error());
        }
        let _guard = OpenGuard;
        if EmptyClipboard() == 0 {
            return Err(error());
        }
        if SetClipboardData(format, allocation.0).is_null() {
            return Err(error());
        }
        // The clipboard owns immediate-rendered data from this point.
        std::mem::forget(allocation);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "touches the real OS clipboard"]
    fn writes_text_to_the_real_clipboard() {
        let expected = "VectrCap native clipboard: \u{1f4ce} \u{00e4}";
        WindowsClipboard.write_text(expected).unwrap();
        #[allow(unsafe_code)]
        unsafe {
            use windows_sys::Win32::System::{DataExchange::GetClipboardData, Memory::GlobalSize};
            let mut opened = false;
            for _ in 0..50 {
                if OpenClipboard(std::ptr::null_mut()) != 0 {
                    opened = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(opened, "clipboard readback remained busy");
            let _guard = OpenGuard;
            let handle = GetClipboardData(13);
            assert!(!handle.is_null());
            let pointer = GlobalLock(handle);
            assert!(!pointer.is_null());
            let units = std::slice::from_raw_parts(pointer.cast::<u16>(), GlobalSize(handle) / 2);
            let end = units.iter().position(|u| *u == 0).unwrap();
            let actual = String::from_utf16(&units[..end]).unwrap();
            GlobalUnlock(handle);
            assert_eq!(actual, expected);
        }
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;
    #[test]
    fn nul_text_is_rejected_before_touching_the_clipboard() {
        assert!(matches!(
            WindowsClipboard.write_text("a\0b"),
            Err(AppError::InvalidInput(_))
        ));
    }
}

/// DROPFILES with a UTF-16, double-NUL-terminated list containing one durable
/// file. This is data for the shell clipboard format, never command text.
pub fn file_drop_bytes(path: &std::path::Path) -> Result<Vec<u8>, AppError> {
    use std::os::windows::ffi::OsStrExt;
    let units: Vec<u16> = path.as_os_str().encode_wide().collect();
    if !path.is_absolute() || units.contains(&0) || units.len() > 32767 {
        return Err(AppError::InvalidInput("clipboard file path is invalid"));
    }
    let mut bytes = vec![0; 20];
    bytes[..4].copy_from_slice(&20u32.to_le_bytes());
    bytes[16..20].copy_from_slice(&1u32.to_le_bytes());
    for u in units.into_iter().chain([0, 0]) {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    Ok(bytes)
}
