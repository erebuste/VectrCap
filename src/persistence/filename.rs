use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use uuid::Uuid;

use crate::app::error::AppError;

pub const MAX_PREFIX_SCALARS: usize = 48;
pub const MAX_TEMPLATE_SCALARS: usize = 256;
/// Bound on the expanded name, including the unique suffix and extension.
pub const MAX_NAME_UTF16: usize = 200;
/// Collision retries before giving up; each retry uses a fresh suffix.
pub(crate) const MAX_NAME_ATTEMPTS: usize = 16;
const SAMPLE_SUFFIX: &str = "00000000";
const LONGEST_FORMAT: OutputFormat = OutputFormat::WebP;

/// Windows device names, reserved with or without an extension.
const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4",
    "COM5", "COM6", "COM7", "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3",
    "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
];

/// Encoder-owned output type; the preset never chooses the extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Png,
    Jpeg,
    WebP,
    Gif,
    Mp4,
}

impl OutputFormat {
    pub fn extension(self) -> &'static str {
        match self {
            OutputFormat::Png => "png",
            OutputFormat::Jpeg => "jpg",
            OutputFormat::WebP => "webp",
            OutputFormat::Gif => "gif",
            OutputFormat::Mp4 => "mp4",
        }
    }
}

/// Saving preset with the limited `{prefix}`, `{date}`, `{time}`, `{id}` grammar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilenamePreset {
    pub prefix: String,
    pub template: String,
}

impl Default for FilenamePreset {
    fn default() -> Self {
        FilenamePreset {
            prefix: "VectrCap".into(),
            template: "{prefix}_{date}_{time}_{id}".into(),
        }
    }
}

enum Part<'a> {
    Literal(&'a str),
    Prefix,
    Date,
    Time,
    Id,
}

impl FilenamePreset {
    pub fn validate(&self) -> Result<(), AppError> {
        self.parts().map(|_| ())
    }

    /// Renders the final filename. `suffix` is the generated unique ID
    /// (lowercase ASCII alphanumerics); it is appended when `{id}` is omitted.
    pub fn render(
        &self,
        captured_at: DateTime<Local>,
        suffix: &str,
        format: OutputFormat,
    ) -> Result<String, AppError> {
        let parts = self.parts()?;
        if !(4..=32).contains(&suffix.len())
            || !suffix
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        {
            return Err(AppError::InvalidInput("unique filename suffix is invalid"));
        }
        let name = self.compose(&parts, captured_at.naive_local(), suffix, format);
        validate_file_name(&name)?;
        Ok(name)
    }

    /// Moves `tmp` into `dir` under a freshly rendered name without ever
    /// replacing an existing file, regenerating the suffix on collision.
    pub fn persist_unique(
        &self,
        tmp: NamedTempFile,
        dir: &Path,
        captured_at: DateTime<Local>,
        format: OutputFormat,
        next_suffix: &mut dyn FnMut() -> String,
    ) -> Result<(String, PathBuf), AppError> {
        self.validate()?;
        let mut names = std::iter::repeat_with(|| self.render(captured_at, &next_suffix(), format));
        persist_new(tmp, dir, &mut names)
    }

    fn parts(&self) -> Result<Vec<Part<'_>>, AppError> {
        if self.prefix.chars().count() > MAX_PREFIX_SCALARS {
            return Err(AppError::InvalidInput(
                "filename prefix is longer than 48 characters",
            ));
        }
        if self.prefix.contains(['{', '}']) {
            return Err(AppError::InvalidInput(
                "filename prefix cannot contain braces",
            ));
        }
        check_name_chars(&self.prefix)?;
        if self.template.is_empty() {
            return Err(AppError::InvalidInput("filename template is empty"));
        }
        if self.template.chars().count() > MAX_TEMPLATE_SCALARS {
            return Err(AppError::InvalidInput(
                "filename template is longer than 256 characters",
            ));
        }
        let parts = parse_template(&self.template)?;
        for part in &parts {
            if let Part::Literal(text) = part {
                check_name_chars(text)?;
            }
        }
        // Token expansions have fixed lengths and safe characters, so a sample
        // with the longest extension bounds every real render.
        let sample = NaiveDate::from_ymd_opt(2000, 1, 1)
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .unwrap_or_default();
        validate_file_name(&self.compose(&parts, sample, SAMPLE_SUFFIX, LONGEST_FORMAT))?;
        Ok(parts)
    }

    fn compose(
        &self,
        parts: &[Part<'_>],
        at: NaiveDateTime,
        suffix: &str,
        format: OutputFormat,
    ) -> String {
        let mut name = String::new();
        let mut has_id = false;
        for part in parts {
            match part {
                Part::Literal(text) => name.push_str(text),
                Part::Prefix => name.push_str(&self.prefix),
                Part::Date => {
                    let _ = write!(name, "{}", at.format("%Y-%m-%d"));
                }
                Part::Time => {
                    let _ = write!(name, "{}", at.format("%H-%M-%S"));
                }
                Part::Id => {
                    has_id = true;
                    name.push_str(suffix);
                }
            }
        }
        if !has_id {
            name.push('_');
            name.push_str(suffix);
        }
        name.push('.');
        name.push_str(format.extension());
        name
    }
}

