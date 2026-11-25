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
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu},
};
type Choices = Arc<Mutex<HashMap<String, (DestinationId, u64)>>>;
/// Rendered by `scripts/render-logo.py`.
const TRAY_LOGO_PNG: &[u8] = include_bytes!("../../../assets/logo/vectrcap-32.png");
pub struct Tray {
    _hotkeys: super::hotkeys::HotkeyBridge,
    _appearance: super::appearance::AppearanceWake,
    icon: TrayIcon,
    choices: Choices,
    checks: Vec<(DestinationId, CheckMenuItem)>,
    revision: Option<u64>,
    /// The update version the current menu offers.
    offered: Option<String>,
}

/// One entry of the tray menu, built into the native menu by `Tray::update`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuNode {
    Item {
        id: String,
        label: String,
        enabled: bool,
    },
    Check {
        id: String,
        label: String,
        checked: bool,
    },
    Separator,
    Submenu {
        label: String,
        enabled: bool,
        children: Vec<MenuNode>,
    },
}

fn item(id: &str, label: &str) -> MenuNode {
    MenuNode::Item {
        id: id.into(),
        label: label.into(),
        enabled: true,
    }
}
fn submenu(label: &str, children: Vec<MenuNode>) -> MenuNode {
    MenuNode::Submenu {
        label: label.into(),
        enabled: true,
        children,
    }
}

