//! Windows "open"/"reveal" shell actions for completion-notice job actions.
use crate::app::error::AppError;
use std::{os::windows::ffi::OsStrExt, path::Path};
use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};

pub trait ShellActions: Send + Sync {
    fn open_path(&self, path: &Path) -> Result<(), AppError>;
    fn open_url(&self, url: &str) -> Result<(), AppError>;
    fn reveal_path(&self, path: &Path) -> Result<(), AppError>;
}

pub struct WindowsShellActions;

fn error() -> AppError {
    AppError::Persistence("shell action failed")
}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// `ShellExecuteW` returns a value `<= 32` on failure (an `HINSTANCE`-shaped
/// error code on success is always `> 32`), per its documented contract.
#[allow(unsafe_code)]
fn execute(file: &str, parameters: Option<&str>) -> Result<(), AppError> {
    let operation = wide("open");
    let file = wide(file);
    let parameters = parameters.map(wide);
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            parameters.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if (result as isize) <= 32 {
        return Err(error());
    }
    Ok(())
}

impl ShellActions for WindowsShellActions {
    fn open_path(&self, path: &Path) -> Result<(), AppError> {
        execute(&path.display().to_string(), None)
    }
    fn open_url(&self, url: &str) -> Result<(), AppError> {
        execute(url, None)
    }
    fn reveal_path(&self, path: &Path) -> Result<(), AppError> {
        let path = path.display().to_string();
        if path.contains('"') {
            return Err(AppError::InvalidInput(
                "path contains a quote and cannot be revealed",
            ));
        }
        execute("explorer.exe", Some(&format!("/select,\"{path}\"")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "launches a real process"]
    fn opens_a_real_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smoke.txt");
        std::fs::write(&path, b"x").unwrap();
        WindowsShellActions.open_path(&path).unwrap();
    }
}
