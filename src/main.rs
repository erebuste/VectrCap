#![windows_subsystem = "windows"]
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use vectrcap::{
    app::{desktop::DesktopRuntime, error::AppError},
    platform::{
        CredentialClientFactory,
        windows::{credentials::WindowsCredentialStore, instance::InstanceGuard},
    },
    ui::shell::Shell,
};

fn main() {
    if run().is_err() {
        startup_error();
    }
}
fn run() -> Result<(), AppError> {
    // File/config/Explorer ingress belongs to Task 11. Never interpret arguments
    // as paths, tokens, commands, destinations, or upload requests.
    if std::env::args_os().len() != 1 {
        return Err(AppError::InvalidInput(
            "command-line ingress is not supported",
        ));
    }
    let context = Arc::new(OnceLock::<eframe::egui::Context>::new());
    let focused = Arc::new(AtomicBool::new(false));
    let wake_context = context.clone();
    let wake_focus = focused.clone();
    let Some(_guard) = InstanceGuard::acquire(
        "VectrCap-v1",
        Arc::new(move || {
            wake_focus.store(true, Ordering::Release);
            if let Some(ctx) = wake_context.get() {
                ctx.request_repaint();
            }
        }),
    )?
    else {
        return Ok(());
    };
    // dirs 7's Windows adapter uses SHGetKnownFolderPath(FOLDERID_LocalAppData).
    let root = dirs::data_local_dir()
        .ok_or(AppError::Persistence("Windows LocalAppData unavailable"))?
        .join("VectrCap");
    let mut runtime = None;
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([610.0, 650.0])
            .with_min_inner_size([360.0, 320.0])
            .with_visible(false),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    let result = eframe::run_native(
        "VectrCap",
        options,
        Box::new(|cc| {
            let _ = context.set(cc.egui_ctx.clone());
            let ctx = cc.egui_ctx.clone();
            let store = Arc::new(WindowsCredentialStore::current_user());
            let factory = Arc::new(CredentialClientFactory::new(store.clone()));
            let worker = DesktopRuntime::spawn(
                root,
                store,
                factory,
                Arc::new(move || ctx.request_repaint()),
            )?;
            let shell = Shell::new(worker.client.clone(), focused, cc.egui_ctx.clone())?;
            runtime = Some(worker);
            Ok(Box::new(shell))
        }),
    );
    // Normally already settled before Close. Also drain after GUI initialization
    // failure/OS teardown, without blocking a live UI on disk or network work.
    drop(runtime);
    result.map_err(|_| AppError::Persistence("native desktop initialization failed"))
}
#[allow(unsafe_code)]
fn startup_error() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let title: Vec<u16> = "VectrCap\0".encode_utf16().collect();
    let message: Vec<u16> = "VectrCap could not start safely. Another settings owner, an unavailable Windows service, or unsupported arguments may be responsible. No upload was requested.\0".encode_utf16().collect();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}