/// The tray menu: the common capture first, then grouped actions, the window,
/// the upload destination, and Quit. Task switches live in Settings ▸ Tasks.
pub fn menu_layout(
    destinations: &[(DestinationId, String)],
    default: Option<DestinationId>,
    update: Option<&str>,
) -> Vec<MenuNode> {
    let mut menu = vec![
        item("capture-region", "Capture region"),
        submenu(
            "Capture",
            vec![
                item("capture-window", "Active window"),
                item("capture-fullscreen", "Fullscreen"),
                item("capture-last", "Last region"),
                item("scrolling", "Scrolling (manual)"),
                MenuNode::Separator,
                item("cancel-capture", "Cancel capture / delay"),
            ],
        ),
        submenu(
            "Record",
            vec![
                item("record-gif", "GIF"),
                item("record-mp4", "MP4"),
                MenuNode::Separator,
                item("record-pause", "Pause / resume"),
                item("record-stop", "Stop"),
                item("record-cancel", "Cancel"),
            ],
        ),
        submenu(
            "Upload",
            vec![
                item("upload-file", "File…"),
                item("upload-clipboard", "From clipboard"),
            ],
        ),
        submenu(
            "Tools",
            vec![
                item("tool-sampler", "Screen color picker"),
                item("tool-ruler", "Ruler"),
                MenuNode::Separator,
                item("pin", "Pin to screen…"),
                item("close-pins", "Close all pins"),
            ],
        ),
        MenuNode::Separator,
        item("open", "Open VectrCap"),
        item("settings", "Settings"),
        MenuNode::Submenu {
            label: "Destination".into(),
            enabled: !destinations.is_empty(),
            children: destinations
                .iter()
                .map(|(id, name)| MenuNode::Check {
                    id: format!("destination-{}", id.0),
                    // A lone '&' would underline the next letter in Win32 menus.
                    label: name.replace('&', "&&"),
                    checked: Some(*id) == default,
                })
                .collect(),
        },
        MenuNode::Separator,
    ];
    if let Some(version) = update {
        menu.push(item("update", &format!("Update to {version} and restart")));
    }
    menu.push(item("quit", "Quit"));
    menu
}
fn error() -> AppError {
    AppError::Persistence("native tray unavailable")
}
/// Tray mouse actions, as in ShareX: left click = region capture, double click
/// = main window, middle click = upload clipboard; the menu opens on right click.
/// A left click waits out the double-click time so a double click never also captures.
#[derive(Default)]
struct TrayClicks {
    ticket: u64,
    swallow_up: bool,
}
#[allow(clippy::large_enum_variant)] // one short-lived value per click
enum TrayClick {
    Now(AppCommand),
    /// Capture after the double-click time, if `fire` still accepts this ticket.
    Deferred(u64),
}
impl TrayClicks {
    fn event(&mut self, event: &tray_icon::TrayIconEvent) -> Option<TrayClick> {
        use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};
        match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } => {
                // The release that ends a double click is not a new click.
                if std::mem::take(&mut self.swallow_up) {
                    return None;
                }
                self.ticket += 1;
                Some(TrayClick::Deferred(self.ticket))
            }
            TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => {
                self.ticket += 1;
                self.swallow_up = true;
                Some(TrayClick::Now(AppCommand::Show(Window::History)))
            }
            TrayIconEvent::Click {
                button: MouseButton::Middle,
                button_state: MouseButtonState::Up,
                ..
            } => Some(TrayClick::Now(AppCommand::UploadClipboard)),
            _ => None,
        }
    }
    fn fire(&self, ticket: u64) -> Option<AppCommand> {
        (ticket == self.ticket).then(|| AppCommand::StartCapture {
            mode: crate::capture::request::CaptureMode::Region,
            selection: Default::default(),
        })
    }
}
#[allow(unsafe_code)]
fn double_click_time() -> std::time::Duration {
    // SAFETY: no arguments; returns the user's setting in milliseconds.
    let ms = unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() };
    std::time::Duration::from_millis(u64::from(ms.clamp(100, 1000)))
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
        let click_client = client.clone();
        let clicks = Arc::new(Mutex::new(TrayClicks::default()));
        tray_icon::TrayIconEvent::set_event_handler(Some(move |event| {
            let Some(click) = clicks.lock().ok().and_then(|mut c| c.event(&event)) else {
                return;
            };
            let submit = |client: &DesktopClient, command| {
                if let Err(error) = client.submit(command) {
                    client.report_error(error);
                }
            };
            match click {
                TrayClick::Now(command) => submit(&click_client, command),
                TrayClick::Deferred(ticket) => {
                    let (clicks, client) = (clicks.clone(), click_client.clone());
                    std::thread::spawn(move || {
                        std::thread::sleep(double_click_time());
                        let command = clicks.lock().ok().and_then(|c| c.fire(ticket));
                        if let Some(command) = command {
                            submit(&client, command);
                        }
                    });
                }
            }
        }));
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if event.id.as_ref() == "upload-file" {
                super::file_dialog::spawn_pick_and_upload(client.clone(), picker_gate.clone());
                return;
            }
            let command = match event.id.as_ref() {
                "tool-sampler" => Some(AppCommand::StartUtility(
                    crate::tools::session::UtilityKind::Sampler,
                )),
                "tool-ruler" => Some(AppCommand::StartUtility(
                    crate::tools::session::UtilityKind::Ruler,
                )),
                "upload-clipboard" => Some(AppCommand::UploadClipboard),
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
                // "history" and "uploads" are ids from the previous menu.
                "open" | "history" | "uploads" => Some(AppCommand::Show(Window::History)),
                "settings" => Some(AppCommand::Show(Window::Settings)),
                "quit" => Some(AppCommand::Quit),
                "update" => {
                    crate::update::install();
                    None
                }
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
        let logo = image::load_from_memory(TRAY_LOGO_PNG)
            .map_err(|_| error())?
            .into_rgba8();
        let (width, height) = logo.dimensions();
        let icon = TrayIconBuilder::new()
            .with_tooltip("VectrCap")
            .with_menu_on_left_click(false)
            .with_icon(Icon::from_rgba(logo.into_raw(), width, height).map_err(|_| error())?)
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
        // Before the first menu exists, so Windows draws every menu dark.
        super::appearance::force_dark_menus();
        let mut tray = Self {
            _hotkeys: hotkeys,
            _appearance: appearance,
            icon,
            choices,
            checks: vec![],
            revision: None,
            offered: None,
        };
        tray.update(&Destinations::default(), 0)?;
        Ok(tray)
    }
    pub fn update_settings(
        &mut self,
        settings: &crate::persistence::settings::Settings,
    ) -> Result<(), AppError> {
        self.update(&settings.destinations, settings.revision)
    }
    pub fn update(&mut self, destinations: &Destinations, revision: u64) -> Result<(), AppError> {
        let offered = crate::update::offered_version();
        if self.revision != Some(revision) || self.offered != offered {
            let listed: Vec<_> = destinations
                .iter()
                .map(|d| (d.id, d.name.clone()))
                .collect();
            let menu = Menu::new();
            let mut choices = HashMap::new();
            let mut checks = vec![];
            for node in menu_layout(&listed, destinations.default_id(), offered.as_deref()) {
                append(&menu, node, revision, &mut choices, &mut checks)?;
            }
            *self.choices.lock().map_err(|_| error())? = choices;
            self.checks = checks;
            self.icon.set_menu(Some(Box::new(menu)));
            self.revision = Some(revision);
            self.offered = offered;
        }
        for (id, item) in &self.checks {
            item.set_checked(Some(*id) == destinations.default_id());
        }
        Ok(())
    }
}

