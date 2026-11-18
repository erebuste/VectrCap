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
    use vectrcap::app::cli::{LaunchMode, parse_launch_args};
    use vectrcap::platform::windows::{drop_target::DropServer, ipc};
    let launch = parse_launch_args(std::env::args_os().skip(1))?;
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
        return match launch {
            LaunchMode::Ui => Ok(()),
            LaunchMode::Request(request) => ipc::send_request(&request).map(|_| ()),
            LaunchMode::ShellServer => DropServer::serve_once(Arc::new(|request| {
                ipc::send_request(&request)?;
                Ok(request.request_id)
            })),
        };
    };
    let root = vectrcap::persistence::AppPaths::current_user()?.root;
    std::fs::create_dir_all(&root).map_err(|e| AppError::Io {
        stage: "opening application data",
        kind: e.kind(),
    })?;
    let mut runtime = None;
    let mut pipe_server = None;
    let mut drop_server = None;
    // Taskbar/title-bar icon; the exe icon comes from build.rs.
    let logo = image::load_from_memory(include_bytes!("../assets/logo/vectrcap-64.png"))
        .map(|logo| logo.into_rgba8())
        .ok();
    let mut viewport = eframe::egui::ViewportBuilder::default();
    if let Some(logo) = logo {
        viewport = viewport.with_icon(eframe::egui::IconData {
            width: logo.width(),
            height: logo.height(),
            rgba: logo.into_raw(),
        });
    }
    let options = eframe::NativeOptions {
        viewport: viewport
            .with_inner_size([610.0, 650.0])
            .with_min_inner_size([360.0, 320.0])
            .with_visible(false)
            .with_active(false),
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
            let pipe_client = worker.client.clone();
            pipe_server = Some(ipc::PipeServer::start(Arc::new(move |request| {
                pipe_client.accept_handoff(request)
            }))?);
            let drop_client = worker.client.clone();
            drop_server = Some(DropServer::start(Arc::new(move |request| {
                drop_client.accept_handoff(request)
            }))?);
            if let LaunchMode::Request(request) = launch {
                worker
                    .client
                    .submit(vectrcap::app::events::AppCommand::Launch {
                        request,
                        reply: None,
                    })?;
            }
            let shell = Shell::new(worker.client.clone(), focused, cc.egui_ctx.clone())?;
            runtime = Some(worker);
            Ok(Box::new(shell))
        }),
    );
    // Normally already settled before Close. Also drain after GUI initialization
    // failure/OS teardown, without blocking a live UI on disk or network work.
    drop(drop_server);
    drop(pipe_server);
    drop(runtime);
    result.map_err(|_| AppError::Persistence("native desktop initialization failed"))
}
#[allow(unsafe_code)]
fn startup_error() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let title: Vec<u16> = "VectrCap\0".encode_utf16().collect();
    let message: Vec<u16> = "VectrCap could not start safely. Another settings owner, an unavailable Windows service, or unsupported arguments may be responsible. Accepted requests remain recoverable on the next start.\0".encode_utf16().collect();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}