fn parse_template(template: &str) -> Result<Vec<Part<'_>>, AppError> {
    const UNBALANCED: AppError =
        AppError::InvalidInput("filename template has an unbalanced brace");
    let mut parts = Vec::new();
    let mut rest = template;
    while let Some(i) = rest.find(['{', '}']) {
        if i > 0 {
            parts.push(Part::Literal(&rest[..i]));
        }
        rest = &rest[i..];
        if rest.starts_with('}') {
            return Err(UNBALANCED);
        }
        let end = rest.find('}').ok_or(UNBALANCED)?;
        parts.push(match &rest[..=end] {
            "{prefix}" => Part::Prefix,
            "{date}" => Part::Date,
            "{time}" => Part::Time,
            "{id}" => Part::Id,
            _ => {
                return Err(AppError::InvalidInput(
                    "filename template uses an unknown placeholder",
                ));
            }
        });
        rest = &rest[end + 1..];
    }
    if !rest.is_empty() {
        parts.push(Part::Literal(rest));
    }
    Ok(parts)
}

fn check_name_chars(text: &str) -> Result<(), AppError> {
    if text.chars().any(|c| {
        c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
    }) {
        return Err(AppError::InvalidInput(
            "filename contains a character Windows does not allow",
        ));
    }
    Ok(())
}

/// Checks a single generated filename (no directories) against Windows rules.
pub(crate) fn validate_file_name(name: &str) -> Result<(), AppError> {
    if name.is_empty() || name.encode_utf16().count() > MAX_NAME_UTF16 {
        return Err(AppError::InvalidInput(
            "filename is empty or longer than 200 UTF-16 units",
        ));
    }
    check_name_chars(name)?;
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    if name.ends_with(['.', ' ']) || stem.is_empty() || stem.ends_with(['.', ' ']) {
        return Err(AppError::InvalidInput(
            "filename must not end with a dot or space",
        ));
    }
    let device = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    if RESERVED_NAMES
        .iter()
        .any(|r| device.eq_ignore_ascii_case(r))
    {
        return Err(AppError::InvalidInput(
            "filename uses a name reserved by Windows",
        ));
    }
    Ok(())
}

/// Random 8-character lowercase hex suffix.
pub fn random_suffix() -> String {
    Uuid::new_v4().simple().to_string()[..8].to_owned()
}

/// Atomically renames `tmp` (already in `dir`) to the first candidate that does
/// not exist yet. Never overwrites; gives up after `MAX_NAME_ATTEMPTS`.
pub(crate) fn persist_new(
    mut tmp: NamedTempFile,
    dir: &Path,
    candidates: &mut dyn Iterator<Item = Result<String, AppError>>,
) -> Result<(String, PathBuf), AppError> {
    for candidate in candidates.take(MAX_NAME_ATTEMPTS) {
        let name = candidate?;
        validate_file_name(&name)?;
        let path = dir.join(&name);
        match tmp.persist_noclobber(&path) {
            Ok(_) => return Ok((name, path)),
            Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => tmp = e.file,
            Err(e) => return Err(AppError::io("saving a file", &e.error)),
        }
    }
    Err(AppError::Io {
        stage: "choosing a unique file name",
        kind: io::ErrorKind::AlreadyExists,
    })
}