/// Anything a native menu node can be appended to (the menu or a submenu).
trait MenuParent {
    fn add(&self, item: &dyn tray_icon::menu::IsMenuItem) -> Result<(), AppError>;
}
impl MenuParent for Menu {
    fn add(&self, item: &dyn tray_icon::menu::IsMenuItem) -> Result<(), AppError> {
        self.append(item).map_err(|_| error())
    }
}
impl MenuParent for Submenu {
    fn add(&self, item: &dyn tray_icon::menu::IsMenuItem) -> Result<(), AppError> {
        self.append(item).map_err(|_| error())
    }
}

/// Builds one `MenuNode` natively. Destination checks are recorded so a click
/// maps back to its destination at this settings revision.
fn append(
    parent: &dyn MenuParent,
    node: MenuNode,
    revision: u64,
    choices: &mut HashMap<String, (DestinationId, u64)>,
    checks: &mut Vec<(DestinationId, CheckMenuItem)>,
) -> Result<(), AppError> {
    match node {
        MenuNode::Item { id, label, enabled } => {
            parent.add(&MenuItem::with_id(id, label, enabled, None))
        }
        MenuNode::Check { id, label, checked } => {
            let check = CheckMenuItem::with_id(id.clone(), label, true, checked, None);
            parent.add(&check)?;
            if let Some(destination) = id
                .strip_prefix("destination-")
                .and_then(|uuid| uuid.parse().ok())
                .map(DestinationId)
            {
                choices.insert(id, (destination, revision));
                checks.push((destination, check));
            }
            Ok(())
        }
        MenuNode::Separator => parent.add(&PredefinedMenuItem::separator()),
        MenuNode::Submenu {
            label,
            enabled,
            children,
        } => {
            let submenu = Submenu::new(label, enabled);
            for child in children {
                append(&submenu, child, revision, choices, checks)?;
            }
            parent.add(&submenu)
        }
    }
}
impl Drop for Tray {
    fn drop(&mut self) {
        MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
        tray_icon::TrayIconEvent::set_event_handler(None::<fn(tray_icon::TrayIconEvent)>);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tray_logo_decodes() {
        let logo = image::load_from_memory(TRAY_LOGO_PNG).unwrap();
        assert_eq!((logo.width(), logo.height()), (32, 32));
    }

    #[test]
    fn a_left_click_on_the_tray_icon_starts_a_region_capture() {
        use tray_icon::{
            MouseButton, MouseButtonState, Rect, TrayIconEvent, TrayIconId, dpi::PhysicalPosition,
        };
        let click = |button, button_state| TrayIconEvent::Click {
            id: TrayIconId::new("tray"),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect::default(),
            button,
            button_state,
        };
        let mut c = TrayClicks::default();
        let Some(TrayClick::Deferred(ticket)) =
            c.event(&click(MouseButton::Left, MouseButtonState::Up))
        else {
            panic!("a left click defers a capture");
        };
        assert!(matches!(
            c.fire(ticket),
            Some(AppCommand::StartCapture {
                mode: crate::capture::request::CaptureMode::Region,
                ..
            })
        ));
        // One capture per click, and the right button keeps the menu.
        assert!(
            c.event(&click(MouseButton::Left, MouseButtonState::Down))
                .is_none()
        );
        assert!(
            c.event(&click(MouseButton::Right, MouseButtonState::Up))
                .is_none()
        );
        assert!(matches!(
            c.event(&click(MouseButton::Middle, MouseButtonState::Up)),
            Some(TrayClick::Now(AppCommand::UploadClipboard))
        ));
    }

    #[test]
    fn a_double_click_opens_the_window_without_capturing() {
        use tray_icon::{
            MouseButton, MouseButtonState, Rect, TrayIconEvent, TrayIconId, dpi::PhysicalPosition,
        };
        let id = TrayIconId::new("tray");
        let at = PhysicalPosition::new(0.0, 0.0);
        let up = TrayIconEvent::Click {
            id: id.clone(),
            position: at,
            rect: Rect::default(),
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
        };
        let mut c = TrayClicks::default();
        let Some(TrayClick::Deferred(first)) = c.event(&up) else {
            panic!("first release defers");
        };
        assert!(matches!(
            c.event(&TrayIconEvent::DoubleClick {
                id,
                position: at,
                rect: Rect::default(),
                button: MouseButton::Left,
            }),
            Some(TrayClick::Now(AppCommand::Show(Window::History)))
        ));
        assert!(
            c.event(&up).is_none(),
            "the double click's release is swallowed"
        );
        assert!(c.fire(first).is_none(), "the pending capture is cancelled");
    }

    fn label(node: &MenuNode) -> String {
        match node {
            MenuNode::Item { label, .. }
            | MenuNode::Check { label, .. }
            | MenuNode::Submenu { label, .. } => label.clone(),
            MenuNode::Separator => "—".into(),
        }
    }
    fn top_level_labels(menu: &[MenuNode]) -> Vec<String> {
        menu.iter().map(label).collect()
    }
    fn submenu<'a>(menu: &'a [MenuNode], name: &str) -> (&'a [MenuNode], bool) {
        menu.iter()
            .find_map(|node| match node {
                MenuNode::Submenu {
                    label,
                    enabled,
                    children,
                } if label == name => Some((children.as_slice(), *enabled)),
                _ => None,
            })
            .expect("submenu exists")
    }
    fn ids(menu: &[MenuNode], out: &mut Vec<String>) {
        for node in menu {
            match node {
                MenuNode::Item { id, .. } | MenuNode::Check { id, .. } => out.push(id.clone()),
                MenuNode::Submenu { children, .. } => ids(children, out),
                MenuNode::Separator => {}
            }
        }
    }

