mod task11_support;
use task11_support::Registry;
use vectrcap::platform::windows::explorer::*;
fn exe() -> std::path::PathBuf {
    std::env::current_exe().unwrap()
}
#[test]
fn registration_is_per_user_and_owned() {
    let r = Registry::default();
    ExplorerIntegration::new(&r, exe()).enable().unwrap();
    let trees = r.trees.borrow();
    assert_eq!(trees.len(), 2);
    assert!(trees.keys().all(|k| k.starts_with("Software\\Classes\\")));
    assert!(trees.values().all(|t| t.owned_by_app()));
}
#[test]
fn quoted_executable_has_no_token() {
    let r = Registry::default();
    ExplorerIntegration::new(&r, exe()).enable().unwrap();
    let trees = r.trees.borrow();
    let server = trees
        .get(CLSID_KEY)
        .unwrap()
        .get("LocalServer32", "")
        .unwrap();
    assert_eq!(server, format!("\"{}\" --shell-server", exe().display()));
    assert!(!server.contains("token"));
}
#[test]
fn multi_select_uses_drop_data_not_command_concatenation() {
    let r = Registry::default();
    ExplorerIntegration::new(&r, exe()).enable().unwrap();
    let trees = r.trees.borrow();
    let verb = trees.get(VERB_KEY).unwrap();
    assert_eq!(verb.get("", "MultiSelectModel").unwrap(), "Player");
    assert!(verb.get("DropTarget", "CLSID").is_some());
    assert!(verb.get("command", "").is_none());
}
#[test]
fn disable_preserves_unrelated_registry_entries() {
    let r = Registry::default();
    let mut foreign = RegistryTree::default();
    foreign.set("", "", "Foreign");
    r.trees
        .borrow_mut()
        .insert(VERB_KEY.into(), foreign.clone());
    let integration = ExplorerIntegration::new(&r, exe());
    assert!(integration.enable().is_err());
    assert!(integration.disable().is_err());
    assert_eq!(r.trees.borrow().get(VERB_KEY), Some(&foreign));
}
#[test]
fn moved_executable_reports_stale_entry() {
    let r = Registry::default();
    let integration = ExplorerIntegration::new(&r, exe());
    integration.enable().unwrap();
    let moved = ExplorerIntegration::new(&r, std::path::PathBuf::from(r"C:\missing\VectrCap.exe"));
    assert_eq!(moved.status().unwrap(), RegistrationStatus::Stale);
    *r.fail.borrow_mut() = true;
    assert!(integration.disable().is_err());
    assert_eq!(integration.status().unwrap(), RegistrationStatus::Enabled);
}
#[test]
fn late_ownership_read_failure_rolls_back_earlier_registry_write() {
    struct Fail {
        r: Registry,
        reads: std::cell::Cell<usize>,
    }
    impl RegistryBackend for Fail {
        fn read_tree(
            &self,
            key: &str,
        ) -> Result<Option<RegistryTree>, vectrcap::app::error::AppError> {
            let n = self.reads.get() + 1;
            self.reads.set(n);
            if n == 4 {
                return Err(vectrcap::app::error::AppError::Persistence(
                    "ownership read failed",
                ));
            }
            self.r.read_tree(key)
        }
        fn replace_tree(
            &self,
            key: &str,
            value: Option<&RegistryTree>,
        ) -> Result<(), vectrcap::app::error::AppError> {
            self.r.replace_tree(key, value)
        }
    }
    let registry = Fail {
        r: Registry::default(),
        reads: std::cell::Cell::new(0),
    };
    assert!(ExplorerIntegration::new(&registry, exe()).enable().is_err());
    assert!(registry.r.trees.borrow().is_empty());
}
#[test]
fn rollback_never_overwrites_a_foreign_intervening_tree() {
    struct Intervening {
        r: Registry,
        writes: std::cell::Cell<usize>,
    }
    impl RegistryBackend for Intervening {
        fn read_tree(
            &self,
            key: &str,
        ) -> Result<Option<RegistryTree>, vectrcap::app::error::AppError> {
            self.r.read_tree(key)
        }
        fn replace_tree(
            &self,
            key: &str,
            value: Option<&RegistryTree>,
        ) -> Result<(), vectrcap::app::error::AppError> {
            let n = self.writes.get() + 1;
            self.writes.set(n);
            if n == 2 {
                let mut foreign = RegistryTree::default();
                foreign.set("", "", "foreign intervening data");
                self.r.trees.borrow_mut().insert(VERB_KEY.into(), foreign);
                return Err(vectrcap::app::error::AppError::Persistence("write failed"));
            }
            self.r.replace_tree(key, value)
        }
    }
    let r = Intervening {
        r: Registry::default(),
        writes: std::cell::Cell::new(0),
    };
    assert!(ExplorerIntegration::new(&r, exe()).enable().is_err());
    assert_eq!(
        r.r.trees.borrow().get(VERB_KEY).unwrap().get("", ""),
        Some("foreign intervening data")
    );
}
