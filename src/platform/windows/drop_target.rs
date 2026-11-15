//! Out-of-process COM activation only. Selection data is a bounded CF_HDROP,
//! never expanded into a command line or passed to an interpreter.
#![allow(unsafe_code)]
use crate::app::{cli::LaunchRequest, error::AppError};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::{CLASS_E_NOAGGREGATION, E_FAIL, E_POINTER, POINTL},
        System::{Com::*, Ole::*, SystemServices::MODIFIERKEYS_FLAGS},
        UI::WindowsAndMessaging::*,
    },
    core::{BOOL, GUID, IUnknown, Interface, Ref, implement},
};
type Accept = Arc<dyn Fn(LaunchRequest) -> Result<uuid::Uuid, AppError> + Send + Sync>;
fn error() -> AppError {
    AppError::Persistence("Explorer selection server unavailable")
}
#[implement(IDropTarget)]
struct Target {
    accept: Accept,
    completed: Arc<AtomicBool>,
}
impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(
        &self,
        data: Ref<IDataObject>,
        _: MODIFIERKEYS_FLAGS,
        _: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        unsafe {
            if effect.is_null() {
                return Err(E_POINTER.into());
            }
            let supported = data
                .as_ref()
                .is_some_and(|d| d.QueryGetData(&file_format()).is_ok());
            *effect = if supported {
                DROPEFFECT_COPY
            } else {
                DROPEFFECT_NONE
            };
            Ok(())
        }
    }
    fn DragOver(
        &self,
        _: MODIFIERKEYS_FLAGS,
        _: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        unsafe {
            if effect.is_null() {
                return Err(E_POINTER.into());
            }
            *effect = DROPEFFECT_COPY;
            Ok(())
        }
    }
    fn DragLeave(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn Drop(
        &self,
        data: Ref<IDataObject>,
        _: MODIFIERKEYS_FLAGS,
        _: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        unsafe {
            if effect.is_null() {
                return Err(E_POINTER.into());
            }
            *effect = DROPEFFECT_NONE;
            let request = selection(data.as_ref().ok_or(E_POINTER)?)
                .map_err(|_| windows::core::Error::from(E_FAIL))?;
            (self.accept)(request).map_err(|_| windows::core::Error::from(E_FAIL))?;
            *effect = DROPEFFECT_COPY;
            self.completed.store(true, Ordering::Release);
            Ok(())
        }
    }
}
fn file_format() -> FORMATETC {
    FORMATETC {
        cfFormat: 15,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}
unsafe fn selection(data: &IDataObject) -> Result<LaunchRequest, AppError> {
    unsafe {
        let medium = data.GetData(&file_format()).map_err(|_| error())?;
        struct Medium(STGMEDIUM);
        impl std::ops::Drop for Medium {
            fn drop(&mut self) {
                unsafe {
                    ReleaseStgMedium(&mut self.0);
                }
            }
        }
        let medium = Medium(medium);
        if medium.0.tymed != TYMED_HGLOBAL.0 as u32 {
            return Err(error());
        }
        let handle = medium.0.u.hGlobal;
        use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
        let size = GlobalSize(handle.0);
        if !(20..=super::ipc::MAX_PAYLOAD).contains(&size) {
            return Err(error());
        }
        let pointer = GlobalLock(handle.0);
        if pointer.is_null() {
            return Err(error());
        }
        let result = super::clipboard_sources::decode_file_list(std::slice::from_raw_parts(
            pointer.cast::<u8>(),
            size,
        ));
        GlobalUnlock(handle.0);
        LaunchRequest::upload(result?)
    }
}
#[implement(IClassFactory)]
struct Factory {
    accept: Accept,
    completed: Arc<AtomicBool>,
}
impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        iid: *const GUID,
        out: *mut *mut std::ffi::c_void,
    ) -> windows::core::Result<()> {
        unsafe {
            if out.is_null() || iid.is_null() {
                return Err(E_POINTER.into());
            }
            *out = std::ptr::null_mut();
            if outer.is_some() {
                return Err(CLASS_E_NOAGGREGATION.into());
            }
            let target: IDropTarget = Target {
                accept: self.accept.clone(),
                completed: self.completed.clone(),
            }
            .into();
            target.query(iid, out).ok()
        }
    }
    fn LockServer(&self, _: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}
pub struct DropServer {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    completed: Arc<AtomicBool>,
}
impl DropServer {
    pub fn start(accept: Accept) -> Result<Self, AppError> {
        let stop = Arc::new(AtomicBool::new(false));
        let shutdown = stop.clone();
        let completed = Arc::new(AtomicBool::new(false));
        let done = completed.clone();
        let (ready, rx) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("explorer-drop-server".into())
            .spawn(move || unsafe {
                if CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_err() {
                    let _ = ready.send(Err(error()));
                    return;
                }
                let factory: IClassFactory = Factory {
                    accept,
                    completed: done,
                }
                .into();
                let clsid = GUID::from_u128(0xe17546c7_ed8a_47b0_ab53_113b779d7211);
                let cookie = match CoRegisterClassObject(
                    &clsid,
                    &factory,
                    CLSCTX_LOCAL_SERVER,
                    REGCLS_MULTIPLEUSE,
                ) {
                    Ok(cookie) => cookie,
                    Err(_) => {
                        let _ = ready.send(Err(error()));
                        CoUninitialize();
                        return;
                    }
                };
                let _ = ready.send(Ok(()));
                while !shutdown.load(Ordering::Acquire) {
                    let mut message = MSG::default();
                    while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                    MsgWaitForMultipleObjects(None, false, 50, QS_ALLINPUT);
                }
                let _ = CoRevokeClassObject(cookie);
                drop(factory);
                CoUninitialize();
            })
            .map_err(|_| error())?;
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self {
                stop,
                thread: Some(thread),
                completed,
            }),
            _ => {
                stop.store(true, Ordering::Release);
                let _ = thread.join();
                Err(error())
            }
        }
    }
    /// Racing COM activation can run without a second GUI/tray while the primary
    /// registers its class. It exits after delivery or a bounded activation wait.
    pub fn serve_once(accept: Accept) -> Result<(), AppError> {
        let server = Self::start(accept)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !server.completed.load(Ordering::Acquire) {
            if std::time::Instant::now() >= deadline {
                return Err(error());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }
}
impl Drop for DropServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
