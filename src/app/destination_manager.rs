//! Serialized destination/settings transactions. The controller must serialize
//! workflow/job acceptance and reference repairs with apply/cleanup, including
//! the interval between reference inspection and durable commit. FileRepository
//! additionally excludes other destination managers for this settings path.
use crate::{
    app::{
        destinations::{CredentialKey, DestinationId, DestinationSelection, Destinations},
        error::AppError,
        shortcuts::ShortcutBinding,
    },
    persistence::{
        history::History,
        settings::{AppearancePreferences, Settings, StartupPreference},
    },
    platform::{CredentialStore, windows::credentials::validate_token},
    vectr::client::UploadToken,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::Read,
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Candidate destination preferences use the existing validated model. Existing
/// credential refs/generations cannot be edited by callers. Supply tokens only
/// in this short-lived, non-serializable operation, never inside Settings.
pub struct DestinationEdit {
    pub destinations: Destinations,
    pub tokens: Vec<(DestinationId, UploadToken)>,
}

/// Required injection: no default implementation silently ignores workflows or
/// future hotkey/profile references. Implementations must fail closed on errors.
pub trait DestinationReferences: Send + Sync {
    fn active_workflows(&self) -> Result<HashSet<DestinationId>, AppError>;
    /// Validate every configured hotkey/profile ID against the candidate, after
    /// explicit repairs. Future controllers must include their candidate refs.
    fn validate_configured(&self, candidate: &Settings) -> Result<(), AppError>;
    /// Record which destinations the current shortcut bindings reference, so
    /// a later `validate_configured` call (from a destination edit) blocks
    /// removing one a shortcut still points at by stable ID. Called only
    /// after a shortcut save has already durably committed.
    fn set_configured_shortcuts(&self, bindings: &[ShortcutBinding]) -> Result<(), AppError>;
}

/// Nonsecret exact-key ownership journal, committed BEFORE credential writes.
/// High-water marks survive cleanup so abandoned generations are never reused.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialJournal {
    pub entries: Vec<CredentialKey>,
    pub generations: HashMap<DestinationId, u32>,
}
impl CredentialJournal {
    fn validate(&self) -> Result<(), AppError> {
        let mut keys = HashSet::new();
        for key in &self.entries {
            if key.destination().0.is_nil()
                || key.generation() == 0
                || !keys.insert(key)
                || self
                    .generations
                    .get(&key.destination())
                    .copied()
                    .unwrap_or(0)
                    < key.generation()
            {
                return Err(AppError::Persistence(
                    "credential journal binding is invalid",
                ));
            }
        }
        if self
            .generations
            .iter()
            .any(|(id, g)| id.0.is_nil() || *g == 0)
        {
            return Err(AppError::Persistence(
                "credential journal generation is invalid",
            ));
        }
        Ok(())
    }
    fn register(&mut self, key: &CredentialKey) {
        self.generations
            .entry(key.destination())
            .and_modify(|g| *g = (*g).max(key.generation()))
            .or_insert(key.generation());
        if !self.entries.contains(key) {
            self.entries.push(key.clone());
        }
    }
}

/// Implementations own the settings lease. A save error MUST mean no commit;
/// do not report a successful replacement as an error in postcommit bookkeeping.
pub trait DestinationRepository {
    fn load_settings(&self) -> Result<Settings, AppError>;
    fn save_settings(&self, settings: &Settings) -> Result<(), AppError>;
    fn load_journal(&self) -> Result<CredentialJournal, AppError>;
    fn save_journal(&self, journal: &CredentialJournal) -> Result<(), AppError>;
}

