//! Configurable global hotkeys and editor-tool shortcuts.
//!
//! This module is pure policy: normalized key chords, persisted bindings for
//! every approved action/scope, and the transactional apply/dispatch rules.
//! Native `RegisterHotKey`/message-loop wiring lives in
//! `platform::windows::hotkeys` and only ever talks to this module through
//! `ShortcutRegistrar`, so the policy here is fully testable with a fake.
//!
//! Only a few approved global actions have a real handler today (Show
//! History, Show settings, Upload file). Every other approved action from
//! the spec's hotkey section is modeled and persisted — so a saved
//! assignment survives restarts and is ready the moment its owning
//! capture/recording/editor feature ships — but [`ShortcutAction::is_available_now`]
//! is false for them, and this module never asks the registrar to claim
//! their key: an unimplemented action must never silently take a combination
//! away from the OS or another app while doing nothing.
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::app::destinations::DestinationSelection;
use crate::app::error::AppError;

/// A validated virtual-key code from a fixed allowlist: letters, digits,
/// function keys, navigation keys, and a handful of named/OEM keys. Never an
/// arbitrary string or command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct KeyCode(u32);

impl KeyCode {
    pub fn from_vk(vk: u32) -> Result<Self, AppError> {
        if is_supported_vk(vk) {
            Ok(KeyCode(vk))
        } else {
            Err(AppError::InvalidInput("unsupported shortcut key"))
        }
    }

    pub fn vk(&self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for KeyCode {
    type Error = AppError;
    fn try_from(vk: u32) -> Result<Self, AppError> {
        KeyCode::from_vk(vk)
    }
}

impl From<KeyCode> for u32 {
    fn from(k: KeyCode) -> u32 {
        k.0
    }
}

fn is_supported_vk(vk: u32) -> bool {
    matches!(vk,
        0x30..=0x39   // '0'..'9'
        | 0x41..=0x5A // 'A'..'Z'
        | 0x70..=0x87 // F1..F24
        | 0x20        // space
        | 0x08 | 0x09 | 0x0D | 0x1B // backspace, tab, enter, escape
        | 0x2D..=0x2E // insert, delete
        | 0x21..=0x28 // page up/down, end, home, left, up, right, down
        | 0xBA..=0xC0 // OEM ; = , - . / `
        | 0xDB..=0xDE // OEM [ \ ] '
        | 0x60..=0x6F // numpad 0..9 * + separator - . /
    )
}

/// Normalized modifier flags plus a supported key. Two chords with the same
/// flags/key are equal regardless of how a user pressed left/right modifier
/// keys or in what order — there is nothing further to normalize once stored
/// this way, which is what duplicate detection in [`ShortcutSettings::validate`]
/// relies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyChord {
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub win: bool,
    pub key: KeyCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ShortcutScope {
    Global,
    Editor,
    Ruler,
    ScreenSampler,
}

/// Every action the approved spec names for the hotkeys/shortcuts section.
/// `is_available_now` is the only thing that gates native registration or
/// dispatch; scope/identity never change for an action once shipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ShortcutAction {
    // Global actions with a real handler today.
    ShowHistory,
    ShowSettings,
    UploadFile,
    // Approved global actions whose owning capture/recording/pin feature is
    // a later task. Modeled and persisted now; never registered or dispatched.
    RegionScreenshot,
    WindowScreenshot,
    FullscreenScreenshot,
    CaptureLastRegion,
    ToggleScrollingCapture,
    ToggleGifRecording,
    ToggleMp4Recording,
    StopRecording,
    PauseResumeRecording,
    UploadFromClipboard,
    PinSourceChooser,
    PinFromScreen,
    PinFromClipboard,
    PinFromFile,
    CloseAllPins,
    ScreenColorPicker,
    Ruler,
    // Editor tool shortcuts (Task 6's overlay does not exist yet).
    ToolRectangle,
    ToolEllipse,
    ToolLine,
    ToolArrow,
    ToolFreehand,
    ToolText,
    ToolStep,
    ToolHighlight,
    ToolSpotlight,
    ToolAnnotationEraser,
    ToolSmartEraser,
}

impl ShortcutAction {
    pub fn all() -> &'static [ShortcutAction] {
        use ShortcutAction::*;
        &[
            ShowHistory,
            ShowSettings,
            UploadFile,
            RegionScreenshot,
            WindowScreenshot,
            FullscreenScreenshot,
            CaptureLastRegion,
            ToggleScrollingCapture,
            ToggleGifRecording,
            ToggleMp4Recording,
            StopRecording,
            PauseResumeRecording,
            UploadFromClipboard,
            PinSourceChooser,
            PinFromScreen,
            PinFromClipboard,
            PinFromFile,
            CloseAllPins,
            ScreenColorPicker,
            Ruler,
            ToolRectangle,
            ToolEllipse,
            ToolLine,
            ToolArrow,
            ToolFreehand,
            ToolText,
            ToolStep,
            ToolHighlight,
            ToolSpotlight,
            ToolAnnotationEraser,
            ToolSmartEraser,
        ]
    }

