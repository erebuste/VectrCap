//! Explicit current-user sign-in registration. Never reads a command from settings.
#![allow(unsafe_code)]
use crate::app::error::AppError;
use std::{os::windows::ffi::OsStrExt, path::Path};
use windows_sys::Win32::System::Registry::*;

pub trait StartupBackend {
    fn set(&self, enabled: bool) -> Result<(), AppError>;
}
pub struct NativeStartup;
fn command(path: &Path) -> Result<Vec<u16>, AppError> {
    let raw: Vec<u16> = path.as_os_str().encode_wide().collect();
    if !path.is_absolute() || raw.iter().any(|c| *c == 0 || *c == 34) {
        return Err(AppError::InvalidInput("invalid startup executable path"));
    }
    Ok(std::iter::once(34).chain(raw).chain([34, 0]).collect())
}
impl StartupBackend for NativeStartup {
    fn set(&self, enabled: bool) -> Result<(), AppError> {
        let path = std::env::current_exe().map_err(|e| AppError::io("locating executable", &e))?;
        let value = command(&path)?;
        let key: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Run\0"
            .encode_utf16()
            .collect();
        let name: Vec<u16> = "VectrCap\0".encode_utf16().collect();
        let mut handle = std::ptr::null_mut();
        // Fixed HKCU key and value; no shell invocation or machine-wide access.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut handle,
                std::ptr::null_mut(),
            )
        };
        if status != 0 {
            return Err(AppError::Persistence(
                "cannot open per-user startup registration",
            ));
        }
        let status = unsafe {
            let status = if enabled {
                RegSetValueExW(
                    handle,
                    name.as_ptr(),
                    0,
                    REG_SZ,
                    value.as_ptr().cast(),
                    (value.len() * 2) as u32,
                )
            } else {
                RegDeleteValueW(handle, name.as_ptr())
            };
            RegCloseKey(handle);
            status
        };
        if status != 0 && !(status == 2 && !enabled) {
            return Err(AppError::Persistence(
                "cannot update per-user startup registration",
            ));
        }
        Ok(())
    }
}
pub fn apply(
    backend: &dyn StartupBackend,
    before: bool,
    after: bool,
    save: impl FnOnce() -> Result<(), AppError>,
) -> Result<(), AppError> {
    if before == after {
        return save();
    }
    backend.set(after)?;
    if let Err(error) = save() {
        backend.set(before).map_err(|_| {
            AppError::Persistence(
                "startup settings failed and startup registration could not be restored",
            )
        })?;
        return Err(error);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quotes_executable_as_one_argument() {
        assert_eq!(
            String::from_utf16(&command(Path::new(r"C:\Program Files\VectrCap.exe")).unwrap())
                .unwrap(),
            "\"C:\\Program Files\\VectrCap.exe\"\0"
        );
    }
    #[test]
    fn save_failure_restores_registration() {
        struct Fake(std::cell::RefCell<Vec<bool>>);
        impl StartupBackend for Fake {
            fn set(&self, enabled: bool) -> Result<(), AppError> {
                self.0.borrow_mut().push(enabled);
                Ok(())
            }
        }
        let fake = Fake(Default::default());
        assert!(apply(&fake, false, true, || Err(AppError::Persistence("fixture"))).is_err());
        assert_eq!(*fake.0.borrow(), [true, false]);
    }
}
