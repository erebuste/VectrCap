use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::app::destinations::Destinations;
use crate::app::error::AppError;
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

/// Local, versioned, secret-free settings. Tokens are never fields here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub version: u32,
    pub destinations: Destinations,
    pub archive: ArchivePolicy,
    pub filename: FilenamePreset,
    pub recording: RecordingLimits,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            version: SETTINGS_VERSION,
            destinations: Destinations::default(),
            archive: ArchivePolicy::default(),
            filename: FilenamePreset::default(),
            recording: RecordingLimits::default(),
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
        self.recording.validate()
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
        const STAGE: &str = "saving settings";
        self.validate()?;
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .ok_or(AppError::InvalidInput("settings path has no parent folder"))?;
        fs::create_dir_all(dir).map_err(|e| AppError::io(STAGE, &e))?;
        let mut tmp = tempfile::Builder::new()
            .prefix(PARTIAL_PREFIX)
            .tempfile_in(dir)
            .map_err(|e| AppError::io(STAGE, &e))?;
        serde_json::to_writer_pretty(&mut tmp, self)
            .map_err(|_| AppError::Persistence("settings could not be encoded"))?;
        tmp.flush().map_err(|e| AppError::io(STAGE, &e))?;
        tmp.as_file()
            .sync_all()
            .map_err(|e| AppError::io(STAGE, &e))?;
        tmp.persist(path)
            .map_err(|e| AppError::io(STAGE, &e.error))?;
        Ok(())
    }
}
