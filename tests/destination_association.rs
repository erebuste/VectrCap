mod task11_support;
use task11_support::Registry;
use vectrcap::{
    app::{cli::*, ingress::*},
    platform::windows::{destination_association::*, explorer::*},
};
fn exe() -> std::path::PathBuf {
    std::env::current_exe().unwrap()
}
#[test]
fn association_is_per_user_and_app_owned() {
    let r = Registry::default();
    DestinationAssociation::new(&r, exe()).enable().unwrap();
    assert_eq!(r.trees.borrow().len(), 2);
    assert!(
        r.trees
            .borrow()
            .keys()
            .all(|k| k.starts_with("Software\\Classes\\"))
    );
    assert!(r.trees.borrow().values().all(|t| t.owned_by_app()));
}
#[test]
fn quoted_open_command_contains_only_import_flag_and_path() {
    let r = Registry::default();
    DestinationAssociation::new(&r, exe()).enable().unwrap();
    assert_eq!(
        r.trees
            .borrow()
            .get(PROGID_KEY)
            .unwrap()
            .get("shell\\open\\command", "")
            .unwrap(),
        format!("\"{}\" --import-destination -- \"%1\"", exe().display())
    );
}
#[test]
fn file_open_handoff_never_uploads_config() {
    let LaunchMode::Request(r) =
        parse_launch_args(["--import-destination", "--", r"C:\private.vectrcap"].map(Into::into))
            .unwrap()
    else {
        panic!()
    };
    assert!(matches!(
        r.kind,
        LaunchRequestKind::ImportDestination { .. }
    ));
}
#[test]
fn first_run_import_needs_no_existing_token() {
    let d = tempfile::tempdir().unwrap();
    let mut ingress = Ingress::open(&d.path().join("ingress.db")).unwrap();
    ingress
        .accept(LaunchRequest::import(r"C:\private.vectrcap".into()).unwrap())
        .unwrap();
    assert_eq!(ingress.pending().unwrap().len(), 1);
}
#[test]
fn running_instance_prompts_once_after_ack_loss() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("ingress.db");
    let request = LaunchRequest::import(r"C:\private.vectrcap".into()).unwrap();
    let mut ingress = Ingress::open(&db).unwrap();
    ingress.accept(request.clone()).unwrap();
    drop(ingress);
    let mut ingress = Ingress::open(&db).unwrap();
    assert!(ingress.accept(request).unwrap().duplicate);
    assert_eq!(ingress.pending().unwrap().len(), 1);
    for _ in 1..16 {
        ingress
            .accept(LaunchRequest::import(r"C:\other.vectrcap".into()).unwrap())
            .unwrap();
    }
    assert!(
        ingress
            .accept(LaunchRequest::import(r"C:\overflow.vectrcap".into()).unwrap())
            .is_err()
    );
}
#[test]
fn disable_preserves_foreign_association_and_userchoice() {
    let r = Registry::default();
    let mut foreign = RegistryTree::default();
    foreign.set("", "", "Foreign.ProgID");
    r.trees
        .borrow_mut()
        .insert(EXTENSION_KEY.into(), foreign.clone());
    let choice =
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.vectrcap\UserChoice";
    r.trees.borrow_mut().insert(choice.into(), foreign.clone());
    DestinationAssociation::new(&r, exe()).enable().unwrap();
    DestinationAssociation::new(&r, exe()).disable().unwrap();
    assert_eq!(r.trees.borrow().get(EXTENSION_KEY), Some(&foreign));
    assert_eq!(r.trees.borrow().get(choice), Some(&foreign));
}
#[test]
fn moved_executable_reports_stale_association() {
    let r = Registry::default();
    DestinationAssociation::new(&r, exe()).enable().unwrap();
    assert_eq!(
        DestinationAssociation::new(&r, r"C:\moved\VectrCap.exe".into())
            .status()
            .unwrap(),
        RegistrationStatus::Stale
    );
}
#[test]
fn controller_import_is_serialized_and_never_staged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.vectrcap");
    std::fs::write(&path,br#"{"format":"vectrcap-destination","version":1,"name":"Phone","origin":"https://v.example","token":"synthetic"}"#).unwrap();
    let mut app = task11_support::controller(dir.path());
    let request = LaunchRequest::import(path.into_os_string()).unwrap();
    let (_, effects) = app.accept_launch(request.clone()).unwrap();
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, vectrcap::app::events::AppEffect::StageFiles { .. }))
    );
    assert_eq!(
        app.install_view().unwrap().state,
        vectrcap::app::destination_install::InstallState::AwaitingVerifyConsent
    );
    app.accept_launch(request).unwrap();
    assert_eq!(app.pending_ingress_count().unwrap(), 1);
}
