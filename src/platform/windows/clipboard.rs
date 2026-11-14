//! Windows clipboard writes for completion-notice "Copy link" actions.
use crate::app::error::AppError;
use windows_sys::Win32::{
    Foundation::{GlobalFree, HANDLE},
    System::{
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
    },
};

pub trait ClipboardWriter: Send + Sync {
    fn write_text(&self, text: &str) -> Result<(), AppError>;
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

impl ClipboardWriter for WindowsClipboard {
    #[allow(unsafe_code)]
    fn write_text(&self, text: &str) -> Result<(), AppError> {
        // UTF-16, NUL-terminated buffer, as CF_UNICODETEXT requires.
        let mut units: Vec<u16> = text.encode_utf16().collect();
        units.push(0);
        let bytes = units.len() * std::mem::size_of::<u16>();
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return Err(error());
            }
            let _guard = OpenGuard;
            if EmptyClipboard() == 0 {
                return Err(error());
            }
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if handle.is_null() {
                return Err(error());
            }
            let target = GlobalLock(handle);
            if target.is_null() {
                return Err(error());
            }
            std::ptr::copy_nonoverlapping(units.as_ptr(), target.cast::<u16>(), units.len());
            GlobalUnlock(handle);
            const CF_UNICODETEXT: u32 = 13;
            // Ownership transfer: on success the clipboard now owns `handle`
            // and will free it; only an explicit failure here leaves us
            // still responsible for it, and GlobalAlloc's handle is also a
            // HGLOBAL understood by SetClipboardData's HANDLE parameter.
            if SetClipboardData(CF_UNICODETEXT, handle as HANDLE).is_null() {
                GlobalFree(handle);
                return Err(error());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "touches the real OS clipboard"]
    fn writes_text_to_the_real_clipboard() {
        WindowsClipboard.write_text("vectrcap-smoke-test").unwrap();
    }
}
