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
    #[allow(unsafe_code)]
    fn write_text(&self, text: &str) -> Result<(), AppError> {
        if text.contains('\0') {
            return Err(AppError::InvalidInput("clipboard text contains a NUL"));
        }
        let mut units: Vec<u16> = text.encode_utf16().collect();
        units.push(0);
        let bytes = units.len().checked_mul(size_of::<u16>()).ok_or(error())?;
        unsafe {
            // Prepare memory before emptying the clipboard. Every failure before
            // ownership transfer frees it, including a failed GlobalLock.
            let allocation = Allocation(GlobalAlloc(GMEM_MOVEABLE, bytes));
            if allocation.0.is_null() {
                return Err(error());
            }
            let target = GlobalLock(allocation.0);
            if target.is_null() {
                return Err(error());
            }
            std::ptr::copy_nonoverlapping(units.as_ptr(), target.cast::<u16>(), units.len());
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
            const CF_UNICODETEXT: u32 = 13;
            if SetClipboardData(CF_UNICODETEXT, allocation.0).is_null() {
                return Err(error());
            }
            // The clipboard owns immediate-rendered data from this point.
            std::mem::forget(allocation);
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
