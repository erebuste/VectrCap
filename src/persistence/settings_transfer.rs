//! Portable preferences. This format has no credential/history dependency.
use crate::{
    app::{
        destination_manager::DestinationManager,
        destinations::{DestinationId, DestinationSelection, Destinations},
        error::AppError,
        shortcuts::{KeyChord, ShortcutAction, ShortcutBinding, ShortcutManager, ShortcutScope},
    },
    capture::{preferences::RegionCapturePreferences, request::ScreenshotPreferences},
    persistence::{filename::FilenamePreset, settings::*},
    recording::{audio::devices::SourceKind, options::*, preferences::RecordingPreferences},
    workflow::profile::*,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::File,
    io::{Read, Write},
    path::Path,
};
const INVALID: AppError =
    AppError::InvalidInput("invalid preferences backup; check version, fields and values");
pub const MAX_TRANSFER_BYTES: usize = 1024 * 1024;

/// Dedicated allowlist: never serialize runtime Settings here. No local IDs,
/// credential references, endpoints, installation data, history or runtime state.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsExportV1 {
    pub schema_version: u32,
    pub destinations: Vec<DestinationExportV1>,
    pub default_destination: Option<u16>,
    pub shortcuts: Vec<ShortcutExportV1>,
    pub tasks: TaskProfilesExportV1,
    pub region: RegionCapturePreferences,
    pub screenshot: ScreenshotPreferences,
    pub recording: RecordingExportV1,
    pub image: ImageExportPreferences,
    pub pin: crate::pin::PinOptions,
    pub tools: ToolPreferences,
    pub filename: FilenamePreset,
    pub appearance: AppearancePreferences,
    pub archive: ArchivePolicy,
    pub notifications_enabled: bool,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationExportV1 {
    pub key: u16,
    pub name: String,
    pub origin: ServerOrigin,
    pub link_domain_override: Option<LinkOrigin>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShortcutExportV1 {
    pub action: ShortcutAction,
    pub scope: ShortcutScope,
    pub chord: Option<KeyChord>,
    pub enabled: bool,
    pub destination: Option<u16>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskProfileExportV1 {
    pub capture: BTreeSet<CaptureTask>,
    pub after_upload: BTreeSet<UploadTask>,
    pub before_upload_review: bool,
    pub scope: ProfileScope,
    pub destination: Option<u16>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskProfilesExportV1 {
    pub global: TaskProfileExportV1,
    pub overrides: BTreeMap<ShortcutAction, TaskProfileExportV1>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioExportV1 {
    pub mode: AudioMode,
    pub system_gain: u8,
    pub microphone_gain: u8,
    pub aac_kbps: u32,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingExportV1 {
    pub limits: RecordingLimits,
    pub gif: GifOptions,
    pub mp4_fps: u32,
    pub mp4_quality: Quality,
    pub mp4_custom_bitrate: Option<u32>,
    pub audio: AudioExportV1,
    pub cursor: bool,
    pub countdown_seconds: u32,
    pub output_size: OutputSize,
    pub confirm_discard: bool,
    pub gif_timed_stop: Option<u32>,
    pub mp4_timed_stop: Option<u32>,
}
/// Safe groups deliberately omit every behavior that needs separate consent.
#[derive(Clone, Default)]
pub struct ImportSelection {
    pub tools: bool,
    pub destinations: bool,
    pub shortcuts: bool,
    pub tasks: bool,
    pub capture: bool,
    pub recording: bool,
    pub image: bool,
    pub pin: bool,
    pub filename: bool,
    pub appearance: bool,
    pub archive: bool,
    pub notifications: bool,
    pub automation: bool,
    pub cleanup: bool,
    pub default_destination: bool,
    pub audio_enablement: bool,
    pub destination_mapping: BTreeMap<u16, DestinationId>,
}
impl ImportSelection {
    pub fn safe_preferences() -> Self {
        Self {
            tools: true,
            destinations: true,
            shortcuts: true,
            tasks: true,
            capture: true,
            recording: true,
            image: true,
            pin: true,
            filename: true,
            appearance: true,
            archive: true,
            notifications: true,
            ..Default::default()
        }
    }
}
/// No Debug/Serialize: preview owns only the allowlist, never runtime Settings.
#[derive(Clone)]
pub struct ImportPreview {
    base_revision: u64,
    document: SettingsExportV1,
    diff: Vec<GroupDiff>,
    pub conflicts: Vec<&'static str>,
    pub errors: Vec<&'static str>,
    pub destination_references: Vec<DestinationReference>,
}
#[derive(Clone)]
pub struct DestinationReference {
    pub key: u16,
    pub is_default: bool,
    pub shortcuts: Vec<ShortcutAction>,
    pub global_profile: bool,
    pub profile_overrides: Vec<ShortcutAction>,
}
#[derive(Clone)]
pub struct GroupDiff {
    pub group: &'static str,
    pub summary: String,
    pub requires_opt_in: bool,
    pub details: Vec<String>,
}
impl ImportPreview {
    pub fn base_revision(&self) -> u64 {
        self.base_revision
    }
    pub fn diff(&self) -> &[GroupDiff] {
        &self.diff
    }
    pub fn refresh(&self, current: &Settings) -> Result<Self, AppError> {
        let bytes = serde_json::to_vec(&self.document).map_err(|_| INVALID)?;
        preview_import(&bytes, current)
    }
    pub fn destinations(&self) -> &[DestinationExportV1] {
        &self.document.destinations
    }
}
pub trait ImportAudioResolver {
    fn resolve(&self, kind: SourceKind) -> Result<EndpointSelection, AppError>;
}
pub struct NativeImportAudioResolver;
impl ImportAudioResolver for NativeImportAudioResolver {
    fn resolve(&self, kind: SourceKind) -> Result<EndpointSelection, AppError> {
        let o = AudioOptions {
            mode: match kind {
                SourceKind::System => AudioMode::System,
                SourceKind::Microphone => AudioMode::Microphone,
            },
            ..Default::default()
        };
        let sources = crate::recording::audio::wasapi::resolve_recording_sources(Some(&o))?;
        sources
            .into_iter()
            .next()
            .map(|s| EndpointSelection::Id(s.id))
            .ok_or(AppError::NeedsSetup(
                "select a local audio device before importing audio",
            ))
    }
}
pub struct SettingsTransfer<'a> {
    manager: &'a mut DestinationManager,
    shortcuts: &'a mut ShortcutManager,
    audio: &'a dyn ImportAudioResolver,
}
impl<'a> SettingsTransfer<'a> {
    pub fn new(
        manager: &'a mut DestinationManager,
        shortcuts: &'a mut ShortcutManager,
        audio: &'a dyn ImportAudioResolver,
    ) -> Self {
        Self {
            manager,
            shortcuts,
            audio,
        }
    }
    pub fn export(settings: &Settings) -> Result<Vec<u8>, AppError> {
        settings.validate()?;
        let document = SettingsExportV1::from_settings(settings)?;
        document.validate()?;
        let bytes = serde_json::to_vec_pretty(&document)
            .map_err(|_| AppError::Persistence("preferences backup could not be encoded"))?;
        if bytes.len() > MAX_TRANSFER_BYTES {
            return Err(AppError::InvalidInput("preferences backup exceeds 1 MiB"));
        }
        Ok(bytes)
    }
    pub fn apply_import(
        &mut self,
        preview: ImportPreview,
        groups: ImportSelection,
    ) -> Result<(), AppError> {
        self.manager.ensure_revision(preview.base_revision)?;
        // Complete reference, value and selection validation before device or OS calls.
        let mut candidate = preview
            .document
            .candidate(self.manager.settings(), &groups)?;
        self.manager
            .preflight_preferences(&candidate, preview.base_revision)?;
        if groups.recording && groups.audio_enablement {
            let audio = &mut candidate.recording_options.mp4.audio;
            if matches!(audio.mode, AudioMode::System | AudioMode::Both) {
                audio.system = resolved(self.audio, SourceKind::System)?;
            }
            if matches!(audio.mode, AudioMode::Microphone | AudioMode::Both) {
                audio.microphone = resolved(self.audio, SourceKind::Microphone)?;
            }
        }
        candidate.encode_persistence()?;
        let revision = preview.base_revision;
        if candidate == *self.manager.settings() {
            return Ok(());
        }
        // Same transaction as manual hotkey edits: acquire first, commit ALL
        // groups once, publish only after durable success. No credential operations.
        if candidate.shortcuts == self.manager.settings().shortcuts {
            self.manager.commit_preferences(candidate, revision)
        } else {
            self.shortcuts
                .apply(candidate.shortcuts.bindings.clone(), |_| {
                    self.manager.commit_preferences(candidate, revision)
                })
        }
    }
}
fn resolved(
    audio: &dyn ImportAudioResolver,
    kind: SourceKind,
) -> Result<EndpointSelection, AppError> {
    match audio.resolve(kind)? {
        EndpointSelection::Id(id)
            if !id.is_empty() && id.len() <= 4096 && !id.chars().any(char::is_control) =>
        {
            Ok(EndpointSelection::Id(id))
        }
        _ => Err(AppError::NeedsSetup(
            "audio import requires a resolved local device",
        )),
    }
}
impl TaskProfileExportV1 {
    fn from_profile(p: &TaskProfile, keys: &HashMap<DestinationId, u16>) -> Result<Self, AppError> {
        Ok(Self {
            capture: p.capture.clone(),
            after_upload: p.after_upload.clone(),
            before_upload_review: p.before_upload_review,
            scope: p.scope,
            destination: export_ref(p.destination, keys)?,
        })
    }
    fn profile(&self, keys: &BTreeMap<u16, DestinationId>) -> Result<TaskProfile, AppError> {
        Ok(TaskProfile {
            capture: self.capture.clone(),
            after_upload: self.after_upload.clone(),
            before_upload_review: self.before_upload_review,
            scope: self.scope,
            destination: import_ref(self.destination, keys)?,
        })
    }
}
fn export_ref(
    selection: DestinationSelection,
    keys: &HashMap<DestinationId, u16>,
) -> Result<Option<u16>, AppError> {
    match selection {
        DestinationSelection::Inherit => Ok(None),
        DestinationSelection::Specific(id) => keys.get(&id).copied().map(Some).ok_or(INVALID),
    }
}
fn import_ref(
    key: Option<u16>,
    keys: &BTreeMap<u16, DestinationId>,
) -> Result<DestinationSelection, AppError> {
    match key {
        None => Ok(DestinationSelection::Inherit),
        Some(key) => keys
            .get(&key)
            .copied()
            .map(DestinationSelection::Specific)
            .ok_or(AppError::InvalidInput(
                "select destinations or explicitly map every imported reference",
            )),
    }
}
impl RecordingExportV1 {
    fn from_settings(s: &Settings) -> Self {
        let p = &s.recording_options;
        let m = &p.mp4;
        let a = &m.audio;
        Self {
            limits: s.recording,
            gif: p.gif.clone(),
            mp4_fps: m.fps,
            mp4_quality: m.quality,
            mp4_custom_bitrate: m.custom_bitrate,
            audio: AudioExportV1 {
                mode: a.mode,
                system_gain: a.system_gain,
                microphone_gain: a.microphone_gain,
                aac_kbps: a.aac_kbps,
            },
            cursor: p.cursor,
            countdown_seconds: p.countdown_seconds,
            output_size: p.output_size,
            confirm_discard: p.confirm_discard,
            gif_timed_stop: p.gif_timed_stop,
            mp4_timed_stop: p.mp4_timed_stop,
        }
    }
    fn preferences(&self) -> RecordingPreferences {
        RecordingPreferences {
            gif: self.gif.clone(),
            mp4: Mp4Options {
                fps: self.mp4_fps,
                quality: self.mp4_quality,
                custom_bitrate: self.mp4_custom_bitrate,
                audio: AudioOptions {
                    mode: self.audio.mode,
                    system_gain: self.audio.system_gain,
                    microphone_gain: self.audio.microphone_gain,
                    aac_kbps: self.audio.aac_kbps,
                    ..Default::default()
                },
            },
            cursor: self.cursor,
            countdown_seconds: self.countdown_seconds,
            output_size: self.output_size,
            confirm_discard: self.confirm_discard,
            gif_timed_stop: self.gif_timed_stop,
            mp4_timed_stop: self.mp4_timed_stop,
        }
    }
}
impl SettingsExportV1 {
    fn from_settings(s: &Settings) -> Result<Self, AppError> {
        let keys: HashMap<_, _> = s
            .destinations
            .iter()
            .enumerate()
            .map(|(i, d)| (d.id, (i + 1) as u16))
            .collect();
        let destinations = s
            .destinations
            .iter()
            .map(|d| DestinationExportV1 {
                key: keys[&d.id],
                name: d.name.clone(),
                origin: d.origin.clone(),
                link_domain_override: d.link_domain_override.clone(),
            })
            .collect();
        let shortcuts = s
            .shortcuts
            .bindings
            .iter()
            .map(|b| {
                Ok(ShortcutExportV1 {
                    action: b.action,
                    scope: b.scope,
                    chord: b.chord,
                    enabled: b.enabled,
                    destination: export_ref(b.destination, &keys)?,
                })
            })
            .collect::<Result<_, AppError>>()?;
        let global = TaskProfileExportV1::from_profile(&s.tasks.global, &keys)?;
        let overrides = s
            .tasks
            .overrides
            .iter()
            .map(|(a, p)| Ok((*a, TaskProfileExportV1::from_profile(p, &keys)?)))
            .collect::<Result<_, AppError>>()?;
        Ok(Self {
            schema_version: 1,
            destinations,
            default_destination: s.destinations.default_id().map(|id| keys[&id]),
            shortcuts,
            tasks: TaskProfilesExportV1 { global, overrides },
            region: s.region.clone(),
            screenshot: s.screenshot.clone(),
            recording: RecordingExportV1::from_settings(s),
            image: s.image.clone(),
            pin: s.pin.clone(),
            tools: s.tools.clone(),
            filename: s.filename.clone(),
            appearance: s.appearance,
            archive: s.archive.clone(),
            notifications_enabled: s.notifications_enabled,
        })
    }
    fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != 1
            || self.destinations.len() > crate::app::destinations::MAX_DESTINATIONS
        {
            return Err(INVALID);
        }
        let mut keys = BTreeMap::new();
        let mut destinations = Destinations::default();
        for d in &self.destinations {
            if d.key == 0 || d.name != d.name.trim() || keys.contains_key(&d.key) {
                return Err(INVALID);
            }
            let id = destinations.add(&d.name, d.origin.clone(), d.link_domain_override.clone())?;
            keys.insert(d.key, id);
        }
        match self.default_destination {
            Some(key) => destinations.set_default(*keys.get(&key).ok_or(INVALID)?)?,
            None if destinations.is_empty() => {}
            _ => return Err(INVALID),
        }
        // Validate the FULL imported configuration even if a group is deselected.
        let mut s = Settings {
            destinations,
            ..Default::default()
        };
        s.tasks = self.task_profiles(&keys)?;
        s.shortcuts.bindings = self.shortcut_bindings(&keys)?;
        s.region = self.region.clone();
        s.screenshot = self.screenshot.clone();
        s.recording = self.recording.limits;
        s.recording_options = self.recording.preferences();
        s.image = self.image.clone();
        s.pin = self.pin.clone();
        s.tools = self.tools.clone();
        s.filename = self.filename.clone();
        s.appearance = self.appearance;
        s.archive = self.archive.clone();
        s.validate()
    }
    fn task_profiles(&self, keys: &BTreeMap<u16, DestinationId>) -> Result<TaskProfiles, AppError> {
        Ok(TaskProfiles {
            global: self.tasks.global.profile(keys)?,
            overrides: self
                .tasks
                .overrides
                .iter()
                .map(|(a, p)| Ok((*a, p.profile(keys)?)))
                .collect::<Result<_, AppError>>()?,
        })
    }
    fn shortcut_bindings(
        &self,
        keys: &BTreeMap<u16, DestinationId>,
    ) -> Result<Vec<ShortcutBinding>, AppError> {
        self.shortcuts
            .iter()
            .map(|b| {
                Ok(ShortcutBinding {
                    action: b.action,
                    scope: b.scope,
                    chord: b.chord,
                    enabled: b.enabled,
                    destination: import_ref(b.destination, keys)?,
                })
            })
            .collect()
    }
    fn candidate(&self, current: &Settings, g: &ImportSelection) -> Result<Settings, AppError> {
        self.validate()?;
        let mut s = current.clone();
        let mut keys = BTreeMap::new();
        let mut mapped = HashSet::new();
        // Mappings are explicit, injective and exact-origin. Names and same-origin
        // siblings confer no credential authority; mapped records are never edited.
        for (&key, &id) in &g.destination_mapping {
            let d = self
                .destinations
                .iter()
                .find(|d| d.key == key)
                .ok_or(INVALID)?;
            let existing = current.destinations.get(id).ok_or(INVALID)?;
            if existing.origin != d.origin || !mapped.insert(id) {
                return Err(AppError::InvalidInput(
                    "destination mapping must be distinct and match the exact origin",
                ));
            }
            keys.insert(key, id);
        }
        if g.destinations {
            if current.destinations.is_empty()
                && !self.destinations.is_empty()
                && !g.default_destination
            {
                return Err(AppError::InvalidInput(
                    "adding the first destination requires explicit default-destination selection",
                ));
            }
            for d in &self.destinations {
                if let std::collections::btree_map::Entry::Vacant(slot) = keys.entry(d.key) {
                    let id = s.destinations.add(
                        &d.name,
                        d.origin.clone(),
                        d.link_domain_override.clone(),
                    )?;
                    slot.insert(id);
                }
            }
        }
        if g.default_destination
            && let Some(key) = self.default_destination
        {
            s.destinations
                .set_default(*keys.get(&key).ok_or(AppError::InvalidInput(
                    "map or import the default destination first",
                ))?)?;
        }
        if g.shortcuts {
            s.shortcuts.bindings = self.shortcut_bindings(&keys)?;
        }
        if g.tasks {
            s.tasks = self.task_profiles(&keys)?;
            if !g.automation {
                s.tasks
                    .global
                    .preserve_automation_from(&current.tasks.global);
                // A removed override can otherwise inherit enabled automation
                // from the global profile. Preserve each action's effective gates.
                for &action in ShortcutAction::all()
                    .iter()
                    .filter(|a| a.supports_task_profile())
                {
                    let previous = current
                        .tasks
                        .overrides
                        .get(&action)
                        .unwrap_or(&current.tasks.global);
                    let mut effective = s
                        .tasks
                        .overrides
                        .get(&action)
                        .unwrap_or(&s.tasks.global)
                        .clone();
                    let before = effective.clone();
                    effective.preserve_automation_from(previous);
                    if effective != before {
                        s.tasks.overrides.insert(action, effective);
                    }
                }
            }
        }
        if g.capture {
            s.region = self.region.clone();
            s.screenshot = self.screenshot.clone();
        }
        if g.recording {
            s.recording = self.recording.limits;
            s.recording_options = self.recording.preferences();
            if !g.audio_enablement {
                s.recording_options.mp4.audio = current.recording_options.mp4.audio.clone();
            }
        }
        if g.tools {
            s.tools = self.tools.clone();
        }
        if g.image {
            s.image = self.image.clone();
        }
        if g.pin {
            s.pin = self.pin.clone();
        }
        if g.filename {
            s.filename = self.filename.clone();
        }
        if g.appearance {
            s.appearance = self.appearance;
        }
        if g.archive {
            s.archive = self.archive.clone();
            if !g.cleanup {
                s.archive.cleanup_age_days = current.archive.cleanup_age_days;
            }
        }
        if g.notifications {
            s.notifications_enabled = self.notifications_enabled;
        }
        s.validate()?;
        Ok(s)
    }
}
/// Bounded, typed parsing; serde's potentially input-bearing errors never escape.
/// No adapters, runtime Settings clone, file I/O, device resolution or execution.
pub fn preview_import(bytes: &[u8], current: &Settings) -> Result<ImportPreview, AppError> {
    if bytes.len() > MAX_TRANSFER_BYTES {
        return Err(INVALID);
    }
    let document: SettingsExportV1 = serde_json::from_slice(bytes).map_err(|_| INVALID)?;
    document.validate().map_err(|_| INVALID)?;
    let old = SettingsExportV1::from_settings(current)?;
    let mut diff = Vec::new();
    let mut add = |group, changed, summary: String, opt| {
        if changed {
            diff.push(GroupDiff {
                group,
                summary,
                requires_opt_in: opt,
                details: Vec::new(),
            });
        }
    };
    add(
        "Destinations",
        document.destinations != old.destinations,
        format!(
            "{} portable destinations; new connections need tokens or explicit mapping",
            document.destinations.len()
        ),
        false,
    );
    add(
        "Hotkeys and tool bindings",
        document.shortcuts != old.shortcuts,
        format!("{} validated bindings", document.shortcuts.len()),
        false,
    );
    add(
        "Task profiles",
        document.tasks != old.tasks,
        format!(
            "Global profile and {} action overrides",
            document.tasks.overrides.len()
        ),
        false,
    );
    add(
        "Capture and region",
        document.region != old.region || document.screenshot != old.screenshot,
        "Lens, dimming, size presets, fixed mouse actions, delay and cursor".into(),
        false,
    );
    add(
        "Recording",
        document.recording != old.recording,
        "GIF/MP4 rates, quality, timing and cursor".into(),
        false,
    );
    add(
        "Image export",
        document.image != old.image,
        "Image format, quality and resize".into(),
        false,
    );
    add(
        "Tools",
        document.tools != old.tools,
        "Annotation colors, widths, text, highlight and eraser defaults".into(),
        false,
    );
    add(
        "Pin",
        document.pin != old.pin,
        "Pin display defaults".into(),
        false,
    );
    add(
        "Filename",
        document.filename != old.filename,
        "Validated filename preset".into(),
        false,
    );
    add(
        "Appearance",
        document.appearance != old.appearance,
        format!(
            "UI scale {}% -> {}%",
            old.appearance.ui_scale_percent, document.appearance.ui_scale_percent
        ),
        false,
    );
    add(
        "Archive",
        document.archive != old.archive,
        "Archive storage policy and path; existing files stay in place".into(),
        false,
    );
    add(
        "Notifications",
        document.notifications_enabled != old.notifications_enabled,
        format!(
            "Completion notices: {} -> {}",
            old.notifications_enabled, document.notifications_enabled
        ),
        false,
    );
    add(
        "Automation",
        document.tasks != old.tasks,
        "Automatic upload and open-link behavior require separate selection".into(),
        true,
    );
    add(
        "Cleanup",
        document.archive.cleanup_age_days != old.archive.cleanup_age_days,
        "Cleanup age changes require separate selection".into(),
        true,
    );
    add(
        "Default destination",
        document.default_destination.is_some(),
        "Default routing requires explicit selection and remapping".into(),
        true,
    );
    add(
        "Audio enablement",
        document.recording.audio.mode != AudioMode::Off,
        "Imported audio mode requires explicit selection and local device resolution".into(),
        true,
    );
    // Build old/new values only from the safe DTO, never runtime settings or
    // raw input. Expandable field-level details let owners review real changes.
    let old_value = serde_json::to_value(&old).map_err(|_| INVALID)?;
    let new_value = serde_json::to_value(&document).map_err(|_| INVALID)?;
    for group in &mut diff {
        let fields: &[&str] = match group.group {
            "Destinations" => &["destinations"],
            "Hotkeys and tool bindings" => &["shortcuts"],
            "Task profiles" | "Automation" => &["tasks"],
            "Capture and region" => &["region", "screenshot"],
            "Recording" | "Audio enablement" => &["recording"],
            "Image export" => &["image"],
            "Tools" => &["tools"],
            "Pin" => &["pin"],
            "Filename" => &["filename"],
            "Appearance" => &["appearance"],
            "Archive" | "Cleanup" => &["archive"],
            "Notifications" => &["notifications_enabled"],
            "Default destination" => &["default_destination"],
            _ => &[],
        };
        for &field in fields {
            field_diff(
                field,
                &old_value[field],
                &new_value[field],
                &mut group.details,
            );
        }
    }
    let conflicts=document.destinations.iter().filter(|d|current.destinations.iter().any(|old|old.name.eq_ignore_ascii_case(&d.name))).map(|_|"A destination name already exists; explicitly map an exact-origin connection or deselect destinations").collect();
    let destination_references = document
        .destinations
        .iter()
        .map(|d| DestinationReference {
            key: d.key,
            is_default: document.default_destination == Some(d.key),
            shortcuts: document
                .shortcuts
                .iter()
                .filter(|b| b.destination == Some(d.key))
                .map(|b| b.action)
                .collect(),
            global_profile: document.tasks.global.destination == Some(d.key),
            profile_overrides: document
                .tasks
                .overrides
                .iter()
                .filter(|(_, p)| p.destination == Some(d.key))
                .map(|(a, _)| *a)
                .collect(),
        })
        .collect();
    Ok(ImportPreview {
        base_revision: current.revision,
        document,
        diff,
        conflicts,
        errors: vec![],
        destination_references,
    })
}
fn field_diff(
    path: &str,
    before: &serde_json::Value,
    after: &serde_json::Value,
    out: &mut Vec<String>,
) {
    if before == after {
        return;
    }
    match (before, after) {
        (serde_json::Value::Object(old), serde_json::Value::Object(new)) => {
            let fields: BTreeSet<_> = old.keys().chain(new.keys()).collect();
            for key in fields {
                field_diff(&format!("{path}.{key}"), &before[key], &after[key], out);
            }
        }
        (serde_json::Value::Array(old), serde_json::Value::Array(new)) => {
            for i in 0..old.len().max(new.len()) {
                field_diff(&format!("{path}[{i}]"), &before[i], &after[i], out);
            }
        }
        _ => out.push(format!("{path}: {before} -> {after}")),
    }
}
pub fn read_import(path: &Path, current: &Settings) -> Result<ImportPreview, AppError> {
    let file = File::open(path).map_err(|e| AppError::io("reading preferences backup", &e))?;
    if !file
        .metadata()
        .map_err(|e| AppError::io("reading preferences backup", &e))?
        .is_file()
    {
        return Err(INVALID);
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    file.take(MAX_TRANSFER_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| AppError::io("reading preferences backup", &e))?;
    preview_import(&bytes, current)
}
/// Writes one user-selected file in an existing directory. Atomic no-clobber by
/// default, replacement only after explicit confirmation. No fallible bookkeeping
/// follows publication, so a successful export cannot become a cleanup failure.
pub fn write_export(path: &Path, settings: &Settings, replace: bool) -> Result<(), AppError> {
    let bytes = SettingsTransfer::export(settings)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or(INVALID)?;
    let mut tmp = tempfile::Builder::new()
        .prefix(crate::persistence::PARTIAL_PREFIX)
        .tempfile_in(parent)
        .map_err(|e| AppError::io("writing preferences backup", &e))?;
    tmp.write_all(&bytes)
        .map_err(|e| AppError::io("writing preferences backup", &e))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| AppError::io("writing preferences backup", &e))?;
    if replace {
        super::settings::replace_write_through(tmp.path(), path)?;
    } else {
        tmp.persist_noclobber(path)
            .map_err(|e| AppError::io("publishing preferences backup", &e.error))?;
    }
    Ok(())
}
