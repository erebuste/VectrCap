//! Explicit current-user sign-in registration. Never reads a command from settings.
#![allow(unsafe_code)]
use crate::app::error::AppError;
use std::{os::windows::ffi::OsStrExt, path::Path};
use windows_sys::Win32::System::Registry::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupValue {
    kind: u32,
    bytes: Vec<u8>,
}
pub trait StartupBackend {
    fn read(&self) -> Result<Option<StartupValue>, AppError>;
    fn set(&self, enabled: bool) -> Result<(), AppError>;
    fn restore(&self, value: Option<&StartupValue>) -> Result<(), AppError>;
}
pub struct NativeStartup;
struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}
fn open() -> Result<Key, AppError> {
    let key: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Run\0"
        .encode_utf16()
        .collect();
    let mut handle = std::ptr::null_mut();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            0,
            std::ptr::null(),
            0,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
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
    Ok(Key(handle))
}
fn name() -> Vec<u16> {
    "VectrCap\0".encode_utf16().collect()
}
fn command(path: &Path) -> Result<Vec<u16>, AppError> {
    let raw: Vec<u16> = path.as_os_str().encode_wide().collect();
    if !path.is_absolute() || raw.iter().any(|c| *c == 0 || *c == 34) {
        return Err(AppError::InvalidInput("invalid startup executable path"));
    }
    Ok(std::iter::once(34).chain(raw).chain([34, 0]).collect())
}
impl StartupBackend for NativeStartup {
    fn read(&self) -> Result<Option<StartupValue>, AppError> {
        let key = open()?;
        // A Run command is small; never allocate according to an untrusted registry size.
        let mut bytes = vec![0u8; 65536];
        let mut size = bytes.len() as u32;
        let mut kind = 0;
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                name().as_ptr(),
                std::ptr::null(),
                &mut kind,
                bytes.as_mut_ptr(),
                &mut size,
            )
        };
        if status == 2 {
            return Ok(None);
        }
        if status != 0 {
            return Err(AppError::Persistence(
                "cannot read existing startup registration",
            ));
        }
        bytes.truncate(size as usize);
        Ok(Some(StartupValue { kind, bytes }))
    }
    fn set(&self, enabled: bool) -> Result<(), AppError> {
        let path = std::env::current_exe().map_err(|e| AppError::io("locating executable", &e))?;
        let value = StartupValue {
            kind: REG_SZ,
            bytes: command(&path)?
                .iter()
                .flat_map(|unit| unit.to_le_bytes())
                .collect(),
        };
        if self.read()?.as_ref().is_some_and(|old| old != &value) {
            return Err(AppError::InUse(
                "existing startup registration belongs to another executable; remove it before changing startup",
            ));
        }
        self.restore(enabled.then_some(&value))
    }
    fn restore(&self, value: Option<&StartupValue>) -> Result<(), AppError> {
        let key = open()?;
        let status = unsafe {
            match value {
                Some(value) => RegSetValueExW(
                    key.0,
                    name().as_ptr(),
                    0,
                    value.kind,
                    value.bytes.as_ptr(),
                    value.bytes.len() as u32,
                ),
                None => RegDeleteValueW(key.0, name().as_ptr()),
            }
        };
        if status != 0 && !(status == 2 && value.is_none()) {
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
    let previous = backend.read()?;
    backend.set(after)?;
    if let Err(error) = save() {
        backend.restore(previous.as_ref()).map_err(|_| {
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
        struct Fake(std::cell::RefCell<Option<StartupValue>>);
        impl StartupBackend for Fake {
            fn read(&self) -> Result<Option<StartupValue>, AppError> {
                Ok(self.0.borrow().clone())
            }
            fn set(&self, enabled: bool) -> Result<(), AppError> {
                *self.0.borrow_mut() = enabled.then_some(StartupValue {
                    kind: REG_SZ,
                    bytes: vec![1, 2],
                });
                Ok(())
            }
            fn restore(&self, value: Option<&StartupValue>) -> Result<(), AppError> {
                *self.0.borrow_mut() = value.cloned();
                Ok(())
            }
        }
        // Persisted boolean deliberately disagrees with actual registry bytes/type.
        for previous in [
            None,
            Some(StartupValue {
                kind: REG_EXPAND_SZ,
                bytes: vec![42, 0],
            }),
        ] {
            let fake = Fake(std::cell::RefCell::new(previous.clone()));
            assert!(apply(&fake, false, true, || Err(AppError::Persistence("fixture"))).is_err());
            assert_eq!(*fake.0.borrow(), previous);
        }
    }
}
