//! Per-logon lifetime guard and focus event; path handoff uses the secured pipe.
#![allow(unsafe_code)]
use crate::app::error::AppError;
use std::{ptr, sync::Arc, thread::JoinHandle};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, LocalFree},
    Security::{
        Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, SECURITY_ATTRIBUTES,
    },
    System::Threading::{CreateEventW, CreateMutexW, INFINITE, SetEvent, WaitForMultipleObjects},
};
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn error() -> AppError {
    AppError::Persistence("Windows instance guard unavailable")
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
pub struct InstanceGuard {
    _mutex: Handle,
    _focus: Handle,
    stop: Handle,
    waiter: Option<JoinHandle<()>>,
}
impl InstanceGuard {
    pub fn acquire(
        namespace: &str,
        focus: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Option<Self>, AppError> {
        if namespace.is_empty()
            || namespace.len() > 80
            || !namespace
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(AppError::InvalidInput("invalid instance namespace"));
        }
        let policy = super::ipc::current_policy()?;
        let base = policy.instance_name(namespace)?;
        // The same explicit logon-SID DACL and namespace protect the guard and
        // pipe. Create the event before the mutex so racing activation can focus.
        unsafe {
            let mut descriptor = ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide(&policy.sddl()).as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut(),
            ) == 0
            {
                return Err(error());
            }
            struct Descriptor(*mut std::ffi::c_void);
            impl Drop for Descriptor {
                fn drop(&mut self) {
                    unsafe {
                        LocalFree(self.0);
                    }
                }
            }
            let descriptor = Descriptor(descriptor);
            let security = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: 0,
            };
            let event = Handle(CreateEventW(
                &security,
                0,
                0,
                wide(&format!("{base}-focus")).as_ptr(),
            ));
            if event.0.is_null() {
                return Err(error());
            }
            let mutex = CreateMutexW(&security, 0, wide(&format!("{base}-guard")).as_ptr());
            let existed = GetLastError() == ERROR_ALREADY_EXISTS;
            let mutex = Handle(mutex);
            if mutex.0.is_null() {
                return Err(error());
            }
            if existed {
                if SetEvent(event.0) == 0 {
                    return Err(error());
                }
                return Ok(None);
            }
            let stop = Handle(CreateEventW(ptr::null(), 1, 0, ptr::null()));
            if stop.0.is_null() {
                return Err(error());
            }
            // Handles remain owned by guard until its waiter has joined.
            let handles = [stop.0 as usize, event.0 as usize];
            let waiter = std::thread::Builder::new()
                .name("instance-focus".into())
                .spawn(move || {
                    let handles = handles.map(|h| h as HANDLE);
                    while let 1 = WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) {
                        focus();
                    }
                })
                .map_err(|_| error())?;
            Ok(Some(Self {
                _mutex: mutex,
                _focus: event,
                stop,
                waiter: Some(waiter),
            }))
        }
    }
}
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe {
            SetEvent(self.stop.0);
        }
        if let Some(waiter) = self.waiter.take() {
            let _ = waiter.join();
        }
    }
}