    pub fn scope(&self) -> ShortcutScope {
        use ShortcutAction::*;
        match self {
            ToolRectangle | ToolEllipse | ToolLine | ToolArrow | ToolFreehand | ToolText
            | ToolStep | ToolHighlight | ToolSpotlight | ToolAnnotationEraser | ToolSmartEraser => {
                ShortcutScope::Editor
            }
            _ => ShortcutScope::Global,
        }
    }

    /// Whether a real handler exists today. See the module docs: an
    /// unavailable action is never registered or dispatched, only persisted.
    pub fn is_available_now(&self) -> bool {
        matches!(
            self,
            ShortcutAction::ShowHistory | ShortcutAction::ShowSettings | ShortcutAction::UploadFile
        )
    }

    pub fn label(&self) -> &'static str {
        use ShortcutAction::*;
        match self {
            ShowHistory => "Show history",
            ShowSettings => "Show settings",
            UploadFile => "Upload file",
            RegionScreenshot => "Region screenshot",
            WindowScreenshot => "Window screenshot",
            FullscreenScreenshot => "Fullscreen screenshot",
            CaptureLastRegion => "Capture last region",
            ToggleScrollingCapture => "Start/stop scrolling capture",
            ToggleGifRecording => "Start/stop GIF recording",
            ToggleMp4Recording => "Start/stop MP4 recording",
            StopRecording => "Stop current recording",
            PauseResumeRecording => "Pause/resume recording",
            UploadFromClipboard => "Upload from clipboard",
            PinSourceChooser => "Pin source chooser",
            PinFromScreen => "Pin from screen",
            PinFromClipboard => "Pin from clipboard",
            PinFromFile => "Pin from file",
            CloseAllPins => "Close all pins",
            ScreenColorPicker => "Screen color picker",
            Ruler => "Ruler",
            ToolRectangle => "Tool: Rectangle",
            ToolEllipse => "Tool: Ellipse",
            ToolLine => "Tool: Line",
            ToolArrow => "Tool: Arrow",
            ToolFreehand => "Tool: Freehand",
            ToolText => "Tool: Text",
            ToolStep => "Tool: Step",
            ToolHighlight => "Tool: Highlight",
            ToolSpotlight => "Tool: Spotlight",
            ToolAnnotationEraser => "Tool: Annotation eraser",
            ToolSmartEraser => "Tool: Smart eraser",
        }
    }
}

/// One action's persisted assignment. `destination` only matters for actions
/// that upload; it is still validated/tracked for every binding through the
/// same configured-reference mechanism as other settings data, so a future
/// capture action can never be silently retargeted by an unrelated
/// destination edit (see `DestinationReferences` in `destination_manager`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShortcutBinding {
    pub action: ShortcutAction,
    pub scope: ShortcutScope,
    pub chord: Option<KeyChord>,
    pub enabled: bool,
    #[serde(default)]
    pub destination: DestinationSelection,
}