pub struct FileDestinationRepository {
    path: PathBuf,
    journal: PathBuf,
    _lease: File,
}
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}
impl FileDestinationRepository {
    pub fn open(path: &Path) -> Result<Self, AppError> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or(AppError::InvalidInput("settings path requires a parent"))?;
        fs::create_dir_all(parent).map_err(|e| AppError::io("opening destination settings", &e))?;
        let path = fs::canonicalize(parent)
            .map_err(|e| AppError::io("opening destination settings", &e))?
            .join(
                path.file_name()
                    .ok_or(AppError::InvalidInput("settings filename missing"))?,
            );
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(sidecar(&path, ".destination-lock"))
            .map_err(|_| AppError::InUse("destination settings already open or unavailable"))?;
        match Settings::load(&path) {
            Ok(_) => {}
            Err(AppError::Io {
                kind: std::io::ErrorKind::NotFound,
                ..
            }) => Settings::default().save(&path)?,
            Err(e) => return Err(e),
        }
        Ok(Self {
            journal: sidecar(&path, ".credential-journal.json"),
            path,
            _lease: lease,
        })
    }
}
impl DestinationRepository for FileDestinationRepository {
    fn load_settings(&self) -> Result<Settings, AppError> {
        Settings::load(&self.path)
    }
    fn save_settings(&self, settings: &Settings) -> Result<(), AppError> {
        settings.save(&self.path)
    }
    fn load_journal(&self) -> Result<CredentialJournal, AppError> {
        let file = match File::open(&self.journal) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CredentialJournal::default());
            }
            Err(e) => return Err(AppError::io("reading credential journal", &e)),
        };
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| AppError::io("reading credential journal", &e))?;
        if bytes.len() > 1024 * 1024 {
            return Err(AppError::Persistence("credential journal exceeds capacity"));
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| AppError::Persistence("credential journal is invalid"))
    }
    fn save_journal(&self, journal: &CredentialJournal) -> Result<(), AppError> {
        crate::persistence::settings::save_json(journal, &self.journal)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub revision: u64,
    pub cleanup_pending: bool,
}

