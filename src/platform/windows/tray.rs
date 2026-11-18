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
pub struct Tray {
    _hotkeys: super::hotkeys::HotkeyBridge,
    _appearance: super::appearance::AppearanceWake,
    icon: TrayIcon,
    choices: Choices,
    checks: Vec<(DestinationId, CheckMenuItem)>,
    revision: Option<u64>,
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
) -> Vec<MenuNode> {
    vec![
        item("capture-region", "Capture region"),
        submenu(
            "Capture",
            vec![
                item("capture-window", "Window under pointer"),
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
        item("quit", "Quit"),
    ]
}
fn error() -> AppError {
    AppError::Persistence("native tray unavailable")
}
/// A left click on the icon starts a region capture (as in ShareX); the menu
/// opens on right click. Acts on release, so one click is one capture.
fn tray_click_command(event: &tray_icon::TrayIconEvent) -> Option<AppCommand> {
    use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};
    matches!(
        event,
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        }
    )
    .then(|| AppCommand::StartCapture {
        mode: crate::capture::request::CaptureMode::Region,
        selection: Default::default(),
    })
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
        tray_icon::TrayIconEvent::set_event_handler(Some(move |event| {
            if let Some(command) = tray_click_command(&event)
                && let Err(error) = click_client.submit(command)
            {
                click_client.report_error(error);
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
            .with_menu_on_left_click(false)
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
        // Before the first menu exists, so Windows draws every menu dark.
        super::appearance::force_dark_menus();
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
    pub fn update_settings(
        &mut self,
        settings: &crate::persistence::settings::Settings,
    ) -> Result<(), AppError> {
        self.update(&settings.destinations, settings.revision)
    }
    pub fn update(&mut self, destinations: &Destinations, revision: u64) -> Result<(), AppError> {
        if self.revision != Some(revision) {
            let listed: Vec<_> = destinations
                .iter()
                .map(|d| (d.id, d.name.clone()))
                .collect();
            let menu = Menu::new();
            let mut choices = HashMap::new();
            let mut checks = vec![];
            for node in menu_layout(&listed, destinations.default_id()) {
                append(&menu, node, revision, &mut choices, &mut checks)?;
            }
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
        assert!(matches!(
            tray_click_command(&click(MouseButton::Left, MouseButtonState::Up)),
            Some(AppCommand::StartCapture {
                mode: crate::capture::request::CaptureMode::Region,
                ..
            })
        ));
        // One capture per click, and the right button keeps the menu.
        assert!(tray_click_command(&click(MouseButton::Left, MouseButtonState::Down)).is_none());
        assert!(tray_click_command(&click(MouseButton::Right, MouseButtonState::Up)).is_none());
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
        let menu = menu_layout(&[], None);
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
                "Window under pointer",
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
    fn destinations_are_checks_with_the_default_checked() {
        let work = DestinationId(uuid::Uuid::new_v4());
        let home = DestinationId(uuid::Uuid::new_v4());
        let menu = menu_layout(
            &[(work, "Vectr & work".into()), (home, "Personal".into())],
            Some(work),
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
        ids(&menu_layout(&[], None), &mut all);
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
