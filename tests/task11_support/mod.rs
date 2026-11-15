#![allow(dead_code)]
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex},
};
use vectrcap::{
    app::{destination_manager::*, destinations::*, error::AppError},
    persistence::{history::History, settings::Settings},
    platform::{CredentialStore, windows::explorer::*},
    vectr::client::UploadToken,
};
#[derive(Default)]
pub struct Registry {
    pub trees: RefCell<BTreeMap<String, RegistryTree>>,
    pub fail: RefCell<bool>,
}
impl RegistryBackend for Registry {
    fn read_tree(&self, key: &str) -> Result<Option<RegistryTree>, AppError> {
        Ok(self.trees.borrow().get(key).cloned())
    }
    fn replace_tree(&self, key: &str, value: Option<&RegistryTree>) -> Result<(), AppError> {
        if *self.fail.borrow() {
            return Err(AppError::Persistence("registry fixture"));
        }
        if let Some(value) = value {
            self.trees.borrow_mut().insert(key.into(), value.clone());
        } else {
            self.trees.borrow_mut().remove(key);
        }
        Ok(())
    }
}
#[derive(Default)]
pub struct Store {
    pub entries: Mutex<HashMap<CredentialKey, String>>,
    pub fail: Mutex<bool>,
}
impl CredentialStore for Store {
    fn read(&self, key: &CredentialKey) -> Result<UploadToken, AppError> {
        self.entries
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .map(UploadToken::new)
            .transpose()?
            .ok_or(AppError::NeedsSetup("fixture"))
    }
    fn write(&self, key: &CredentialKey, _token: &UploadToken) -> Result<(), AppError> {
        if *self.fail.lock().unwrap() {
            return Err(AppError::Credential("fixture"));
        }
        self.entries
            .lock()
            .unwrap()
            .insert(key.clone(), "synthetic-token".into());
        Ok(())
    }
    fn delete(&self, key: &CredentialKey) -> Result<(), AppError> {
        self.entries.lock().unwrap().remove(key);
        Ok(())
    }
}
#[derive(Default)]
pub struct Repo {
    pub settings: Mutex<Settings>,
    pub journal: Mutex<CredentialJournal>,
    pub fail: Mutex<bool>,
}
pub struct RepoAdapter(pub Arc<Repo>);
impl DestinationRepository for RepoAdapter {
    fn load_settings(&self) -> Result<Settings, AppError> {
        Ok(self.0.settings.lock().unwrap().clone())
    }
    fn save_settings(&self, s: &Settings) -> Result<(), AppError> {
        if *self.0.fail.lock().unwrap() {
            return Err(AppError::Persistence("fixture"));
        }
        *self.0.settings.lock().unwrap() = s.clone();
        Ok(())
    }
    fn load_journal(&self) -> Result<CredentialJournal, AppError> {
        Ok(self.0.journal.lock().unwrap().clone())
    }
    fn save_journal(&self, j: &CredentialJournal) -> Result<(), AppError> {
        *self.0.journal.lock().unwrap() = j.clone();
        Ok(())
    }
}
#[derive(Default)]
pub struct Refs(pub Mutex<HashSet<DestinationId>>);
impl DestinationReferences for Refs {
    fn set_configured_shortcuts(
        &self,
        _: &[vectrcap::app::shortcuts::ShortcutBinding],
    ) -> Result<(), AppError> {
        Ok(())
    }
    fn active_workflows(&self) -> Result<HashSet<DestinationId>, AppError> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn validate_configured(&self, _: &Settings) -> Result<(), AppError> {
        Ok(())
    }
}
pub struct InstallFixture {
    pub _dir: tempfile::TempDir,
    pub manager: DestinationManager,
    pub repo: Arc<Repo>,
    pub store: Arc<Store>,
    pub refs: Arc<Refs>,
}
impl InstallFixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = Arc::new(Repo::default());
        let store = Arc::new(Store::default());
        let refs = Arc::new(Refs::default());
        let manager = DestinationManager::open(
            Box::new(RepoAdapter(repo.clone())),
            store.clone(),
            Arc::new(History::open(&dir.path().join("history.db")).unwrap()),
            refs.clone(),
        )
        .unwrap();
        Self {
            _dir: dir,
            manager,
            repo,
            store,
            refs,
        }
    }
}