impl ShortcutBinding {
    pub fn unassigned(action: ShortcutAction) -> Self {
        ShortcutBinding {
            action,
            scope: action.scope(),
            chord: None,
            enabled: false,
            destination: DestinationSelection::Inherit,
        }
    }
}

/// Validated, versioned settings for every known action. Missing actions
/// (older settings file, or a newly added action) default to unassigned via
/// [`ShortcutSettings::effective`], never to a silently-active binding.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShortcutSettings {
    #[serde(default)]
    pub bindings: Vec<ShortcutBinding>,
}

impl ShortcutSettings {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.bindings.len() > ShortcutAction::all().len() {
            return Err(AppError::InvalidInput("too many shortcut bindings"));
        }
        let mut seen_actions = HashSet::new();
        let mut seen_chords: HashMap<ShortcutScope, HashSet<KeyChord>> = HashMap::new();
        for binding in &self.bindings {
            if !seen_actions.insert(binding.action) {
                return Err(AppError::InvalidInput("duplicate shortcut action"));
            }
            if binding.scope != binding.action.scope() {
                return Err(AppError::InvalidInput(
                    "shortcut scope does not match its action",
                ));
            }
            if let Some(chord) = binding.chord
                && binding.enabled
                && !seen_chords.entry(binding.scope).or_default().insert(chord)
            {
                return Err(AppError::InvalidInput(
                    "duplicate active shortcut within one scope",
                ));
            }
        }
        Ok(())
    }

    pub fn get(&self, action: ShortcutAction) -> Option<&ShortcutBinding> {
        self.bindings.iter().find(|b| b.action == action)
    }

    /// One binding per known action, defaulting any action absent from the
    /// persisted list to unassigned/disabled.
    pub fn effective(&self) -> Vec<ShortcutBinding> {
        ShortcutAction::all()
            .iter()
            .map(|&action| {
                self.get(action)
                    .cloned()
                    .unwrap_or_else(|| ShortcutBinding::unassigned(action))
            })
            .collect()
    }
}

/// A native registration slot, assigned fresh and never reused for the
/// lifetime of one `ShortcutManager` — the same never-reuse discipline the
/// credential journal uses for generations, and for the same reason: a stale
/// id must never be mistaken for a live one during a transactional swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RegistrationId(pub i32);