    #[test]
    fn tray_menu_matches_the_design() {
        let menu = menu_layout(&[], None, None);
        assert_eq!(
            top_level_labels(&menu),
            [
                "Capture region",
                "Capture",
                "Record",
                "Upload",
                "Tools",
                "—",
                "Open VectrCap",
                "Settings",
                "Destination",
                "—",
                "Quit"
            ]
        );
        let names = |name| top_level_labels(submenu(&menu, name).0);
        assert_eq!(
            names("Capture"),
            [
                "Active window",
                "Fullscreen",
                "Last region",
                "Scrolling (manual)",
                "—",
                "Cancel capture / delay"
            ]
        );
        assert_eq!(
            names("Record"),
            ["GIF", "MP4", "—", "Pause / resume", "Stop", "Cancel"]
        );
        assert_eq!(names("Upload"), ["File…", "From clipboard"]);
        assert_eq!(
            names("Tools"),
            [
                "Screen color picker",
                "Ruler",
                "—",
                "Pin to screen…",
                "Close all pins"
            ]
        );
        assert!(
            !submenu(&menu, "Destination").1,
            "empty Destination is disabled"
        );
        let mut all = vec![];
        ids(&menu, &mut all);
        assert!(all.iter().all(|id| !id.contains("-task-")));
    }

    #[test]
    fn an_offered_update_sits_just_above_quit() {
        let menu = menu_layout(&[], None, Some("0.2.0"));
        let labels = top_level_labels(&menu);
        assert_eq!(
            labels[labels.len() - 3..],
            ["—", "Update to 0.2.0 and restart", "Quit"]
        );
    }

    #[test]
    fn destinations_are_checks_with_the_default_checked() {
        let work = DestinationId(uuid::Uuid::new_v4());
        let home = DestinationId(uuid::Uuid::new_v4());
        let menu = menu_layout(
            &[(work, "Vectr & work".into()), (home, "Personal".into())],
            Some(work),
            None,
        );
        let (children, enabled) = submenu(&menu, "Destination");
        assert!(enabled);
        assert!(matches!(
            &children[0],
            MenuNode::Check { label, checked: true, id } if label == "Vectr && work" && *id == format!("destination-{}", work.0)
        ));
        assert!(matches!(
            &children[1],
            MenuNode::Check { checked: false, .. }
        ));
    }

    #[test]
    fn every_previous_tray_id_is_still_present() {
        let mut all = vec![];
        ids(&menu_layout(&[], None, None), &mut all);
        for id in [
            "tool-sampler",
            "tool-ruler",
            "upload-clipboard",
            "upload-file",
            "scrolling",
            "record-gif",
            "record-mp4",
            "record-stop",
            "record-pause",
            "record-cancel",
            "pin",
            "close-pins",
            "cancel-capture",
            "capture-region",
            "capture-window",
            "capture-fullscreen",
            "capture-last",
            "open",
            "settings",
            "quit",
        ] {
            assert!(all.iter().any(|listed| listed == id), "{id} missing");
        }
    }
}
