//! Main-thread native tray. Callbacks enqueue commands and wake the event loop.
use crate::app::{
    desktop::DesktopClient,
    destinations::{DestinationId, Destinations},
    error::AppError,
    events::{AppCommand, Window},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, Submenu},
};
type Choices = Arc<Mutex<HashMap<String, (DestinationId, u64)>>>;
pub struct Tray {
    _hotkeys: super::hotkeys::HotkeyBridge,
    _appearance: super::appearance::AppearanceWake,
    icon: TrayIcon,
    choices: Choices,
    checks: Vec<(DestinationId, CheckMenuItem)>,
    revision: Option<u64>,
    task_settings: Option<(
        crate::workflow::profile::TaskProfiles,
        crate::persistence::settings::ArchivePolicy,
    )>,
}
fn error() -> AppError {
    AppError::Persistence("native tray unavailable")
}
impl Tray {
    #[allow(unsafe_code)]
    pub fn new(
        client: DesktopClient,
        context: eframe::egui::Context,
        picker_gate: super::file_dialog::PickerGate,
    ) -> Result<Self, AppError> {
        let choices: Choices = Arc::new(Mutex::new(HashMap::new()));
        let map = choices.clone();
        let hotkey_client = client.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            use crate::workflow::profile::{CaptureTask, UploadTask};
            let key = event.id.as_ref();
            if key.starts_with("capture-task-") || key.starts_with("upload-task-") {
                if let Some(s) = client.snapshot().settings {
                    let mut tasks = s.tasks.clone();
                    let mut archive = s.archive.clone();
                    if let Some(task) = key
                        .strip_prefix("capture-task-")
                        .and_then(|i| i.parse::<usize>().ok())
                        .and_then(|i| CaptureTask::ALL.get(i))
                        .copied()
                    {
                        if task == CaptureTask::SaveFinal {
                            let enabled = !tasks.save_final(&archive);
                            tasks.set_save_final(&mut archive, enabled);
                        } else if task == CaptureTask::BeforeUploadReview {
                            tasks.global.before_upload_review = !tasks.global.before_upload_review;
                            tasks.global.capture.remove(&task);
                        } else if !tasks.global.capture.remove(&task) {
                            tasks.global.capture.insert(task);
                        }
                    } else if let Some(task) = key
                        .strip_prefix("upload-task-")
                        .and_then(|i| i.parse::<usize>().ok())
                        .and_then(|i| UploadTask::ALL.get(i))
                        .copied()
                        && !tasks.global.after_upload.remove(&task)
                    {
                        tasks.global.after_upload.insert(task);
                    }
                    if let Err(e) = client.submit(AppCommand::SetTaskProfiles {
                        tasks,
                        archive,
                        revision: s.revision,
                    }) {
                        client.report_error(e);
                    }
                }
                return;
            }
            if event.id.as_ref() == "upload-file" {
                super::file_dialog::spawn_pick_and_upload(client.clone(), picker_gate.clone());
                return;
            }
            let command = match event.id.as_ref() {
                "scrolling" => Some(AppCommand::ToggleScrolling),
                "record-gif" => Some(AppCommand::ToggleRecording { mp4: false }),
                "record-mp4" => Some(AppCommand::ToggleRecording { mp4: true }),
                "record-stop" => Some(AppCommand::Recording(
                    crate::recording::session::RecordingInput::Stop,
                )),
                "record-pause" => Some(AppCommand::Recording(
                    crate::recording::session::RecordingInput::PauseResume,
                )),
                "record-cancel" => Some(AppCommand::Recording(
                    crate::recording::session::RecordingInput::Cancel,
                )),
                "pin" => Some(AppCommand::PinSource(
                    crate::pin::source::PinSource::Chooser,
                )),
                "close-pins" => Some(AppCommand::CloseAllPins),
                "cancel-capture" => Some(AppCommand::CancelActiveCapture),
                "capture-region" | "capture-window" | "capture-fullscreen" | "capture-last" => {
                    use crate::capture::request::CaptureMode;
                    let mode = match event.id.as_ref() {
                        "capture-window" => CaptureMode::Window,
                        "capture-fullscreen" => CaptureMode::Fullscreen,
                        "capture-last" => CaptureMode::LastRegion,
                        _ => CaptureMode::Region,
                    };
                    Some(AppCommand::StartCapture {
                        mode,
                        selection: Default::default(),
                    })
                }
                "history" => Some(AppCommand::Show(Window::History)),
                "uploads" => Some(AppCommand::Show(Window::Uploads)),
                "settings" => Some(AppCommand::Show(Window::Settings)),
                "quit" => Some(AppCommand::Quit),
                key => map
                    .lock()
                    .ok()
                    .and_then(|m| m.get(key).copied())
                    .map(|(id, revision)| AppCommand::SetDefault { id, revision }),
            };
            if let Some(command) = command
                && let Err(error) = client.submit(command)
            {
                client.report_error(error);
            }
        }));
        // Small opaque V mark, generated in memory; no resource or image decoder.
        let mut pixels = vec![0u8; 24 * 24 * 4];
        for y in 0usize..24 {
            for x in 0usize..24 {
                let v = (5..=17).contains(&y)
                    && (x.abs_diff(5 + (y - 5) / 2) <= 1 || x.abs_diff(18 - (y - 5) / 2) <= 1);
                let color = if v {
                    [231, 193, 122, 255]
                } else {
                    [36, 39, 43, 255]
                };
                pixels[(y * 24 + x) * 4..(y * 24 + x + 1) * 4].copy_from_slice(&color);
            }
        }
        let icon = TrayIconBuilder::new()
            .with_tooltip("VectrCap")
            .with_icon(Icon::from_rgba(pixels, 24, 24).map_err(|_| error())?)
            .build()
            .map_err(|_| error())?;
        // Safety: the tray icon owns this HWND and keeps it alive at least as
        // long as `icon`, which outlives `appearance` in `Self`.
        let appearance =
            unsafe { super::appearance::AppearanceWake::attach(icon.window_handle(), context) }?;
        let hotkeys = unsafe {
            super::hotkeys::HotkeyBridge::attach(icon.window_handle(), hotkey_client.clone())
        }?;
        hotkey_client.submit(AppCommand::AttachHotkeyWindow(icon.window_handle() as isize))?;
        let mut tray = Self {
            _hotkeys: hotkeys,
            _appearance: appearance,
            icon,
            choices,
            checks: vec![],
            revision: None,
            task_settings: None,
        };
        tray.update(&Destinations::default(), 0)?;
        Ok(tray)
    }
    pub fn update_settings(
        &mut self,
        settings: &crate::persistence::settings::Settings,
    ) -> Result<(), AppError> {
        self.task_settings = Some((settings.tasks.clone(), settings.archive.clone()));
        self.update(&settings.destinations, settings.revision)
    }
    pub fn update(&mut self, destinations: &Destinations, revision: u64) -> Result<(), AppError> {
        if self.revision != Some(revision) {
            let menu = Menu::new();
            for (id, label) in [
                ("capture-region", "Capture region"),
                ("capture-window", "Capture window under pointer"),
                ("capture-fullscreen", "Capture fullscreen"),
                ("capture-last", "Capture last region"),
            ] {
                menu.append(&MenuItem::with_id(id, label, true, None))
                    .map_err(|_| error())?;
            }
            for (id, label) in [
                ("scrolling", "Scrolling capture (manual)"),
                ("record-gif", "Start / stop GIF recording"),
                ("record-mp4", "Start / stop MP4 recording"),
                ("record-stop", "Stop current recording"),
                ("record-pause", "Pause / resume current recording"),
                ("record-cancel", "Cancel current recording"),
            ] {
                menu.append(&MenuItem::with_id(id, label, true, None))
                    .map_err(|_| error())?;
            }
            menu.append(&MenuItem::with_id(
                "cancel-capture",
                "Cancel active capture / delay",
                true,
                None,
            ))
            .map_err(|_| error())?;
            menu.append(&MenuItem::with_id("upload-file", "Upload file", true, None))
                .map_err(|_| error())?;
            menu.append(&MenuItem::with_id("pin", "Pin to screen…", true, None))
                .map_err(|_| error())?;
            menu.append(&MenuItem::with_id(
                "close-pins",
                "Close all pins",
                true,
                None,
            ))
            .map_err(|_| error())?;
            menu.append(&MenuItem::with_id("uploads", "Uploads", true, None))
                .map_err(|_| error())?;
            menu.append(&MenuItem::with_id("settings", "Settings", true, None))
                .map_err(|_| error())?;
            menu.append(&MenuItem::with_id("history", "History", true, None))
                .map_err(|_| error())?;
            if let Some((tasks, archive)) = &self.task_settings {
                use crate::workflow::profile::{CaptureTask, UploadTask};
                let capture = Submenu::new("After capture tasks", true);
                for (i, t) in CaptureTask::ALL.into_iter().enumerate() {
                    let checked = match t {
                        CaptureTask::SaveFinal => tasks.save_final(archive),
                        CaptureTask::BeforeUploadReview => {
                            tasks.global.before_upload_review || tasks.global.capture.contains(&t)
                        }
                        _ => tasks.global.capture.contains(&t),
                    };
                    capture
                        .append(&CheckMenuItem::with_id(
                            format!("capture-task-{i}"),
                            t.label(),
                            true,
                            checked,
                            None,
                        ))
                        .map_err(|_| error())?;
                }
                menu.append(&capture).map_err(|_| error())?;
                let uploaded = Submenu::new("After upload tasks", true);
                for (i, t) in UploadTask::ALL.into_iter().enumerate() {
                    uploaded
                        .append(&CheckMenuItem::with_id(
                            format!("upload-task-{i}"),
                            t.label(),
                            true,
                            tasks.global.after_upload.contains(&t),
                            None,
                        ))
                        .map_err(|_| error())?;
                }
                menu.append(&uploaded).map_err(|_| error())?;
            }
            let submenu = Submenu::new("Destination", !destinations.is_empty());
            let mut choices = HashMap::new();
            let mut checks = vec![];
            for d in destinations.iter() {
                let key = format!("destination-{}", d.id.0);
                let check = CheckMenuItem::with_id(
                    key.clone(),
                    d.name.replace('&', "&&"),
                    true,
                    Some(d.id) == destinations.default_id(),
                    None,
                );
                submenu.append(&check).map_err(|_| error())?;
                choices.insert(key, (d.id, revision));
                checks.push((d.id, check));
            }
            menu.append(&submenu).map_err(|_| error())?;
            menu.append(&MenuItem::with_id("quit", "Quit", true, None))
                .map_err(|_| error())?;
            *self.choices.lock().map_err(|_| error())? = choices;
            self.checks = checks;
            self.icon.set_menu(Some(Box::new(menu)));
            self.revision = Some(revision);
        }
        for (id, item) in &self.checks {
            item.set_checked(Some(*id) == destinations.default_id());
        }
        Ok(())
    }
}
impl Drop for Tray {
    fn drop(&mut self) {
        MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
    }
}
