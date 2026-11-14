//! Windows "open"/"reveal" shell actions for completion-notice job actions.
use crate::app::error::AppError;
use std::{
    ffi::OsStr,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};
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

fn wide_os(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}
fn wide(s: &str) -> Vec<u16> {
    wide_os(OsStr::new(s))
}

/// Never resolved by bare name: a bare "explorer.exe" passed to
/// `ShellExecuteW` is subject to a search order that can include a planted
/// binary ahead of the real one.
fn windows_explorer_path() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    Path::new(&root).join("explorer.exe")
}

/// Builds Explorer's `/select,"<path>"` parameter directly from the path's
/// own UTF-16 units, never through string formatting of a displayed/lossy
/// conversion, so an unusual filename cannot be reinterpreted into a
/// different argument boundary. Rejects a non-absolute path (ambient-cwd
/// ambiguity) and any embedded quote (would break the quoted argument).
fn select_parameter(path: &Path) -> Result<Vec<u16>, AppError> {
    if !path.is_absolute() {
        return Err(AppError::InvalidInput("path must be absolute to reveal"));
    }
    let units: Vec<u16> = path.as_os_str().encode_wide().collect();
    if units.contains(&(b'"' as u16)) {
        return Err(AppError::InvalidInput(
            "path contains a quote and cannot be revealed",
        ));
    }
    let mut parameter = wide("/select,\"");
    parameter.pop(); // drop the NUL terminator so the path can be appended
    parameter.extend(units);
    parameter.push(b'"' as u16);
    parameter.push(0);
    Ok(parameter)
}

/// `ShellExecuteW` returns a value `<= 32` on failure (an `HINSTANCE`-shaped
/// error code on success is always `> 32`), per its documented contract.
#[allow(unsafe_code)]
fn execute(file: &[u16], parameters: Option<&[u16]>) -> Result<(), AppError> {
    let operation = wide("open");
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            parameters.map_or(std::ptr::null(), |p| p.as_ptr()),
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
        execute(&wide_os(path.as_os_str()), None)
    }
    fn open_url(&self, url: &str) -> Result<(), AppError> {
        execute(&wide(url), None)
    }
    fn reveal_path(&self, path: &Path) -> Result<(), AppError> {
        let parameter = select_parameter(path)?;
        execute(
            &wide_os(windows_explorer_path().as_os_str()),
            Some(&parameter),
        )
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
    #[test]
    fn explorer_path_is_absolute_and_not_a_bare_name() {
        // A bare "explorer.exe" passed to ShellExecuteW is resolved via a
        // search order that can include a planted binary; this must always
        // resolve to an absolute, Windows-directory-rooted path instead.
        let path = windows_explorer_path();
        assert!(path.is_absolute());
        assert_eq!(path.file_name().unwrap(), "explorer.exe");
    }
    #[test]
    fn select_parameter_rejects_a_relative_path() {
        assert!(select_parameter(Path::new("relative\\file.txt")).is_err());
    }
    #[test]
    fn select_parameter_rejects_an_embedded_quote() {
        assert!(select_parameter(Path::new("C:\\a\"b.txt")).is_err());
    }
    #[test]
    fn select_parameter_wraps_the_absolute_path_in_quotes() {
        let parameter = select_parameter(Path::new("C:\\dir\\file.txt")).unwrap();
        let text = String::from_utf16(&parameter[..parameter.len() - 1]).unwrap();
        assert_eq!(text, "/select,\"C:\\dir\\file.txt\"");
        assert_eq!(parameter.last(), Some(&0));
    }
}
