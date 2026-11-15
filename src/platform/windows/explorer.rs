//! Per-user app-owned static verb. Registry trees are injectable; tests never
//! instantiate NativeRegistry. Unknown values/subkeys prevent owned removal.
use crate::app::error::AppError;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
pub const VERB_KEY: &str = r"Software\Classes\*\shell\VectrCap.Upload";
pub const CLSID: &str = "{E17546C7-ED8A-47B0-AB53-113B779D7211}";
pub const CLSID_KEY: &str = r"Software\Classes\CLSID\{E17546C7-ED8A-47B0-AB53-113B779D7211}";
const OWNER: &str = "VectrCap.Integrations.v1";
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct RegistryTree {
    values: BTreeMap<(String, String), String>,
}
impl RegistryTree {
    pub fn set(&mut self, key: &str, name: &str, value: &str) {
        self.values.insert((key.into(), name.into()), value.into());
    }
    pub fn get(&self, key: &str, name: &str) -> Option<&str> {
        self.values
            .get(&(key.into(), name.into()))
            .map(String::as_str)
    }
    pub fn owned_by_app(&self) -> bool {
        self.get("", "VectrCapOwner") == Some(OWNER)
    }
    fn owned(exe: &str) -> Self {
        let mut tree = Self::default();
        tree.set("", "VectrCapOwner", OWNER);
        tree.set("", "VectrCapExecutable", exe);
        tree
    }
}
pub trait RegistryBackend {
    fn read_tree(&self, key: &str) -> Result<Option<RegistryTree>, AppError>;
    /// Error means callers must restore the snapshot. Native writes may partially
    /// fail; the transaction reports an explicit rollback failure in that case.
    fn replace_tree(&self, key: &str, value: Option<&RegistryTree>) -> Result<(), AppError>;
    /// Atomic on the native backend. Test adapters use this default checkpoint.
    fn compare_replace(
        &self,
        key: &str,
        expected: Option<&RegistryTree>,
        value: Option<&RegistryTree>,
    ) -> Result<(), AppError> {
        if self.read_tree(key)?.as_ref() != expected {
            return Err(AppError::InUse(
                "integration registration changed; refresh status",
            ));
        }
        self.replace_tree(key, value)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationStatus {
    Disabled,
    Enabled,
    Stale,
    Foreign,
}
pub struct ExplorerIntegration<'a> {
    registry: &'a dyn RegistryBackend,
    executable: PathBuf,
}
pub(crate) fn quote_executable(exe: &Path) -> Result<String, AppError> {
    let text = exe.to_str().ok_or(AppError::InvalidInput(
        "executable path must be valid Unicode",
    ))?;
    crate::app::cli::upload_path(exe.as_os_str())?;
    if text.contains(['"', '\0'])
        || !exe
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
    {
        return Err(AppError::InvalidInput("invalid integration executable"));
    }
    Ok(format!("\"{text}\""))
}
fn templates(exe: &Path) -> Result<Vec<(String, RegistryTree)>, AppError> {
    let quoted = quote_executable(exe)?;
    let text = exe
        .to_str()
        .ok_or(AppError::InvalidInput("invalid executable path"))?;
    let mut verb = RegistryTree::owned(text);
    verb.set("", "", "Upload with Vectr");
    verb.set("", "Icon", &quoted);
    verb.set("", "MultiSelectModel", "Player");
    verb.set("DropTarget", "CLSID", CLSID);
    let mut server = RegistryTree::owned(text);
    server.set("", "", "VectrCap upload selection");
    server.set("LocalServer32", "", &format!("{quoted} --shell-server"));
    server.set("LocalServer32", "ServerExecutable", text);
    Ok(vec![(VERB_KEY.into(), verb), (CLSID_KEY.into(), server)])
}
impl<'a> ExplorerIntegration<'a> {
    pub fn new(registry: &'a dyn RegistryBackend, executable: PathBuf) -> Self {
        Self {
            registry,
            executable,
        }
    }
    pub fn enable(&self) -> Result<(), AppError> {
        if !self.executable.is_file() {
            return Err(AppError::InvalidInput(
                "install a real executable before enabling integration",
            ));
        }
        registration_apply(self.registry, &templates(&self.executable)?, true)
    }
    pub fn disable(&self) -> Result<(), AppError> {
        registration_apply(self.registry, &templates(&self.executable)?, false)
    }
    pub fn status(&self) -> Result<RegistrationStatus, AppError> {
        registration_status(self.registry, &templates(&self.executable)?)
    }
}
pub(crate) fn registration_status(
    registry: &dyn RegistryBackend,
    trees: &[(String, RegistryTree)],
) -> Result<RegistrationStatus, AppError> {
    let mut present = 0;
    let mut stale = false;
    for (key, expected) in trees {
        if let Some(actual) = registry.read_tree(key)? {
            present += 1;
            if !actual.owned_by_app() {
                return Ok(RegistrationStatus::Foreign);
            }
            if actual != *expected {
                stale = true;
            }
        }
    }
    Ok(if present == 0 {
        RegistrationStatus::Disabled
    } else if stale || present != trees.len() {
        RegistrationStatus::Stale
    } else {
        RegistrationStatus::Enabled
    })
}
fn recognized_owned(actual: &RegistryTree, expected: &RegistryTree) -> bool {
    if !actual.owned_by_app() {
        return false;
    }
    // Rebuild the template with its registered executable. This recognizes an
    // old app location but refuses any foreign additions, including subkeys.
    let Some(exe) = actual.get("", "VectrCapExecutable") else {
        return false;
    };
    let Some(expected_exe) = expected.get("", "VectrCapExecutable") else {
        return false;
    };
    let mut adjusted = expected.clone();
    for value in adjusted.values.values_mut() {
        *value = value.replace(expected_exe, exe);
    }
    adjusted == *actual
}
pub(crate) fn registration_apply(
    registry: &dyn RegistryBackend,
    trees: &[(String, RegistryTree)],
    enable: bool,
) -> Result<(), AppError> {
    let mut before = vec![];
    for (key, expected) in trees {
        let old = registry.read_tree(key)?;
        if old
            .as_ref()
            .is_some_and(|actual| !recognized_owned(actual, expected))
        {
            return Err(AppError::InUse(
                "integration registry key contains foreign data",
            ));
        }
        before.push(old);
    }
    for (index, (key, value)) in trees.iter().enumerate() {
        // Ownership and the write share one native registry transaction.
        if let Err(primary) =
            registry.compare_replace(key, before[index].as_ref(), enable.then_some(value))
        {
            let mut failed = false;
            for i in (0..=index).rev() {
                let written = enable.then_some(&trees[i].1);
                match registry.read_tree(&trees[i].0) {
                    Ok(current) if current.as_ref() == before[i].as_ref() => {}
                    Ok(current) if current.as_ref() == written => {
                        if registry
                            .compare_replace(&trees[i].0, written, before[i].as_ref())
                            .is_err()
                        {
                            failed = true;
                        }
                    }
                    _ => failed = true,
                }
            }
            return if failed {
                Err(AppError::Persistence(
                    "integration update failed; registry rollback requires repair",
                ))
            } else {
                Err(primary)
            };
        }
    }
    Ok(())
}
pub(crate) fn owned_tree(exe: &str) -> RegistryTree {
    RegistryTree::owned(exe)
}

mod native {
    #![allow(unsafe_code)]
    use super::*;
    use std::ptr;
    use windows_sys::Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS},
        System::Registry::*,
    };
    pub struct NativeRegistry;
    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }
    fn error() -> AppError {
        AppError::Persistence("per-user integration registry unavailable")
    }
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }
    fn open(path: &str, create: bool) -> Result<Option<Key>, AppError> {
        unsafe {
            // No path is supplied by untrusted file/IPC data.
            if !path.starts_with("Software\\Classes\\") || path.contains('\0') {
                return Err(error());
            }
            let mut key = ptr::null_mut();
            let status = if create {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    wide(path).as_ptr(),
                    0,
                    ptr::null(),
                    0,
                    KEY_READ | KEY_WRITE,
                    ptr::null(),
                    &mut key,
                    ptr::null_mut(),
                )
            } else {
                RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    wide(path).as_ptr(),
                    0,
                    KEY_READ,
                    &mut key,
                )
            };
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            if status != 0 {
                return Err(error());
            }
            Ok(Some(Key(key)))
        }
    }
    fn read(
        key: &Key,
        path: &str,
        tree: &mut RegistryTree,
        depth: usize,
        transaction: Option<windows_sys::Win32::Foundation::HANDLE>,
    ) -> Result<(), AppError> {
        unsafe {
            if depth > 8 || tree.values.len() > 128 {
                return Err(error());
            }
            for i in 0..129 {
                let mut name = [0u16; 512];
                let mut name_len = name.len() as u32;
                let mut bytes = [0u8; 65536];
                let mut bytes_len = bytes.len() as u32;
                let mut kind = 0;
                let status = RegEnumValueW(
                    key.0,
                    i,
                    name.as_mut_ptr(),
                    &mut name_len,
                    ptr::null(),
                    &mut kind,
                    bytes.as_mut_ptr(),
                    &mut bytes_len,
                );
                if status == ERROR_NO_MORE_ITEMS {
                    break;
                }
                if status != 0 || kind != REG_SZ || bytes_len < 2 || !bytes_len.is_multiple_of(2) {
                    return Err(error());
                }
                let units: Vec<u16> = bytes[..bytes_len as usize]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect();
                if units.last() != Some(&0) || units[..units.len() - 1].contains(&0) {
                    return Err(error());
                }
                tree.set(
                    path,
                    &String::from_utf16(&name[..name_len as usize]).map_err(|_| error())?,
                    &String::from_utf16(&units[..units.len() - 1]).map_err(|_| error())?,
                );
            }
            for i in 0..129 {
                let mut name = [0u16; 512];
                let mut len = name.len() as u32;
                let status = RegEnumKeyExW(
                    key.0,
                    i,
                    name.as_mut_ptr(),
                    &mut len,
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                );
                if status == ERROR_NO_MORE_ITEMS {
                    return Ok(());
                }
                if status != 0 {
                    return Err(error());
                }
                let name = String::from_utf16(&name[..len as usize]).map_err(|_| error())?;
                let mut child = ptr::null_mut();
                let status = match transaction {
                    Some(tx) => RegOpenKeyTransactedW(
                        key.0,
                        wide(&name).as_ptr(),
                        0,
                        KEY_READ,
                        &mut child,
                        tx,
                        ptr::null(),
                    ),
                    None => RegOpenKeyExW(key.0, wide(&name).as_ptr(), 0, KEY_READ, &mut child),
                };
                if status != 0 {
                    return Err(error());
                }
                let child = Key(child);
                let path = if path.is_empty() {
                    name
                } else {
                    format!("{path}\\{name}")
                };
                read(&child, &path, tree, depth + 1, transaction)?;
                // Empty foreign subkeys must also block deletion.
                if !tree
                    .values
                    .keys()
                    .any(|(p, _)| p == &path || p.starts_with(&format!("{path}\\")))
                {
                    return Err(error());
                }
            }
            Err(error())
        }
    }
    impl RegistryBackend for NativeRegistry {
        fn read_tree(&self, key: &str) -> Result<Option<RegistryTree>, AppError> {
            let Some(key) = open(key, false)? else {
                return Ok(None);
            };
            let mut tree = RegistryTree::default();
            read(&key, "", &mut tree, 0, None)?;
            Ok(Some(tree))
        }
        fn replace_tree(&self, key: &str, value: Option<&RegistryTree>) -> Result<(), AppError> {
            let before = self.read_tree(key)?;
            self.compare_replace(key, before.as_ref(), value)
        }
        fn compare_replace(
            &self,
            key: &str,
            expected: Option<&RegistryTree>,
            value: Option<&RegistryTree>,
        ) -> Result<(), AppError> {
            use windows_sys::Win32::{
                Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
                Storage::FileSystem::{CommitTransaction, CreateTransaction, RollbackTransaction},
            };
            unsafe {
                if ![
                    VERB_KEY,
                    CLSID_KEY,
                    super::super::destination_association::EXTENSION_KEY,
                    super::super::destination_association::PROGID_KEY,
                ]
                .contains(&key)
                {
                    return Err(error());
                }
                let handle =
                    CreateTransaction(ptr::null_mut(), ptr::null_mut(), 0, 0, 0, 5000, ptr::null());
                if handle == INVALID_HANDLE_VALUE || handle.is_null() {
                    return Err(error());
                }
                struct Transaction(windows_sys::Win32::Foundation::HANDLE, bool);
                impl Drop for Transaction {
                    fn drop(&mut self) {
                        unsafe {
                            if !self.1 {
                                RollbackTransaction(self.0);
                            }
                            CloseHandle(self.0);
                        }
                    }
                }
                let mut transaction = Transaction(handle, false);
                let previous = if let Some(root) = open_tx(key, false, handle)? {
                    let mut tree = RegistryTree::default();
                    read(&root, "", &mut tree, 0, Some(handle))?;
                    Some(tree)
                } else {
                    None
                };
                if previous.as_ref() != expected {
                    return Err(AppError::InUse(
                        "integration registration changed; refresh status",
                    ));
                }
                if previous.as_ref().is_some_and(|tree| !tree.owned_by_app())
                    || value.is_some_and(|tree| !tree.owned_by_app())
                {
                    return Err(AppError::InUse(
                        "integration registration belongs to another application",
                    ));
                }
                if let Some(tree) = value {
                    for ((sub, name), text) in &tree.values {
                        let full = if sub.is_empty() {
                            key.into()
                        } else {
                            format!("{key}\\{sub}")
                        };
                        let root = open_tx(&full, true, handle)?.ok_or_else(error)?;
                        let bytes: Vec<u8> =
                            wide(text).iter().flat_map(|u| u.to_le_bytes()).collect();
                        if RegSetValueExW(
                            root.0,
                            wide(name).as_ptr(),
                            0,
                            REG_SZ,
                            bytes.as_ptr(),
                            bytes.len() as u32,
                        ) != 0
                        {
                            return Err(error());
                        }
                    }
                } else if let Some(tree) = previous {
                    let mut subkeys = std::collections::BTreeSet::new();
                    subkeys.insert(String::new());
                    for (sub, name) in tree.values.keys() {
                        let full = if sub.is_empty() {
                            key.into()
                        } else {
                            format!("{key}\\{sub}")
                        };
                        let root = open_tx(&full, false, handle)?.ok_or_else(error)?;
                        if RegDeleteValueW(root.0, wide(name).as_ptr()) != 0 {
                            return Err(error());
                        }
                        let mut part = sub.as_str();
                        while !part.is_empty() {
                            subkeys.insert(part.into());
                            part = part.rsplit_once('\\').map_or("", |(parent, _)| parent);
                        }
                    }
                    let mut subkeys: Vec<String> = subkeys.into_iter().collect();
                    subkeys.sort_by_key(|s| std::cmp::Reverse(s.len()));
                    for sub in subkeys {
                        let full = if sub.is_empty() {
                            key.into()
                        } else {
                            format!("{key}\\{sub}")
                        };
                        if RegDeleteKeyTransactedW(
                            HKEY_CURRENT_USER,
                            wide(&full).as_ptr(),
                            0,
                            0,
                            handle,
                            ptr::null(),
                        ) != 0
                        {
                            return Err(error());
                        }
                    }
                }
                if CommitTransaction(transaction.0) == 0 {
                    return Err(error());
                }
                transaction.1 = true;
                Ok(())
            }
        }
    }
    fn open_tx(
        path: &str,
        create: bool,
        transaction: windows_sys::Win32::Foundation::HANDLE,
    ) -> Result<Option<Key>, AppError> {
        unsafe {
            let mut key = ptr::null_mut();
            let status = if create {
                RegCreateKeyTransactedW(
                    HKEY_CURRENT_USER,
                    wide(path).as_ptr(),
                    0,
                    ptr::null(),
                    0,
                    KEY_READ | KEY_WRITE,
                    ptr::null(),
                    &mut key,
                    ptr::null_mut(),
                    transaction,
                    ptr::null(),
                )
            } else {
                RegOpenKeyTransactedW(
                    HKEY_CURRENT_USER,
                    wide(path).as_ptr(),
                    0,
                    KEY_READ | KEY_WRITE,
                    &mut key,
                    transaction,
                    ptr::null(),
                )
            };
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            if status != 0 {
                return Err(error());
            }
            Ok(Some(Key(key)))
        }
    }
}
pub use native::NativeRegistry;