/// Injected native backend. Implementations must be independent per call:
/// one rejected chord must never prevent registering the others.
pub trait ShortcutRegistrar: Send + Sync {
    fn register(&self, id: RegistrationId, chord: KeyChord) -> Result<(), AppError>;
    fn unregister(&self, id: RegistrationId);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutStatus {
    Unassigned,
    Disabled,
    Active,
    /// Assigned and enabled, but not currently registered (a startup/runtime
    /// conflict). The saved chord is preserved, never erased.
    Unavailable,
    /// Approved by the spec; its owning feature has not shipped.
    NotYetImplemented,
}

/// One fired native hotkey. `repeat` is true for an OS-level auto-repeat
/// occurrence of an already-down combination; a fresh press is always false.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortcutEvent {
    pub action: ShortcutAction,
    pub repeat: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutFocus {
    Normal,
    /// The Hotkeys settings page's press-to-set field is capturing input.
    ShortcutEntry,
    /// An ordinary text/password/editor control has keyboard focus.
    TextEntry,
}

/// What a dispatched action should do. `UploadFile` does not go through
/// `AppCommand`: like the tray menu's own "Upload file" item, it opens the
/// native picker directly and only submits a command once the user has
/// actually chosen files.
///
/// `AppCommand` deliberately implements neither `Debug` nor `Clone` (it can
/// carry a secret token), so this type cannot derive them either.
pub enum ShortcutOutcome {
    Command(crate::app::events::AppCommand),
    OpenFilePicker(DestinationSelection),
}

fn command_for(action: ShortcutAction) -> Option<ShortcutOutcome> {
    use crate::app::events::{AppCommand, Window};
    match action {
        ShortcutAction::ShowHistory => {
            Some(ShortcutOutcome::Command(AppCommand::Show(Window::History)))
        }
        ShortcutAction::ShowSettings => {
            Some(ShortcutOutcome::Command(AppCommand::Show(Window::Settings)))
        }
        ShortcutAction::UploadFile => Some(ShortcutOutcome::OpenFilePicker(
            DestinationSelection::Inherit,
        )),
        _ => None,
    }
}

/// Owns the live native registrations and the transactional apply/startup
/// policy. `handle` is pure dispatch logic exercised without any native
/// dependency; `apply`/`new` call the injected `ShortcutRegistrar`.
pub struct ShortcutManager {
    registrar: Box<dyn ShortcutRegistrar>,
    live: HashMap<ShortcutAction, (RegistrationId, KeyChord)>,
    next_id: i32,
    bindings: Vec<ShortcutBinding>,
}

impl ShortcutManager {
    /// Best-effort startup registration: each enabled, available, global
    /// binding is attempted independently, so one saved conflict never
    /// blocks any other action. Returns the actions that failed to register;
    /// their saved chord is preserved exactly as loaded.
    pub fn new(
        registrar: Box<dyn ShortcutRegistrar>,
        bindings: Vec<ShortcutBinding>,
    ) -> (Self, Vec<ShortcutAction>) {
        let mut manager = ShortcutManager {
            registrar,
            live: HashMap::new(),
            next_id: 1,
            bindings,
        };
        let mut failed = Vec::new();
        for binding in manager.bindings.clone() {
            let ShortcutBinding {
                action,
                scope,
                chord: Some(chord),
                enabled: true,
                ..
            } = binding
            else {
                continue;
            };
            if scope != ShortcutScope::Global || !action.is_available_now() {
                continue;
            }
            let id = RegistrationId(manager.next_id);
            match manager.registrar.register(id, chord) {
                Ok(()) => {
                    manager.next_id += 1;
                    manager.live.insert(action, (id, chord));
                }
                Err(_) => failed.push(action),
            }
        }
        (manager, failed)
    }

    pub fn bindings(&self) -> &[ShortcutBinding] {
        &self.bindings
    }

    /// Resolves a fired native id back to its action. `None` for an id this
    /// manager no longer owns (e.g. a just-superseded registration whose
    /// unregister raced with an in-flight OS message).
    pub fn action_for_id(&self, id: i32) -> Option<ShortcutAction> {
        self.live
            .iter()
            .find(|(_, (reg, _))| reg.0 == id)
            .map(|(action, _)| *action)
    }

    pub fn status(&self, action: ShortcutAction) -> ShortcutStatus {
        if !action.is_available_now() {
            return ShortcutStatus::NotYetImplemented;
        }
        match self.bindings.iter().find(|b| b.action == action) {
            None => ShortcutStatus::Unassigned,
            Some(b) if b.chord.is_none() => ShortcutStatus::Unassigned,
            Some(b) if !b.enabled => ShortcutStatus::Disabled,
            Some(_) if self.live.contains_key(&action) => ShortcutStatus::Active,
            Some(_) => ShortcutStatus::Unavailable,
        }
    }