pub struct NoShell;
impl vectrcap::platform::windows::shell_actions::ShellActions for NoShell {
    fn open_path(&self, _: &std::path::Path) -> Result<(), AppError> {
        Ok(())
    }
    fn open_url(&self, _: &str) -> Result<(), AppError> {
        Ok(())
    }
    fn reveal_path(&self, _: &std::path::Path) -> Result<(), AppError> {
        Ok(())
    }
}
#[derive(Default)]
pub struct Clipboard(pub Mutex<Vec<String>>);
impl vectrcap::platform::windows::clipboard::ClipboardWriter for Clipboard {
    fn write_text(&self, text: &str) -> Result<(), AppError> {
        self.0.lock().unwrap().push(text.into());
        Ok(())
    }
}
pub fn controller(root: &std::path::Path) -> vectrcap::app::controller::AppController {
    vectrcap::app::controller::AppController::open(
        Box::new(
            vectrcap::app::destination_manager::FileDestinationRepository::open(
                &root.join("settings.json"),
            )
            .unwrap(),
        ),
        Arc::new(History::open(&root.join("history.db")).unwrap()),
        Arc::new(Store::default()),
        Arc::new(vectrcap::persistence::artifacts::ArtifactStore::new(root.join("spool")).unwrap()),
        Arc::new(NoShell),
        Arc::new(Clipboard::default()),
    )
    .unwrap()
}

/// Temporary mount-point fixture built with the Windows API, without an
/// interpreter or elevation. Never points outside the caller's TempDir.
#[allow(unsafe_code)]
pub fn junction(link: &std::path::Path, target: &std::path::Path) {
    use std::{
        fs::OpenOptions,
        os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
    };
    assert_eq!(link.parent(), target.parent());
    std::fs::create_dir(link).unwrap();
    let substitute: Vec<u16> = format!(r"\??\{}", target.display())
        .encode_utf16()
        .collect();
    let print: Vec<u16> = target.as_os_str().encode_wide().collect();
    let mut bytes = vec![];
    bytes.extend_from_slice(&0xa0000003u32.to_le_bytes());
    bytes.extend_from_slice(&((8 + (substitute.len() + print.len() + 2) * 2) as u16).to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    for n in [
        0,
        substitute.len() * 2,
        (substitute.len() + 1) * 2,
        print.len() * 2,
    ] {
        bytes.extend_from_slice(&(n as u16).to_le_bytes());
    }
    for unit in substitute.into_iter().chain([0]).chain(print).chain([0]) {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .custom_flags(0x00200000 | 0x02000000)
        .open(link)
        .unwrap();
    let mut returned = 0;
    let result = unsafe {
        windows_sys::Win32::System::IO::DeviceIoControl(
            file.as_raw_handle().cast(),
            0x000900a4,
            bytes.as_ptr().cast(),
            bytes.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        result,
        0,
        "temporary reparse fixture: {}",
        std::io::Error::last_os_error()
    );
}

/// Exercise each production staging admission independently, like the worker.
pub fn stage_selection(
    store: &vectrcap::persistence::artifacts::ArtifactStore,
    paths: &[std::ffi::OsString],
) -> Vec<Result<vectrcap::app::types::Artifact, AppError>> {
    paths
        .iter()
        .map(|path| {
            vectrcap::app::cli::upload_path(path)?;
            store.stage_file_cancellable(
                std::path::Path::new(path),
                vectrcap::app::types::ArtifactKind::File,
                &tokio_util::sync::CancellationToken::new(),
            )
        })
        .collect()
}
