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
            if event.id.as_ref() == "upload-file" {
                super::file_dialog::spawn_pick_and_upload(client.clone(), picker_gate.clone());
                return;
            }
            let command = match event.id.as_ref() {
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
        };
        tray.update(&Destinations::default(), 0)?;
        Ok(tray)
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
            menu.append(&MenuItem::with_id(
                "cancel-capture",
                "Cancel active capture / delay",
                true,
                None,
            ))
            .map_err(|_| error())?;
            menu.append(&MenuItem::with_id("upload-file", "Upload file", true, None))
                .map_err(|_| error())?;
            menu.append(&MenuItem::with_id("uploads", "Uploads", true, None))
                .map_err(|_| error())?;
            menu.append(&MenuItem::with_id("settings", "Settings", true, None))
                .map_err(|_| error())?;
            menu.append(&MenuItem::with_id("history", "History", true, None))
                .map_err(|_| error())?;
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