    /// Transactional replacement: validates the candidate, acquires every
    /// new/changed registration (freeing only this app's OWN registration of
    /// a chord being reassigned to a different action, never anyone else's),
    /// persists via `save` only once every acquisition has succeeded, then
    /// retires superseded registrations. Any failure — a conflicting
    /// registration or `save` itself — releases exactly what this attempt
    /// acquired/freed and restores what it freed, leaving the previous
    /// working mapping (both native and persisted) untouched.
    pub fn apply(
        &mut self,
        candidate: Vec<ShortcutBinding>,
        save: impl FnOnce(&[ShortcutBinding]) -> Result<(), AppError>,
    ) -> Result<(), AppError> {
        let full = ShortcutSettings {
            bindings: candidate.clone(),
        };
        full.validate()?;

        let desired: HashMap<ShortcutAction, KeyChord> = candidate
            .iter()
            .filter(|b| {
                b.enabled && b.action.is_available_now() && b.scope == ShortcutScope::Global
            })
            .filter_map(|b| b.chord.map(|c| (b.action, c)))
            .collect();
        let old_live = self.live.clone();

        // Registration ownership is by chord, not action. Reuse owned chords
        // during swaps; never release a working key before persistence commits.
        let mut acquired = Vec::new();
        let mut replacement = HashMap::new();
        let result = (|| {
            for (&action, &chord) in &desired {
                let slot = if let Some((id, _)) = old_live.values().find(|(_, c)| *c == chord) {
                    *id
                } else {
                    if self.next_id > 0xBFFF {
                        return Err(AppError::InUse(
                            "hotkey registration capacity exhausted; restart the app",
                        ));
                    }
                    let id = RegistrationId(self.next_id);
                    self.next_id += 1; // Never reuse failed/stale slots.
                    self.registrar.register(id, chord)?;
                    acquired.push(id);
                    id
                };
                replacement.insert(action, (slot, chord));
            }
            save(&candidate)
        })();
        if let Err(error) = result {
            for id in acquired {
                self.registrar.unregister(id);
            }
            return Err(error);
        }
        let kept: HashSet<_> = replacement.values().map(|(id, _)| *id).collect();
        for (id, _) in old_live.values() {
            if !kept.contains(id) {
                self.registrar.unregister(*id);
            }
        }
        self.live = replacement;
        self.bindings = candidate;
        Ok(())
    }

    /// Pure dispatch: never touches the registrar. Suppressed entirely while
    /// a shortcut-entry field or ordinary text control has focus, and a
    /// repeat occurrence of an already-dispatched press is always ignored.
    pub fn handle(&self, event: ShortcutEvent, focus: ShortcutFocus) -> Option<ShortcutOutcome> {
        if focus != ShortcutFocus::Normal || event.repeat {
            return None;
        }
        let outcome = command_for(event.action)?;
        match outcome {
            ShortcutOutcome::OpenFilePicker(_) => Some(ShortcutOutcome::OpenFilePicker(
                self.bindings
                    .iter()
                    .find(|b| b.action == event.action)?
                    .destination,
            )),
            other => Some(other),
        }
    }

