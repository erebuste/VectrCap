//! Secret-bearing import stays in one receiving process. No Debug/Serialize DTO.
use super::{
    destination_manager::{DestinationEdit, DestinationManager},
    destinations::{DestinationId, Destinations},
    error::AppError,
};
use crate::{
    persistence::{settings::ServerOrigin, winfs},
    vectr::{
        client::{UploadToken, VectrClient},
        uploader_info::UploaderInfoSupport,
    },
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;
const INVALID: AppError = AppError::InvalidInput("invalid VectrCap destination file");
pub const MAX_INSTALL_BYTES: usize = 16_384;
/// The secret DTO cannot enter a generic debug log or serde export.
/// ```compile_fail
/// fn log(dto: &vectrcap::app::destination_install::DestinationInstallV1) {
///     let _ = format!("{dto:?}");
/// }
/// ```
/// ```compile_fail
/// fn export(dto: &vectrcap::app::destination_install::DestinationInstallV1) {
///     let _ = serde_json::to_string(dto);
/// }
/// ```
pub struct DestinationInstallV1 {
    name: String,
    origin: ServerOrigin,
    token: Zeroizing<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DestinationWireV1 {
    format: String,
    version: u32,
    name: String,
    origin: ServerOrigin,
    #[serde(deserialize_with = "secret")]
    token: Zeroizing<String>,
}
fn secret<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Zeroizing<String>, D::Error> {
    String::deserialize(d).map(Zeroizing::new)
}
impl DestinationInstallV1 {
    pub fn parse(bytes: &[u8]) -> Result<Self, AppError> {
        if bytes.len() > MAX_INSTALL_BYTES {
            return Err(INVALID);
        }
        let dto: DestinationWireV1 = serde_json::from_slice(bytes).map_err(|_| INVALID)?;
        if dto.format != "vectrcap-destination" || dto.version != 1 {
            return Err(INVALID);
        }
        let mut names = Destinations::default();
        names
            .add(&dto.name, dto.origin.clone(), None)
            .map_err(|_| INVALID)?;
        let token = UploadToken::new(dto.token.to_string()).map_err(|_| INVALID)?;
        crate::platform::windows::credentials::validate_token(&token).map_err(|_| INVALID)?;
        Ok(Self {
            name: dto.name,
            origin: dto.origin,
            token: dto.token,
        })
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn origin(&self) -> &ServerOrigin {
        &self.origin
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallState {
    Preview,
    AwaitingVerifyConsent,
    Verifying,
    AwaitingInstall,
    Installing,
    Installed,
    Cancelled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    NotVerified,
    Verified,
    Unverified,
    Blocked,
}
pub enum InstallChoice {
    Add { name: String, set_default: bool },
    Replace(DestinationId),
}
struct SourceSnapshot {
    path: PathBuf,
    identity: winfs::FileIdentity,
    hash: [u8; 32],
}
pub struct DestinationInstallSession {
    dto: Option<DestinationInstallV1>,
    source: Option<SourceSnapshot>,
    display: (String, ServerOrigin),
    state: InstallState,
    verification: Verification,
    uploader_name: Option<String>,
    explicit_unverified: bool,
}
impl DestinationInstallSession {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, AppError> {
        let dto = DestinationInstallV1::parse(bytes)?;
        let display = (dto.name.clone(), dto.origin.clone());
        Ok(Self {
            dto: Some(dto),
            source: None,
            display,
            state: InstallState::AwaitingVerifyConsent,
            verification: Verification::NotVerified,
            uploader_name: None,
            explicit_unverified: false,
        })
    }
    pub fn read(path: &Path) -> Result<Self, AppError> {
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("vectrcap"))
        {
            return Err(INVALID);
        }
        let mut file = open_source(path, false)?;
        let identity = winfs::identity(&file).map_err(|_| INVALID)?;
        if file.metadata().map_err(|_| INVALID)?.len() > MAX_INSTALL_BYTES as u64 {
            return Err(INVALID);
        }
        let mut bytes = Zeroizing::new(Vec::new());
        file.by_ref()
            .take(MAX_INSTALL_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| INVALID)?;
        let mut session = Self::from_bytes(&bytes)?;
        session.source = Some(SourceSnapshot {
            path: path.into(),
            identity,
            hash: Sha256::digest(&bytes).into(),
        });
        Ok(session)
    }
    pub fn display(&self) -> (&str, &ServerOrigin) {
        (&self.display.0, &self.display.1)
    }
    pub fn install_allowed(&self) -> bool {
        self.state == InstallState::AwaitingInstall
            && (self.verification == Verification::Verified
                || self.verification == Verification::Unverified && self.explicit_unverified)
    }
    pub fn state(&self) -> InstallState {
        self.state
    }
    pub fn verification(&self) -> Verification {
        self.verification
    }
    pub fn uploader_name(&self) -> Option<&str> {
        self.uploader_name.as_deref()
    }
    pub fn preview(&self) -> Option<(&str, &ServerOrigin)> {
        self.dto.as_ref().map(|d| (d.name(), d.origin()))
    }
    pub fn consent(&mut self) -> Result<(), AppError> {
        if self.state != InstallState::AwaitingVerifyConsent
            && !(self.state == InstallState::AwaitingInstall
                && self.verification == Verification::Unverified)
        {
            return Err(INVALID);
        }
        self.verification = Verification::NotVerified;
        self.uploader_name = None;
        self.explicit_unverified = false;
        self.state = InstallState::Verifying;
        Ok(())
    }
    pub fn verification_origin(&self) -> Result<&ServerOrigin, AppError> {
        if self.state != InstallState::Verifying {
            return Err(INVALID);
        }
        Ok(&self.dto.as_ref().ok_or(INVALID)?.origin)
    }
    pub fn verification_client(&self) -> Result<VectrClient, AppError> {
        let origin = self.verification_origin()?.clone();
        let d = self.dto.as_ref().ok_or(INVALID)?;
        VectrClient::new(origin, UploadToken::new(d.token.to_string())?)
    }
    pub fn verification_result(
        &mut self,
        result: Result<UploaderInfoSupport, AppError>,
    ) -> Result<(), AppError> {
        if self.state != InstallState::Verifying {
            return Err(INVALID);
        }
        match result {
            Ok(UploaderInfoSupport::Available(info)) => {
                self.uploader_name = Some(info.name);
                self.verification = Verification::Verified;
                self.state = InstallState::AwaitingInstall;
                Ok(())
            }
            Ok(UploaderInfoSupport::Unsupported404) | Err(AppError::Network | AppError::Tls) => {
                self.verification = Verification::Unverified;
                self.state = InstallState::AwaitingInstall;
                Ok(())
            }
            Err(error) => {
                self.verification = Verification::Blocked;
                self.state = InstallState::AwaitingVerifyConsent;
                Err(error)
            }
        }
    }
    pub fn allow_unverified(&mut self) -> Result<(), AppError> {
        if self.state != InstallState::AwaitingInstall
            || self.verification != Verification::Unverified
        {
            return Err(INVALID);
        }
        self.explicit_unverified = true;
        Ok(())
    }
    pub fn install(
        &mut self,
        manager: &mut DestinationManager,
        choice: InstallChoice,
    ) -> Result<DestinationId, AppError> {
        if self.state != InstallState::AwaitingInstall
            || !(self.verification == Verification::Verified
                || self.verification == Verification::Unverified && self.explicit_unverified)
        {
            return Err(INVALID);
        }
        // Prepare locally; no optimistic installed state before credential/settings commit.
        let d = self.dto.as_ref().ok_or(INVALID)?;
        let mut destinations = manager.settings().destinations.clone();
        let id = match choice {
            InstallChoice::Add { name, set_default } => {
                let id = destinations.add(&name, d.origin.clone(), None)?;
                if set_default {
                    destinations.set_default(id)?;
                }
                id
            }
            InstallChoice::Replace(id) => {
                let old = destinations.get(id).ok_or(INVALID)?;
                if old.origin != d.origin
                    || old.name.trim().to_lowercase() != d.name.trim().to_lowercase()
                {
                    return Err(AppError::InvalidInput(
                        "replacement requires the same name and origin",
                    ));
                }
                id
            }
        };
        let token = UploadToken::new(d.token.to_string())?;
        let revision = manager.settings().revision;
        self.state = InstallState::Installing;
        match manager.apply(
            DestinationEdit {
                destinations,
                tokens: vec![(id, token)],
            },
            revision,
        ) {
            Ok(_) => {
                self.dto = None;
                self.state = InstallState::Installed;
                Ok(id)
            }
            Err(error) => {
                self.state = InstallState::AwaitingInstall;
                Err(error)
            }
        }
    }
    pub fn cancel(&mut self) {
        if self.state != InstallState::Installed {
            self.dto = None;
            self.uploader_name = None;
            self.state = InstallState::Cancelled;
        }
    }
    pub fn can_delete_source(&self) -> bool {
        self.state == InstallState::Installed && self.source.is_some()
    }
    pub fn delete_source(&mut self) -> Result<(), AppError> {
        if !self.can_delete_source() {
            return Err(INVALID);
        }
        let source = self.source.as_ref().ok_or(INVALID)?;
        let mut file = open_source(&source.path, true)?;
        if !source
            .identity
            .same_object(winfs::identity(&file).map_err(|_| INVALID)?)
        {
            return Err(AppError::ArtifactChanged);
        }
        let mut bytes = Zeroizing::new(Vec::new());
        file.by_ref()
            .take(MAX_INSTALL_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| INVALID)?;
        if bytes.len() > MAX_INSTALL_BYTES
            || <[u8; 32]>::from(Sha256::digest(&bytes)) != source.hash
        {
            return Err(AppError::ArtifactChanged);
        }
        winfs::delete_by_handle(&file)
            .map_err(|e| AppError::io("deleting downloaded config", &e))?;
        self.source = None;
        Ok(())
    }
}
/// Walk each directory relative to its parent handle; never follow a reparse
/// component, including when a parent is exchanged during the import/delete.
fn open_source(path: &Path, delete: bool) -> Result<File, AppError> {
    super::cli::upload_path(path.as_os_str())?;
    winfs::open_local_source(path, delete).map_err(|_| INVALID)
}
