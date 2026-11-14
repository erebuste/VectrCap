use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::app::destinations::Destinations;
use crate::app::error::AppError;
use crate::app::shortcuts::ShortcutSettings;
use crate::persistence::filename::FilenamePreset;
use crate::persistence::{PARTIAL_PREFIX, default_archive_root};

pub const SETTINGS_VERSION: u32 = 1;
const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;
const MAX_ORIGIN_BYTES: usize = 2048;

/// Normalized `https://host[:port]` origin that receives authorized requests.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ServerOrigin(String);

impl ServerOrigin {
    pub fn parse(input: &str) -> Result<Self, AppError> {
        parse_https_origin(input).map(ServerOrigin)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ServerOrigin {
    type Error = AppError;
    fn try_from(s: String) -> Result<Self, AppError> {
        ServerOrigin::parse(&s)
    }
}

impl From<ServerOrigin> for String {
    fn from(o: ServerOrigin) -> String {
        o.0
    }
}

/// Link-domain override: same strict parser, but only ever used as a validated
/// domain choice, never as an authorization request target.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LinkOrigin(String);

impl LinkOrigin {
    pub fn parse(input: &str) -> Result<Self, AppError> {
        parse_https_origin(input).map(LinkOrigin)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for LinkOrigin {
    type Error = AppError;
    fn try_from(s: String) -> Result<Self, AppError> {
        LinkOrigin::parse(&s)
    }
}

impl From<LinkOrigin> for String {
    fn from(o: LinkOrigin) -> String {
        o.0
    }
}

/// Strict HTTPS origin parser. The raw input is checked before URL parsing
/// because WHATWG parsing silently strips tabs/newlines and repairs slashes.
fn parse_https_origin(input: &str) -> Result<String, AppError> {
    if input.is_empty() || input.len() > MAX_ORIGIN_BYTES {
        return Err(AppError::InvalidInput("origin must be 1 to 2048 bytes"));
    }
    if input.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(AppError::InvalidInput(
            "origin contains whitespace or control characters",
        ));
    }
    if input.contains(['@', '\\', '?', '#', '%']) {
        return Err(AppError::InvalidInput(
            "origin must not contain credentials, a query, a fragment, or escapes",
        ));
    }
    let has_scheme = input
        .get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case("https://"));
    if !has_scheme || input[8..].starts_with('/') {
        return Err(AppError::InvalidInput(
            "origin must start with https:// followed by a host",
        ));
    }
    // `? # \` are already refused, so the authority ends at the first '/'. Whatever
    // follows must be exactly "/": WHATWG would collapse "/a/.." or "/." to "/".
    if input[8..].find('/').is_some_and(|i| &input[8 + i..] != "/") {
        return Err(AppError::InvalidInput("origin must not contain a path"));
    }
    let url = Url::parse(input).map_err(|_| AppError::InvalidInput("origin is not a valid URL"))?;
    if url.scheme() != "https" || url.host().is_none() {
        return Err(AppError::InvalidInput("origin must be an HTTPS host"));
    }
    if url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(AppError::InvalidInput(
            "origin must not contain a path, query, fragment, or credentials",
        ));
    }
    if url.port() == Some(0) {
        return Err(AppError::InvalidInput("origin port is invalid"));
    }
    Ok(url.origin().ascii_serialization())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StorageMode {
    #[default]
    AlwaysSave,
    PendingOrFailed,
    NeverAutomatic,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchivePolicy {
    pub storage_mode: StorageMode,
    /// `None` means the Pictures known folder + `VectrCap`, resolved at use time.
    pub archive_root: Option<PathBuf>,
    /// Optional age-based cleanup, 1–3650 days; disabled (`None`) by default.
    pub cleanup_age_days: Option<u16>,
}

impl ArchivePolicy {
    /// Validates values only; never creates, moves, or deletes folders.
    pub fn validate(&self) -> Result<(), AppError> {
        if let Some(days) = self.cleanup_age_days
            && !(1..=3650).contains(&days)
        {
            return Err(AppError::InvalidInput("cleanup age must be 1 to 3650 days"));
        }
        if let Some(root) = &self.archive_root {
            validate_archive_root(root)?;
        }
        Ok(())
    }

    pub fn resolved_root(&self) -> Result<PathBuf, AppError> {
        match &self.archive_root {
            Some(root) => Ok(root.clone()),
            None => default_archive_root(),
        }
    }

    /// Whether a finalized capture/recording gets an automatic archive copy.
    /// Explicit Save is independent of this policy.
    pub fn should_auto_archive(&self, upload_bound: bool) -> bool {
        match self.storage_mode {
            StorageMode::AlwaysSave => true,
            StorageMode::PendingOrFailed => upload_bound,
            StorageMode::NeverAutomatic => false,
        }
    }
}

fn validate_archive_root(root: &Path) -> Result<(), AppError> {
    let text = root.to_str().ok_or(AppError::InvalidInput(
        "archive folder path must be valid Unicode",
    ))?;
    if !root.is_absolute() || text.chars().any(char::is_control) {
        return Err(AppError::InvalidInput(
            "archive folder must be an absolute path",
        ));
    }
    for component in root.components() {
        match component {
            #[cfg(windows)]
            Component::Prefix(p) => {
                use std::path::Prefix;
                if !matches!(
                    p.kind(),
                    Prefix::Disk(_)
                        | Prefix::VerbatimDisk(_)
                        | Prefix::UNC(..)
                        | Prefix::VerbatimUNC(..)
                ) {
                    return Err(AppError::InvalidInput(
                        "archive folder must not be a device path",
                    ));
                }
            }
            Component::ParentDir => {
                return Err(AppError::InvalidInput(
                    "archive folder must not contain '..'",
                ));
            }
            Component::Normal(part) if part.to_string_lossy().contains(':') => {
                return Err(AppError::InvalidInput(
                    "archive folder must not name an alternate data stream",
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingLimits {
    /// Maximum active GIF duration, 1–300 seconds (default 60).
    pub gif_max_seconds: u32,
    /// Maximum active MP4 duration, 1–60 minutes (default 10).
    pub mp4_max_minutes: u32,
}

impl Default for RecordingLimits {
    fn default() -> Self {
        RecordingLimits {
            gif_max_seconds: 60,
            mp4_max_minutes: 10,
        }
    }
}

impl RecordingLimits {
    pub fn validate(&self) -> Result<(), AppError> {
        if !(1..=300).contains(&self.gif_max_seconds) {
            return Err(AppError::InvalidInput(
                "GIF duration limit must be 1 to 300 seconds",
            ));
        }
        if !(1..=60).contains(&self.mp4_max_minutes) {
            return Err(AppError::InvalidInput(
                "MP4 duration limit must be 1 to 60 minutes",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoryLayout {
    #[default]
    Gallery,
    List,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HistoryPreferences {
    pub layout: HistoryLayout,
    pub preview: bool,
}
impl Default for HistoryPreferences {
    fn default() -> Self {
        Self {
            layout: HistoryLayout::Gallery,
            preview: true,
        }
    }
}

/// UI scale relative to the monitor's own Windows DPI scaling; chrome, text,
/// and hit targets only — never physical capture/media coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppearancePreferences {
    pub ui_scale_percent: u16,
}

impl AppearancePreferences {
    pub const VALID_SCALES: [u16; 5] = [100, 125, 150, 175, 200];

    pub fn validate(&self) -> Result<(), AppError> {
        if !Self::VALID_SCALES.contains(&self.ui_scale_percent) {
            return Err(AppError::InvalidInput(
                "UI scale must be 100, 125, 150, 175, or 200 percent",
            ));
        }
        Ok(())
    }
}

impl Default for AppearancePreferences {
    fn default() -> Self {
        AppearancePreferences {
            ui_scale_percent: 100,
        }
    }
}

/// Explicit per-user opt-in; off by default. A native startup registration is
/// created only when this is `true`, never implied by any other setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupPreference {
    pub launch_at_sign_in: bool,
}

/// Local, versioned, secret-free settings. Tokens are never fields here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub version: u32,
    /// Local optimistic-concurrency version, never a secret or export identity.
    #[serde(default)]
    pub revision: u64,
    /// Missing in older settings files: preserve the enabled default.
    #[serde(default = "notifications_default_enabled")]
    pub notifications_enabled: bool,
    #[serde(default)]
    pub history: HistoryPreferences,
    pub destinations: Destinations,
    pub archive: ArchivePolicy,
    pub filename: FilenamePreset,
    pub recording: RecordingLimits,
    #[serde(default)]
    pub shortcuts: ShortcutSettings,
    #[serde(default)]
    pub appearance: AppearancePreferences,
    #[serde(default)]
    pub startup: StartupPreference,
}

fn notifications_default_enabled() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            version: SETTINGS_VERSION,
            revision: 0,
            notifications_enabled: true,
            history: HistoryPreferences::default(),
            destinations: Destinations::default(),
            archive: ArchivePolicy::default(),
            filename: FilenamePreset::default(),
            recording: RecordingLimits::default(),
            shortcuts: ShortcutSettings::default(),
            appearance: AppearancePreferences::default(),
            startup: StartupPreference::default(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.version != SETTINGS_VERSION {
            return Err(AppError::InvalidInput("unsupported settings version"));
        }
        self.destinations.validate()?;
        self.archive.validate()?;
        self.filename.validate()?;
        self.recording.validate()?;
        self.shortcuts.validate()?;
        if self.shortcuts.bindings.iter().any(|binding| matches!(binding.destination, crate::app::destinations::DestinationSelection::Specific(id) if self.destinations.get(id).is_none())) {
            return Err(AppError::InvalidInput("shortcut references an unknown destination"));
        }
        self.appearance.validate()
    }

    /// Reads at most 1 MiB, rejects unknown fields, and validates every value.
    pub fn load(path: &Path) -> Result<Settings, AppError> {
        const STAGE: &str = "reading settings";
        let file = File::open(path).map_err(|e| AppError::io(STAGE, &e))?;
        let mut bytes = Vec::new();
        file.take(MAX_SETTINGS_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| AppError::io(STAGE, &e))?;
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            return Err(AppError::InvalidInput("settings file is larger than 1 MiB"));
        }
        let settings: Settings = serde_json::from_slice(&bytes).map_err(|_| {
            AppError::InvalidInput("settings file is malformed or has unknown fields")
        })?;
        settings.validate()?;
        Ok(settings)
    }

    /// Validates, then atomically replaces the file; a failed save keeps the old one.
    pub fn save(&self, path: &Path) -> Result<(), AppError> {
        let bytes = self.encode_persistence()?;
        save_encoded_json(&bytes, path)
    }

    /// The exact bounded representation used by save, without filesystem effects.
    pub(crate) fn encode_persistence(&self) -> Result<Vec<u8>, AppError> {
        self.validate()?;
        encode_json(self)
    }
}

pub(crate) fn save_json(value: &impl Serialize, path: &Path) -> Result<(), AppError> {
    let bytes = encode_json(value)?;
    save_encoded_json(&bytes, path)
}

fn encode_json(value: &impl Serialize) -> Result<Vec<u8>, AppError> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|_| AppError::Persistence("local settings could not be encoded"))?;
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(AppError::Persistence("local settings exceed capacity"));
    }
    Ok(bytes)
}

fn save_encoded_json(bytes: &[u8], path: &Path) -> Result<(), AppError> {
    const STAGE: &str = "saving local settings";
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .ok_or(AppError::InvalidInput("settings path has no parent folder"))?;
    fs::create_dir_all(dir).map_err(|e| AppError::io(STAGE, &e))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(PARTIAL_PREFIX)
        .tempfile_in(dir)
        .map_err(|e| AppError::io(STAGE, &e))?;
    tmp.write_all(bytes).map_err(|e| AppError::io(STAGE, &e))?;
    tmp.flush().map_err(|e| AppError::io(STAGE, &e))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| AppError::io(STAGE, &e))?;
    replace_write_through(tmp.path(), path)?;
    Ok(())
}

#[allow(unsafe_code)]
fn replace_write_through(source: &Path, destination: &Path) -> Result<(), AppError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let wide = |p: &Path| -> Result<Vec<u16>, AppError> {
        let mut v: Vec<u16> = p.as_os_str().encode_wide().collect();
        if v.contains(&0) {
            return Err(AppError::InvalidInput("local settings path is invalid"));
        }
        v.push(0);
        Ok(v)
    };
    let source = wide(source)?;
    let destination = wide(destination)?;
    // SAFETY: both NUL-terminated paths live for this synchronous call. Same
    // directory/volume, no copy fallback, reboot scheduling or elevated flags.
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(AppError::io(
            "committing local settings",
            &std::io::Error::last_os_error(),
        ));
    }
    // No fallible operation after the commit point: callers may now retire old
    // credentials. Tempfile drop only removes the no-longer-existing old name.
    Ok(())
}