    /// A candidate equal to the current bindings except the named actions
    /// (or every action, if `None`) reset to unassigned/disabled. Feeds
    /// straight into `apply`, so a reset is exactly as transactional as any
    /// other edit.
    pub fn reset_candidate(&self, actions: Option<&[ShortcutAction]>) -> Vec<ShortcutBinding> {
        self.bindings
            .iter()
            .map(|b| match actions {
                Some(subset) if !subset.contains(&b.action) => b.clone(),
                _ => ShortcutBinding::unassigned(b.action),
            })
            .collect()
    }
}

impl Drop for ShortcutManager {
    fn drop(&mut self) {
        for (_, (id, _)) in self.live.drain() {
            self.registrar.unregister(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn key(vk: u32) -> KeyCode {
        KeyCode::from_vk(vk).unwrap()
    }
    fn chord(ctrl: bool, shift: bool, vk: u32) -> KeyChord {
        KeyChord {
            ctrl,
            alt: false,
            shift,
            win: false,
            key: key(vk),
        }
    }
    fn binding(action: ShortcutAction, chord: KeyChord, enabled: bool) -> ShortcutBinding {
        ShortcutBinding {
            action,
            scope: action.scope(),
            chord: Some(chord),
            enabled,
            destination: DestinationSelection::Inherit,
        }
    }

    #[derive(Default)]
    struct FakeRegistrar {
        taken: Mutex<HashMap<i32, KeyChord>>,
        /// Chords the fake pretends are owned by another application.
        externally_taken: HashSet<KeyChord>,
    }
    impl ShortcutRegistrar for FakeRegistrar {
        fn register(&self, id: RegistrationId, chord: KeyChord) -> Result<(), AppError> {
            if self.externally_taken.contains(&chord)
                || self.taken.lock().unwrap().values().any(|c| *c == chord)
            {
                return Err(AppError::InUse("hotkey combination is already in use"));
            }
            self.taken.lock().unwrap().insert(id.0, chord);
            Ok(())
        }
        fn unregister(&self, id: RegistrationId) {
            self.taken.lock().unwrap().remove(&id.0);
        }
    }
    /// A thin forwarding newtype so the same `Arc<FakeRegistrar>` instance can
    /// still be inspected by its test after the manager owns a boxed copy.
    struct ForwardingRegistrar(Arc<FakeRegistrar>);
    impl ShortcutRegistrar for ForwardingRegistrar {
        fn register(&self, id: RegistrationId, chord: KeyChord) -> Result<(), AppError> {
            self.0.register(id, chord)
        }
        fn unregister(&self, id: RegistrationId) {
            self.0.unregister(id)
        }
    }

    fn manager_with(bindings: Vec<ShortcutBinding>) -> (ShortcutManager, Arc<FakeRegistrar>) {
        manager_with_conflicts(bindings, HashSet::new())
    }
    fn manager_with_conflicts(
        bindings: Vec<ShortcutBinding>,
        externally_taken: HashSet<KeyChord>,
    ) -> (ShortcutManager, Arc<FakeRegistrar>) {
        let fake = Arc::new(FakeRegistrar {
            externally_taken,
            ..FakeRegistrar::default()
        });
        let (manager, _failed) =
            ShortcutManager::new(Box::new(ForwardingRegistrar(fake.clone())), bindings);
        (manager, fake)
    }

    fn save_ok(_: &[ShortcutBinding]) -> Result<(), AppError> {
        Ok(())
    }

    #[test]
    fn hotkey_settings_survive_restart() {
        let settings = ShortcutSettings {
            bindings: vec![binding(
                ShortcutAction::ShowHistory,
                chord(true, false, 0x48),
                true,
            )],
        };
        settings.validate().unwrap();
        let effective = settings.effective();
        assert_eq!(
            effective
                .iter()
                .find(|b| b.action == ShortcutAction::ShowHistory)
                .unwrap()
                .chord,
            Some(chord(true, false, 0x48))
        );
        // Round-trip through JSON, the real persistence boundary.
        let json = serde_json::to_string(&settings).unwrap();
        let reloaded: ShortcutSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(reloaded, settings);
    }

    #[test]
    fn normalized_duplicates_rejected() {
        let settings = ShortcutSettings {
            bindings: vec![
                binding(ShortcutAction::ShowHistory, chord(true, false, 0x48), true),
                binding(ShortcutAction::ShowSettings, chord(true, false, 0x48), true),
            ],
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn disabled_duplicate_is_allowed() {
        // A disabled binding never registers, so it cannot collide with an
        // active one even if it names the same chord.
        let settings = ShortcutSettings {
            bindings: vec![
                binding(ShortcutAction::ShowHistory, chord(true, false, 0x48), true),
                binding(
                    ShortcutAction::ShowSettings,
                    chord(true, false, 0x48),
                    false,
                ),
            ],
        };
        settings.validate().unwrap();
    }

    #[test]
    fn failed_registration_keeps_old_mapping() {
        let mut externally_taken = HashSet::new();
        externally_taken.insert(chord(true, true, 0x53));
        let (mut manager, _fake) = manager_with_conflicts(
            vec![binding(
                ShortcutAction::ShowHistory,
                chord(true, false, 0x48),
                true,
            )],
            externally_taken,
        );
        let candidate = vec![binding(
            ShortcutAction::ShowHistory,
            chord(true, true, 0x53),
            true,
        )];
        let result = manager.apply(candidate, save_ok);
        assert!(result.is_err());
        assert_eq!(
            manager.bindings()[0].chord,
            Some(chord(true, false, 0x48)),
            "a failed registration must leave the previous working mapping"
        );
        assert_eq!(
            manager.status(ShortcutAction::ShowHistory),
            ShortcutStatus::Active
        );
    }

    #[test]
    fn failed_save_rolls_back_new_registrations() {
        let (mut manager, fake) = manager_with(vec![]);
        let candidate = vec![binding(
            ShortcutAction::ShowHistory,
            chord(true, false, 0x48),
            true,
        )];
        let result = manager.apply(candidate, |_| Err(AppError::Persistence("disk full")));
        assert!(result.is_err());
        assert!(
            fake.taken.lock().unwrap().is_empty(),
            "new registration must be released on save failure"
        );
        assert_eq!(
            manager.status(ShortcutAction::ShowHistory),
            ShortcutStatus::Unassigned
        );
    }

    #[test]
    fn swapping_own_bindings_reuses_registrations() {
        let (mut manager, fake) = manager_with(vec![
            binding(ShortcutAction::ShowHistory, chord(true, false, 0x48), true),
            binding(ShortcutAction::ShowSettings, chord(true, false, 0x53), true),
        ]);
        let swapped = vec![
            binding(ShortcutAction::ShowHistory, chord(true, false, 0x53), true),
            binding(ShortcutAction::ShowSettings, chord(true, false, 0x48), true),
        ];
        manager.apply(swapped, save_ok).unwrap();
        assert_eq!(manager.bindings()[0].chord, Some(chord(true, false, 0x53)));
        assert_eq!(manager.bindings()[1].chord, Some(chord(true, false, 0x48)));
        assert_eq!(
            manager.status(ShortcutAction::ShowHistory),
            ShortcutStatus::Active
        );
        assert_eq!(
            manager.status(ShortcutAction::ShowSettings),
            ShortcutStatus::Active
        );
        // Both chords are still exactly-once registered after the swap.
        assert_eq!(fake.taken.lock().unwrap().len(), 2);
    }

    #[test]
    fn disable_and_clear_unregister_only_that_action() {
        let (mut manager, fake) = manager_with(vec![
            binding(ShortcutAction::ShowHistory, chord(true, false, 0x48), true),
            binding(ShortcutAction::ShowSettings, chord(true, false, 0x53), true),
        ]);
        let mut candidate = manager.bindings().to_vec();
        candidate[0].enabled = false;
        manager.apply(candidate, save_ok).unwrap();
        assert_eq!(
            manager.status(ShortcutAction::ShowHistory),
            ShortcutStatus::Disabled
        );
        assert_eq!(
            manager.status(ShortcutAction::ShowSettings),
            ShortcutStatus::Active
        );
        assert_eq!(fake.taken.lock().unwrap().len(), 1);
    }

    #[test]
    fn startup_conflict_preserves_saved_choice_and_other_actions() {
        let mut externally_taken = HashSet::new();
        externally_taken.insert(chord(true, false, 0x48));
        let fake = Arc::new(FakeRegistrar {
            externally_taken,
            ..FakeRegistrar::default()
        });
        let (manager, failed) = ShortcutManager::new(
            Box::new(ForwardingRegistrar(fake)),
            vec![
                binding(ShortcutAction::ShowHistory, chord(true, false, 0x48), true),
                binding(ShortcutAction::ShowSettings, chord(true, false, 0x53), true),
            ],
        );
        assert_eq!(failed, vec![ShortcutAction::ShowHistory]);
        assert_eq!(
            manager.status(ShortcutAction::ShowHistory),
            ShortcutStatus::Unavailable
        );
        assert_eq!(
            manager.bindings()[0].chord,
            Some(chord(true, false, 0x48)),
            "the saved choice must not be erased"
        );
        assert_eq!(
            manager.status(ShortcutAction::ShowSettings),
            ShortcutStatus::Active
        );
    }

    #[test]
    fn upload_shortcut_preserves_explicit_destination_in_picker_outcome() {
        let destination = crate::app::destinations::DestinationId(uuid::Uuid::new_v4());
        let mut upload = binding(ShortcutAction::UploadFile, chord(true, false, 0x55), true);
        upload.destination = DestinationSelection::Specific(destination);
        let (manager, _) = manager_with(vec![upload]);
        let outcome = manager.handle(
            ShortcutEvent {
                action: ShortcutAction::UploadFile,
                repeat: false,
            },
            ShortcutFocus::Normal,
        );
        assert!(
            matches!(outcome, Some(ShortcutOutcome::OpenFilePicker(DestinationSelection::Specific(id))) if id == destination)
        );
    }

    #[test]
    fn shortcut_entry_never_dispatches_actions() {
        let (manager, _fake) = manager_with(vec![]);
        let event = ShortcutEvent {
            action: ShortcutAction::ShowHistory,
            repeat: false,
        };
        assert!(
            manager
                .handle(event, ShortcutFocus::ShortcutEntry)
                .is_none()
        );
        assert!(manager.handle(event, ShortcutFocus::TextEntry).is_none());
        assert!(manager.handle(event, ShortcutFocus::Normal).is_some());
    }

    #[test]
    fn repeat_keydown_dispatches_once() {
        let (manager, _fake) = manager_with(vec![]);
        let fresh = ShortcutEvent {
            action: ShortcutAction::ShowSettings,
            repeat: false,
        };
        let repeat = ShortcutEvent {
            repeat: true,
            ..fresh
        };
        assert!(manager.handle(fresh, ShortcutFocus::Normal).is_some());
        assert!(manager.handle(repeat, ShortcutFocus::Normal).is_none());
    }

    #[test]
    fn unavailable_actions_are_never_registered_or_dispatched() {
        let (manager, fake) = manager_with(vec![binding(
            ShortcutAction::RegionScreenshot,
            chord(true, false, 0x52),
            true,
        )]);
        assert!(
            fake.taken.lock().unwrap().is_empty(),
            "an unimplemented action's key must never be taken from the OS"
        );
        assert_eq!(
            manager.status(ShortcutAction::RegionScreenshot),
            ShortcutStatus::NotYetImplemented
        );
        let event = ShortcutEvent {
            action: ShortcutAction::RegionScreenshot,
            repeat: false,
        };
        assert!(manager.handle(event, ShortcutFocus::Normal).is_none());
    }

    #[test]
    fn reset_transactional() {
        let (mut manager, fake) = manager_with(vec![binding(
            ShortcutAction::ShowHistory,
            chord(true, false, 0x48),
            true,
        )]);
        let candidate = manager.reset_candidate(None);
        manager.apply(candidate, save_ok).unwrap();
        assert_eq!(
            manager.status(ShortcutAction::ShowHistory),
            ShortcutStatus::Unassigned
        );
        assert!(fake.taken.lock().unwrap().is_empty());
    }

    #[test]
    fn reset_rolls_back_on_save_failure() {
        let (mut manager, fake) = manager_with(vec![binding(
            ShortcutAction::ShowHistory,
            chord(true, false, 0x48),
            true,
        )]);
        let candidate = manager.reset_candidate(None);
        let result = manager.apply(candidate, |_| Err(AppError::Persistence("io")));
        assert!(result.is_err());
        assert_eq!(
            manager.status(ShortcutAction::ShowHistory),
            ShortcutStatus::Active
        );
        assert_eq!(fake.taken.lock().unwrap().len(), 1);
    }
}
