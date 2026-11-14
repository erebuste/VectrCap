use std::fmt;
use std::path::PathBuf;

use uuid::Uuid;

use crate::app::error::AppError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JobId(pub Uuid);

impl JobId {
    pub fn new_random() -> Self {
        JobId(Uuid::new_v4())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArtifactId(pub Uuid);

impl ArtifactId {
    pub fn new_random() -> Self {
        ArtifactId(Uuid::new_v4())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArtifactKind {
    Screenshot,
    Gif,
    Mp4,
    /// An existing user-selected file; never archived or deleted by the app.
    File,
}

/// An immutable app-managed copy of bytes to upload. `path` is a generated
/// spool path; `display_name` is the name shown to the user and sent upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub id: ArtifactId,
    pub path: PathBuf,
    pub display_name: String,
    pub kind: ArtifactKind,
    pub size: u64,
    pub sha256: [u8; 32],
}

const MAX_RECEIPT_URL_BYTES: usize = 2048;
const MAX_RECEIPT_NAME_SCALARS: usize = 255;

/// A confirmed Vectr result. Debug output redacts the sharing URL.
#[derive(Clone, PartialEq, Eq)]
pub struct UploadReceipt {
    url: String,
    name: String,
}

impl UploadReceipt {
    /// Validates an untrusted server receipt: HTTPS link without credentials,
    /// bounded name without control characters.
    pub fn new(url: &str, name: &str) -> Result<Self, AppError> {
        if url.is_empty()
            || url.len() > MAX_RECEIPT_URL_BYTES
            || url.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(AppError::InvalidInput(
                "receipt link is empty, too long, or contains whitespace",
            ));
        }
        let parsed = url::Url::parse(url)
            .map_err(|_| AppError::InvalidInput("receipt link is malformed"))?;
        if parsed.scheme() != "https"
            || parsed.host().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(AppError::InvalidInput(
                "receipt link must be HTTPS without credentials",
            ));
        }
        if name.is_empty()
            || name.chars().count() > MAX_RECEIPT_NAME_SCALARS
            || name.chars().any(char::is_control)
        {
            return Err(AppError::InvalidInput(
                "receipt name is empty, too long, or contains control characters",
            ));
        }
        Ok(UploadReceipt {
            url: url.to_owned(),
            name: name.to_owned(),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Debug for UploadReceipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UploadReceipt")
            .field("url", &"<redacted>")
            // A malicious server can echo credentials/links in its name field.
            .field("name", &"<redacted>")
            .finish()
    }
}