pub struct DestinationManager {
    repository: Box<dyn DestinationRepository>,
    store: Arc<dyn CredentialStore>,
    history: Arc<History>,
    references: Arc<dyn DestinationReferences>,
    settings: Settings,
    journal: CredentialJournal,
}
impl DestinationManager {
    pub fn open(
        repository: Box<dyn DestinationRepository>,
        store: Arc<dyn CredentialStore>,
        history: Arc<History>,
        references: Arc<dyn DestinationReferences>,
    ) -> Result<Self, AppError> {
        let settings = repository.load_settings()?;
        settings.validate()?;
        references.validate_configured(&settings)?;
        let journal = repository.load_journal()?;
        journal.validate()?;
        let mut manager = Self {
            repository,
            store,
            history,
            references,
            settings,
            journal,
        };
        manager.cleanup()?;
        Ok(manager)
    }
    pub fn settings(&self) -> &Settings {
        &self.settings
    }
    /// Commit the preference before publishing it to the controller or UI.
    pub fn set_notifications_enabled(&mut self, enabled: bool) -> Result<(), AppError> {
        self.ensure_not_corrupted()?;
        if self.settings.notifications_enabled == enabled {
            return Ok(());
        }
        let mut candidate = self.settings.clone();
        candidate.notifications_enabled = enabled;
        candidate.revision = candidate
            .revision
            .checked_add(1)
            .ok_or(AppError::Persistence("settings revisions are exhausted"))?;
        candidate.validate()?;
        self.repository.save_settings(&candidate)?;
        self.settings = candidate;
        Ok(())
    }
    pub fn set_history_preferences(
        &mut self,
        preferences: crate::persistence::settings::HistoryPreferences,
    ) -> Result<(), AppError> {
        self.ensure_not_corrupted()?;
        if self.settings.history == preferences {
            return Ok(());
        }
        let mut candidate = self.settings.clone();
        candidate.history = preferences;
        candidate.revision = candidate
            .revision
            .checked_add(1)
            .ok_or(AppError::Persistence("settings revisions are exhausted"))?;
        candidate.validate()?;
        self.repository.save_settings(&candidate)?;
        self.settings = candidate;
        Ok(())
    }
    pub fn set_appearance(&mut self, appearance: AppearancePreferences) -> Result<(), AppError> {
        self.ensure_not_corrupted()?;
        if self.settings.appearance == appearance {
            return Ok(());
        }
        let mut candidate = self.settings.clone();
        candidate.appearance = appearance;
        candidate.revision = candidate
            .revision
            .checked_add(1)
            .ok_or(AppError::Persistence("settings revisions are exhausted"))?;
        candidate.validate()?;
        self.repository.save_settings(&candidate)?;
        self.settings = candidate;
        Ok(())
    }
    pub fn set_archive_policy(
        &mut self,
        policy: crate::persistence::settings::ArchivePolicy,
    ) -> Result<(), AppError> {
        self.ensure_not_corrupted()?;
        if self.settings.archive == policy {
            return Ok(());
        }
        let mut candidate = self.settings.clone();
        candidate.archive = policy;
        candidate.revision = candidate
            .revision
            .checked_add(1)
            .ok_or(AppError::Persistence("settings revisions are exhausted"))?;
        candidate.validate()?;
        self.repository.save_settings(&candidate)?;
        self.settings = candidate;
        Ok(())
    }
    pub fn set_startup(&mut self, startup: StartupPreference) -> Result<(), AppError> {
        self.ensure_not_corrupted()?;
        if self.settings.startup == startup {
            return Ok(());
        }
        let mut candidate = self.settings.clone();
        candidate.startup = startup;
        candidate.revision = candidate
            .revision
            .checked_add(1)
            .ok_or(AppError::Persistence("settings revisions are exhausted"))?;
        candidate.validate()?;
        self.repository.save_settings(&candidate)?;
        self.settings = candidate;
        Ok(())
    }
    /// Validates the candidate's own destination references against the
    /// CURRENT destination set, persists it as a new revision, then records
    /// those references so a later destination removal cannot silently
    /// orphan them. Native registration itself is the caller's job (see
    /// `ShortcutManager::apply`); this method is exactly the "configuration
    /// writer" that transaction is injected with.
    pub fn save_shortcuts(&mut self, candidate: &[ShortcutBinding]) -> Result<(), AppError> {
        self.ensure_not_corrupted()?;
        for binding in candidate {
            if let DestinationSelection::Specific(id) = binding.destination
                && self.settings.destinations.get(id).is_none()
            {
                return Err(AppError::InvalidInput(
                    "shortcut destination no longer exists",
                ));
            }
        }
        let mut settings = self.settings.clone();
        settings.shortcuts.bindings = candidate.to_vec();
        settings.revision = settings
            .revision
            .checked_add(1)
            .ok_or(AppError::Persistence("settings revisions are exhausted"))?;
        settings.validate()?;
        self.repository.save_settings(&settings)?;
        self.settings = settings;

        Ok(())
    }
    /// Cooperating acceptance and edits share this leased revision boundary.
    pub fn ensure_revision(&self, expected_revision: u64) -> Result<(), AppError> {
        if expected_revision != self.settings.revision {
            return Err(AppError::InvalidInput(
                "settings changed; refresh before applying",
            ));
        }
        self.ensure_not_corrupted()
    }
    /// Detects settings.json being modified out-of-band since it was loaded.
    /// Workflow admission/acceptance pin their destination snapshot at
    /// BeginWorkflow time by design (same as never-reused credential
    /// generations) and intentionally skip the caller-revision half of
    /// `ensure_revision`, which only has meaning for destination edits.
    pub fn ensure_not_corrupted(&self) -> Result<(), AppError> {
        if self.repository.load_settings()? != self.settings {
            return Err(AppError::InvalidInput(
                "settings changed; refresh before applying",
            ));
        }
        Ok(())
    }
    pub fn apply(
        &mut self,
        edit: DestinationEdit,
        expected_revision: u64,
    ) -> Result<ApplyOutcome, AppError> {
        // Check BEFORE journal/store writes or any setup side effect.
        self.ensure_revision(expected_revision)?;
        edit.destinations.validate()?;
        let mut tokens = HashSet::new();
        for (id, token) in &edit.tokens {
            validate_token(token)?;
            if !tokens.insert(*id) || edit.destinations.get(*id).is_none() {
                return Err(AppError::InvalidInput(
                    "token destination is missing or duplicated",
                ));
            }
        }
        let active = self.active_ids()?;
        for old in self.settings.destinations.iter() {
            let next = edit.destinations.get(old.id);
            let identity_change =
                next.is_none_or(|next| next.origin != old.origin) || tokens.contains(&old.id);
            let configured_shortcut = self
                .settings
                .shortcuts
                .bindings
                .iter()
                .any(|binding| binding.destination == DestinationSelection::Specific(old.id));
            if identity_change && configured_shortcut {
                return Err(AppError::InUse(
                    "clear the shortcut destination assignment before changing or removing its connection",
                ));
            }
            if identity_change && active.contains(&old.id) {
                return Err(AppError::InUse(
                    "destination is referenced by unfinished uploads or workflows",
                ));
            }
        }
        for next in edit.destinations.iter() {
            if self.settings.destinations.get(next.id).is_none() && active.contains(&next.id) {
                return Err(AppError::InUse(
                    "destination identifier is referenced by unfinished work",
                ));
            }
            match self.settings.destinations.get(next.id) {
                Some(old) => {
                    let changed = next.origin != old.origin;
                    if next.credential_generation != old.credential_generation
                        || (if changed {
                            next.credential_ref.is_some()
                        } else {
                            next.credential_ref != old.credential_ref
                        })
                    {
                        return Err(AppError::InvalidInput(
                            "credential bindings are managed internally",
                        ));
                    }
                    if changed && !tokens.contains(&next.id) {
                        return Err(AppError::NeedsSetup(
                            "a changed origin requires an explicitly supplied token",
                        ));
                    }
                }
                None if next.credential_ref.is_some() || next.credential_generation != 0 => {
                    return Err(AppError::InvalidInput(
                        "new destinations must not supply credential references",
                    ));
                }
                None => {}
            }
        }
        let mut candidate = self.settings.clone();
        candidate.destinations = edit.destinations;
        candidate.revision = candidate
            .revision
            .checked_add(1)
            .ok_or(AppError::Persistence("settings revisions are exhausted"))?;
        self.references.validate_configured(&candidate)?;
        let mut journal = self.journal.clone();
        for d in self.settings.destinations.iter() {
            if d.credential_generation > 0 {
                journal
                    .generations
                    .entry(d.id)
                    .and_modify(|g| *g = (*g).max(d.credential_generation))
                    .or_insert(d.credential_generation);
            }
            if let Some(key) = &d.credential_ref {
                journal.register(key);
            }
        }
        let mut staged = Vec::new();
        for (id, token) in edit.tokens {
            let generation = journal
                .generations
                .get(&id)
                .copied()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or(AppError::Credential("credential generations are exhausted"))?;
            let key = candidate
                .destinations
                .bind_reserved_credential(id, generation)?;
            // Never adopt or overwrite an entry unknown to this transaction.
            match self.store.read(&key) {
                Err(AppError::NeedsSetup(_)) => {}
                Ok(_) => {
                    return Err(AppError::Credential(
                        "reserved credential generation already exists",
                    ));
                }
                Err(e) => return Err(e),
            }
            journal.register(&key);
            staged.push((key, token));
        }
        // Preflight the same bounded bytes used by Settings::save before any
        // durable generation reservation or secret write. Runtime I/O failures
        // still use the existing rollback path.
        candidate.encode_persistence()?;
        journal.validate()?;
        self.repository.save_journal(&journal)?;
        self.journal = journal;
        let mut written = Vec::new();
        for (key, token) in &staged {
            if let Err(e) = self.store.write(key, token) {
                self.rollback_staged(&written);
                return Err(e);
            }
            written.push(key.clone());
        }
        if let Err(e) = self.repository.save_settings(&candidate) {
            self.rollback_staged(&written);
            return Err(e);
        }
        // Durable commit is the point of no rollback. Cleanup errors cannot
        // turn successful setup into failure or delete the new working binding.
        self.settings = candidate;
        let cleanup_pending = self.cleanup().unwrap_or(true);
        Ok(ApplyOutcome {
            revision: self.settings.revision,
            cleanup_pending,
        })
    }
    fn active_ids(&self) -> Result<HashSet<DestinationId>, AppError> {
        let mut active = self.history.active_destination_ids()?;
        active.extend(self.references.active_workflows()?);
        Ok(active)
    }
    fn rollback_staged(&self, written: &[CredentialKey]) {
        // A precommit failure touches ONLY successful writes by this attempt.
        // Every key remains journaled even if deletion fails or power is lost.
        for key in written {
            let _ = self.store.delete(key);
        }
    }
    /// Returns whether unreferenced cleanup is still pending. A currently
    /// referenced binding is normal journal ownership, not an orphan.
    pub fn cleanup(&mut self) -> Result<bool, AppError> {
        if self.repository.load_settings()? != self.settings {
            return Err(AppError::InvalidInput(
                "settings changed; reopen before cleanup",
            ));
        }
        let active = self.active_ids()?;
        let live: HashSet<_> = self
            .settings
            .destinations
            .iter()
            .filter_map(|d| d.credential_ref.as_ref())
            .collect();
        let mut journal = self.journal.clone();
        let mut retained = Vec::new();
        let mut pending = false;
        for key in &journal.entries {
            if live.contains(key) {
                retained.push(key.clone());
                continue;
            }
            // Conservative protection covers stale workflow/job generations too.
            if active.contains(&key.destination()) || self.store.delete(key).is_err() {
                pending = true;
                retained.push(key.clone());
            }
        }
        if retained != journal.entries {
            journal.entries = retained;
            self.repository.save_journal(&journal)?;
            self.journal = journal;
        }
        Ok(pending)
    }
}
