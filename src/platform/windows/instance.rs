//! Handle lifetime guard plus event-only focus request. No upload/config ingress.
#![allow(unsafe_code)]
use crate::app::error::AppError;
use std::{ptr, sync::Arc, thread::JoinHandle};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, LocalFree},
    Security::{
        Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    },
    System::Threading::{
        CreateEventW, CreateMutexW, GetCurrentProcess, INFINITE, OpenProcessToken, SetEvent,
        WaitForMultipleObjects,
    },
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
fn current_sid() -> Result<String, AppError> {
    // GetTokenInformation requires aligned storage; Vec<usize> supplies it.
    unsafe {
        let mut token = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(error());
        }
        let token = Handle(token);
        let mut bytes = 0;
        GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut bytes);
        if bytes == 0 || bytes > 65536 {
            return Err(error());
        }
        let mut buffer = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
        if GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        ) == 0
        {
            return Err(error());
        }
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut sid = ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 {
            return Err(error());
        }
        let mut length = 0;
        while *sid.add(length) != 0 {
            length += 1;
        }
        let result =
            String::from_utf16(std::slice::from_raw_parts(sid, length)).map_err(|_| error());
        LocalFree(sid.cast());
        result
    }
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
        let base = format!("Local\\{}-{}", namespace, current_sid()?);
        // Default token DACL, current interactive session and SID-qualified name.
        // Create the event first so a racing secondary always has a signal target.
        unsafe {
            let event = Handle(CreateEventW(
                ptr::null(),
                0,
                0,
                wide(&format!("{base}-focus")).as_ptr(),
            ));
            if event.0.is_null() {
                return Err(error());
            }
            let mutex = CreateMutexW(ptr::null(), 0, wide(&format!("{base}-guard")).as_ptr());
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
