use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app::error::AppError;
use crate::persistence::settings::{LinkOrigin, ServerOrigin};

pub const MAX_DESTINATIONS: usize = 32;
pub const MAX_NAME_SCALARS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DestinationId(pub Uuid);

impl DestinationId {
    pub fn new_random() -> Self {
        DestinationId(Uuid::new_v4())
    }
}

/// Non-secret reference to one Credential Manager entry: destination, the
/// origin the token is bound to, and a never-reused generation number.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialKey {
    destination: DestinationId,
    origin: ServerOrigin,
    generation: u32,
}

impl CredentialKey {
    pub(crate) fn new(destination: DestinationId, origin: ServerOrigin, generation: u32) -> Self {
        CredentialKey {
            destination,
            origin,
            generation,
        }
    }

    pub fn destination(&self) -> DestinationId {
        self.destination
    }

    pub fn origin(&self) -> &ServerOrigin {
        &self.origin
    }

    pub fn generation(&self) -> u32 {
        self.generation
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    pub id: DestinationId,
    pub name: String,
    pub origin: ServerOrigin,
    pub credential_ref: Option<CredentialKey>,
    pub link_domain_override: Option<LinkOrigin>,
    /// Last credential generation issued for this destination; never reused.
    pub credential_generation: u32,
}

/// Validated destination list with exactly one default whenever it is nonempty.
/// Deserialization runs the same validation, so persisted data cannot bypass it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "DestinationsRepr")]
pub struct Destinations {
    items: Vec<Destination>,
    default: Option<DestinationId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DestinationsRepr {
    items: Vec<Destination>,
    default: Option<DestinationId>,
}

impl TryFrom<DestinationsRepr> for Destinations {
    type Error = AppError;

    fn try_from(repr: DestinationsRepr) -> Result<Self, AppError> {
        let d = Destinations {
            items: repr.items,
            default: repr.default,
        };
        d.validate()?;
        Ok(d)
    }
}

const UNKNOWN: AppError = AppError::InvalidInput("unknown destination");

impl Destinations {
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Destination> {
        self.items.iter()
    }

    pub fn get(&self, id: DestinationId) -> Option<&Destination> {
        self.items.iter().find(|d| d.id == id)
    }

    pub fn default_id(&self) -> Option<DestinationId> {
        self.default
    }

    /// Adds a tokenless destination. The first one becomes the default; later
    /// additions never change an existing default.
    pub fn add(
        &mut self,
        name: &str,
        origin: ServerOrigin,
        link_domain: Option<LinkOrigin>,
    ) -> Result<DestinationId, AppError> {
        if self.items.len() >= MAX_DESTINATIONS {
            return Err(AppError::InvalidInput(
                "at most 32 destinations can be configured",
            ));
        }
        let name = self.unique_name(name, None)?;
        let id = DestinationId::new_random();
        self.items.push(Destination {
            id,
            name,
            origin,
            credential_ref: None,
            link_domain_override: link_domain,
            credential_generation: 0,
        });
        self.default.get_or_insert(id);
        Ok(id)
    }

    /// Future-only label change; accepted jobs keep their snapshotted label.
    pub fn rename(&mut self, id: DestinationId, name: &str) -> Result<(), AppError> {
        let name = self.unique_name(name, Some(id))?;
        self.get_mut(id)?.name = name;
        Ok(())
    }

    /// Future-only domain change; accepted jobs keep their snapshotted domain.
    pub fn set_link_domain(
        &mut self,
        id: DestinationId,
        domain: Option<LinkOrigin>,
    ) -> Result<(), AppError> {
        self.get_mut(id)?.link_domain_override = domain;
        Ok(())
    }

    pub fn set_default(&mut self, id: DestinationId) -> Result<(), AppError> {
        self.get_mut(id)?;
        self.default = Some(id);
        Ok(())
    }

    /// Issues a fresh credential reference bound to the current origin (token
    /// replacement). The caller stores the token under the returned key.
    pub fn bind_credential(
        &mut self,
        id: DestinationId,
        active: &HashSet<DestinationId>,
    ) -> Result<CredentialKey, AppError> {
        ensure_inactive(id, active)?;
        let d = self.get_mut(id)?;
        let generation = d
            .credential_generation
            .checked_add(1)
            .ok_or(AppError::InvalidInput(
                "credential generations are exhausted",
            ))?;
        let key = CredentialKey::new(id, d.origin.clone(), generation);
        d.credential_generation = generation;
        d.credential_ref = Some(key.clone());
        Ok(key)
    }

    /// Transaction coordinator reserves generations durably, including failed
    /// installations, before issuing a binding. Never lower or reuse one.
    pub(crate) fn bind_reserved_credential(
        &mut self,
        id: DestinationId,
        generation: u32,
    ) -> Result<CredentialKey, AppError> {
        let d = self.get_mut(id)?;
        if generation <= d.credential_generation {
            return Err(AppError::InvalidInput("credential generation is not fresh"));
        }
        let key = CredentialKey::new(id, d.origin.clone(), generation);
        d.credential_generation = generation;
        d.credential_ref = Some(key.clone());
        Ok(key)
    }

    /// Changes the server origin. The old token binding is dropped, never carried
    /// to the new origin.
    pub fn change_origin(
        &mut self,
        id: DestinationId,
        origin: ServerOrigin,
        active: &HashSet<DestinationId>,
    ) -> Result<(), AppError> {
        ensure_inactive(id, active)?;
        let d = self.get_mut(id)?;
        if d.origin != origin {
            d.origin = origin;
            d.credential_ref = None;
        }
        Ok(())
    }

    /// Removes a destination after explicit reference repair. Removing the
    /// default requires naming another configured destination as the new default
    /// unless it is the last one. Returns the removed entry so the caller can
    /// retire only its own credential.
    pub fn remove(
        &mut self,
        id: DestinationId,
        new_default: Option<DestinationId>,
        active: &HashSet<DestinationId>,
    ) -> Result<Destination, AppError> {
        let index = self.items.iter().position(|d| d.id == id).ok_or(UNKNOWN)?;
        ensure_inactive(id, active)?;
        if let Some(next) = new_default
            && (next == id || self.get(next).is_none())
        {
            return Err(AppError::InvalidInput(
                "the new default must be another configured destination",
            ));
        }
        if self.default == Some(id) && new_default.is_none() && self.items.len() > 1 {
            return Err(AppError::InvalidInput(
                "choose a new default before removing the default destination",
            ));
        }
        let removed = self.items.remove(index);
        if new_default.is_some() || self.default == Some(id) {
            self.default = new_default;
        }
        Ok(removed)
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.items.len() > MAX_DESTINATIONS {
            return Err(AppError::InvalidInput(
                "at most 32 destinations can be configured",
            ));
        }
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for d in &self.items {
            if d.id.0.is_nil() {
                return Err(AppError::InvalidInput(
                    "destination identifier must be nonzero",
                ));
            }
            if !ids.insert(d.id) {
                return Err(AppError::InvalidInput(
                    "destination identifiers must be unique",
                ));
            }
            if d.name != d.name.trim() {
                return Err(AppError::InvalidInput(
                    "destination names must not have surrounding whitespace",
                ));
            }
            check_name(&d.name)?;
            if !names.insert(normalized(&d.name)) {
                return Err(AppError::InvalidInput("destination names must be unique"));
            }
            if let Some(key) = &d.credential_ref
                && (key.destination != d.id
                    || key.origin != d.origin
                    || key.generation == 0
                    || key.generation != d.credential_generation)
            {
                return Err(AppError::InvalidInput(
                    "credential reference does not match its destination",
                ));
            }
        }
        match self.default {
            None if self.items.is_empty() => Ok(()),
            Some(id) if ids.contains(&id) => Ok(()),
            _ => Err(AppError::InvalidInput(
                "exactly one configured destination must be the default",
            )),
        }
    }

    fn get_mut(&mut self, id: DestinationId) -> Result<&mut Destination, AppError> {
        self.items.iter_mut().find(|d| d.id == id).ok_or(UNKNOWN)
    }

    fn unique_name(&self, name: &str, except: Option<DestinationId>) -> Result<String, AppError> {
        let name = name.trim();
        check_name(name)?;
        let key = normalized(name);
        if self
            .items
            .iter()
            .any(|d| Some(d.id) != except && normalized(&d.name) == key)
        {
            return Err(AppError::InvalidInput("destination names must be unique"));
        }
        Ok(name.to_owned())
    }
}

fn check_name(name: &str) -> Result<(), AppError> {
    if name.is_empty() {
        return Err(AppError::InvalidInput("destination name is empty"));
    }
    if name.chars().count() > MAX_NAME_SCALARS {
        return Err(AppError::InvalidInput(
            "destination name is longer than 100 characters",
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(AppError::InvalidInput(
            "destination name contains control characters",
        ));
    }
    Ok(())
}

fn normalized(name: &str) -> String {
    name.trim().to_lowercase()
}

fn ensure_inactive(id: DestinationId, active: &HashSet<DestinationId>) -> Result<(), AppError> {
    if active.contains(&id) {
        return Err(AppError::InUse(
            "destination is referenced by unfinished uploads or workflows",
        ));
    }
    Ok(())
}

/// Immutable copy of a destination taken when work is accepted. It never
/// contains a token; `credential` only names which entry to read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationSnapshot {
    pub id: DestinationId,
    pub label: String,
    pub origin: ServerOrigin,
    pub credential: CredentialKey,
    pub link_domain: Option<LinkOrigin>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DestinationSelection {
    #[default]
    Inherit,
    Specific(DestinationId),
}

/// Resolves the destination once, at workflow/batch acceptance: explicit
/// effective task profile, then explicit hotkey, then the default. A missing or
/// tokenless choice is `NeedsSetup`; it never falls back to another destination.
pub fn resolve_destination(
    effective_task: DestinationSelection,
    hotkey: DestinationSelection,
    destinations: &Destinations,
) -> Result<DestinationSnapshot, AppError> {
    use DestinationSelection::{Inherit, Specific};
    let id = match (effective_task, hotkey) {
        (Specific(id), _) | (Inherit, Specific(id)) => id,
        (Inherit, Inherit) => destinations
            .default
            .ok_or(AppError::NeedsSetup("no default destination is configured"))?,
    };
    let d = destinations.get(id).ok_or(AppError::NeedsSetup(
        "the selected destination no longer exists",
    ))?;
    let credential = d.credential_ref.clone().ok_or(AppError::NeedsSetup(
        "the selected destination has no upload token",
    ))?;
    Ok(DestinationSnapshot {
        id,
        label: d.name.clone(),
        origin: d.origin.clone(),
        credential,
        link_domain: d.link_domain_override.clone(),
    })
}
