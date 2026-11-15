mod task11_support;
use task11_support::*;
use vectrcap::{app::destination_install::*, vectr::uploader_info::UploaderInfoSupport};
fn bytes() -> Vec<u8> {
    br#"{"format":"vectrcap-destination","version":1,"name":"Phone","origin":"https://v.example","token":"PRIVATE-CANARY"}"#.to_vec()
}
fn session() -> DestinationInstallSession {
    DestinationInstallSession::from_bytes(&bytes()).unwrap()
}
fn unverified(s: &mut DestinationInstallSession) {
    s.consent().unwrap();
    s.verification_result(Ok(UploaderInfoSupport::Unsupported404))
        .unwrap();
    s.allow_unverified().unwrap();
}
#[test]
fn valid_v1_has_no_domain_override() {
    let d = DestinationInstallV1::parse(&bytes()).unwrap();
    assert_eq!(d.name(), "Phone");
    assert_eq!(d.origin().as_str(), "https://v.example");
    let mut f = InstallFixture::new();
    let mut s = session();
    unverified(&mut s);
    let id = s
        .install(
            &mut f.manager,
            InstallChoice::Add {
                name: "Phone".into(),
                set_default: false,
            },
        )
        .unwrap();
    assert!(
        f.manager
            .settings()
            .destinations
            .get(id)
            .unwrap()
            .link_domain_override
            .is_none()
    );
}
#[test]
fn rejects_wrong_format_version_unknown_duplicate_fields() {
    let input = String::from_utf8(bytes()).unwrap();
    for bad in [
        input.replace("vectrcap-destination", "sxcu"),
        input.replace("\"version\":1", "\"version\":2"),
        input.replace("\"version\":1", "\"version\":1,\"version\":1"),
        input.replace("\"version\":1", "\"version\":1,\"extra\":true"),
        format!("{input}{{}}"),
        input.replace("\"name\":\"Phone\",", ""),
    ] {
        assert!(DestinationInstallV1::parse(bad.as_bytes()).is_err());
    }
}
#[test]
fn utf8_size_and_header_safe_token_bounds() {
    let input = String::from_utf8(bytes()).unwrap();
    for token in ["", " x", "x ", "x\\r\\nx", &"x".repeat(2561), "ä"] {
        assert!(
            DestinationInstallV1::parse(input.replace("PRIVATE-CANARY", token).as_bytes()).is_err()
        );
    }
    assert!(DestinationInstallV1::parse(&[b' '; 16385]).is_err());
    assert!(DestinationInstallV1::parse(&[0xff]).is_err());
    let mut padded = bytes();
    padded.resize(16384, b' ');
    assert!(DestinationInstallV1::parse(&padded).is_ok());
}
#[test]
fn no_network_before_origin_consent() {
    let mut s = session();
    assert!(s.verification_client().is_err());
    assert!(
        s.verification_result(Ok(UploaderInfoSupport::Unsupported404))
            .is_err()
    );
    assert_eq!(s.state(), InstallState::AwaitingVerifyConsent);
}
#[test]
fn verification_uses_only_displayed_origin() {
    let mut s = session();
    s.consent().unwrap();
    assert_eq!(
        s.verification_origin().unwrap().as_str(),
        "https://v.example"
    );
    assert!(s.verification_client().is_ok());
}
#[test]
fn metadata_uploader_label_not_file_claim() {
    let mut s = session();
    unverified(&mut s);
    assert!(s.uploader_name().is_none());
    assert_eq!(s.verification(), Verification::Unverified);
    let mut blocked = session();
    blocked.consent().unwrap();
    assert!(
        blocked
            .verification_result(Err(vectrcap::app::error::AppError::HttpStatus {
                code: 401,
                retry_after: None
            }))
            .is_err()
    );
    assert!(blocked.allow_unverified().is_err());
}
mod support;
#[tokio::test]
async fn verified_label_comes_from_task2_metadata_and_malformed_blocks() {
    let fixture=support::Fixture::new(vec![support::Reply::json(200,r#"{"name":"Real authenticated uploader","domains":["https://links.example"],"domain":"https://links.example","settings":{"expires":"never","maxViews":null,"nameFormat":"random","convert":"off","keepName":false,"passwordProtected":false}}"#)]).await;
    let input = String::from_utf8(bytes())
        .unwrap()
        .replace("https://v.example", fixture.origin.as_str())
        .replace("PRIVATE-CANARY", "synthetic-token");
    let mut s = DestinationInstallSession::from_bytes(input.as_bytes()).unwrap();
    assert!(s.verification_client().is_err());
    assert!(fixture.log().is_empty());
    s.consent().unwrap();
    assert_eq!(s.verification_origin().unwrap(), &fixture.origin);
    let info = fixture.client().uploader_info().await;
    s.verification_result(info).unwrap();
    assert_eq!(s.uploader_name(), Some("Real authenticated uploader"));
    assert_eq!(s.verification(), Verification::Verified);
    let mut s = session();
    s.consent().unwrap();
    assert!(
        s.verification_result(Err(vectrcap::app::error::AppError::Protocol(
            "malformed uploader metadata"
        )))
        .is_err()
    );
    assert!(s.allow_unverified().is_err());
    for error in [
        vectrcap::app::error::AppError::Network,
        vectrcap::app::error::AppError::Tls,
    ] {
        let mut s = session();
        s.consent().unwrap();
        s.verification_result(Err(error)).unwrap();
        assert!(!s.install_allowed());
        s.allow_unverified().unwrap();
        assert!(s.install_allowed());
        assert_eq!(s.verification(), Verification::Unverified);
    }
}
#[test]
fn cancel_keeps_credentials_and_source() {
    let f = InstallFixture::new();
    let mut s = session();
    s.cancel();
    assert_eq!(s.state(), InstallState::Cancelled);
    assert!(f.store.entries.lock().unwrap().is_empty());
    assert!(!s.can_delete_source());
}
#[test]
fn same_name_origin_requires_explicit_replace_or_rename() {
    let mut f = InstallFixture::new();
    let mut first = session();
    unverified(&mut first);
    first
        .install(
            &mut f.manager,
            InstallChoice::Add {
                name: "Phone".into(),
                set_default: false,
            },
        )
        .unwrap();
    let mut second = session();
    unverified(&mut second);
    assert!(
        second
            .install(
                &mut f.manager,
                InstallChoice::Add {
                    name: "Phone".into(),
                    set_default: false
                }
            )
            .is_err()
    );
    assert_eq!(second.state(), InstallState::AwaitingInstall);
    second
        .install(
            &mut f.manager,
            InstallChoice::Add {
                name: "Phone 2".into(),
                set_default: false,
            },
        )
        .unwrap();
    assert_eq!(f.manager.settings().destinations.len(), 2);
}
#[test]
fn replace_rejected_for_active_destination() {
    let mut f = InstallFixture::new();
    let mut first = session();
    unverified(&mut first);
    let id = first
        .install(
            &mut f.manager,
            InstallChoice::Add {
                name: "Phone".into(),
                set_default: false,
            },
        )
        .unwrap();
    f.refs.0.lock().unwrap().insert(id);
    let before = f.manager.settings().clone();
    let mut second = session();
    unverified(&mut second);
    assert!(
        second
            .install(&mut f.manager, InstallChoice::Replace(id))
            .is_err()
    );
    assert_eq!(f.manager.settings(), &before);
    assert_eq!(second.state(), InstallState::AwaitingInstall);
}
#[test]
fn credential_or_settings_failure_rolls_back_install() {
    for credential_failure in [true, false] {
        let mut f = InstallFixture::new();
        *f.store.fail.lock().unwrap() = credential_failure;
        *f.repo.fail.lock().unwrap() = !credential_failure;
        let mut s = session();
        unverified(&mut s);
        let before = f.manager.settings().clone();
        assert!(
            s.install(
                &mut f.manager,
                InstallChoice::Add {
                    name: "Phone".into(),
                    set_default: false
                }
            )
            .is_err()
        );
        assert_eq!(s.state(), InstallState::AwaitingInstall);
        assert_eq!(f.manager.settings(), &before);
        assert!(f.store.entries.lock().unwrap().is_empty());
        assert!(!s.can_delete_source());
        *f.store.fail.lock().unwrap() = false;
        *f.repo.fail.lock().unwrap() = false;
        s.install(
            &mut f.manager,
            InstallChoice::Add {
                name: "Phone".into(),
                set_default: false,
            },
        )
        .unwrap();
    }
}
#[test]
fn install_does_not_upload_or_change_existing_default() {
    let mut f = InstallFixture::new();
    let mut s = session();
    unverified(&mut s);
    let first = s
        .install(
            &mut f.manager,
            InstallChoice::Add {
                name: "First".into(),
                set_default: false,
            },
        )
        .unwrap();
    let mut second = session();
    unverified(&mut second);
    second
        .install(
            &mut f.manager,
            InstallChoice::Add {
                name: "Second".into(),
                set_default: false,
            },
        )
        .unwrap();
    assert_eq!(f.manager.settings().destinations.default_id(), Some(first));
}
#[test]
fn delete_offer_only_after_success() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.vectrcap");
    std::fs::write(&path, bytes()).unwrap();
    let mut s = DestinationInstallSession::read(&path).unwrap();
    assert!(!s.can_delete_source());
    assert!(s.delete_source().is_err());
    let mut f = InstallFixture::new();
    unverified(&mut s);
    s.install(
        &mut f.manager,
        InstallChoice::Add {
            name: "Phone".into(),
            set_default: false,
        },
    )
    .unwrap();
    assert!(s.can_delete_source());
    s.delete_source().unwrap();
    assert!(!path.exists());
    assert_eq!(s.state(), InstallState::Installed);
}
#[test]
fn changed_or_reparse_source_never_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.vectrcap");
    std::fs::write(&path, bytes()).unwrap();
    let mut s = DestinationInstallSession::read(&path).unwrap();
    let mut f = InstallFixture::new();
    unverified(&mut s);
    s.install(
        &mut f.manager,
        InstallChoice::Add {
            name: "Phone".into(),
            set_default: false,
        },
    )
    .unwrap();
    std::fs::write(&path, b"changed").unwrap();
    assert!(s.delete_source().is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"changed");
    assert_eq!(s.state(), InstallState::Installed);

    // Swap a whole parent directory for a junction with byte-identical data.
    // A content hash alone cannot authorize deleting the redirected file.
    let original = dir.path().join("original");
    let target = dir.path().join("target");
    std::fs::create_dir(&original).unwrap();
    std::fs::create_dir(&target).unwrap();
    let source = original.join("download.vectrcap");
    std::fs::write(&source, bytes()).unwrap();
    let mut session = DestinationInstallSession::read(&source).unwrap();
    unverified(&mut session);
    session
        .install(
            &mut f.manager,
            InstallChoice::Add {
                name: "Reparse".into(),
                set_default: false,
            },
        )
        .unwrap();
    std::fs::rename(&original, dir.path().join("moved")).unwrap();
    std::fs::write(target.join("download.vectrcap"), bytes()).unwrap();
    junction(&original, &target);
    assert!(session.delete_source().is_err());
    assert!(DestinationInstallSession::read(&source).is_err());
    assert_eq!(
        std::fs::read(target.join("download.vectrcap")).unwrap(),
        bytes()
    );
    std::fs::remove_dir(&original).unwrap();
}
#[test]
fn private_canaries_absent_from_logs_exports_diagnostics_ipc() {
    let mut input = bytes();
    input.extend_from_slice(b"PRIVATE-CANARY");
    let error = DestinationInstallV1::parse(&input).err().unwrap();
    assert!(!format!("{error:?} {error}").contains("PRIVATE-CANARY"));
    let mut f = InstallFixture::new();
    let mut s = session();
    unverified(&mut s);
    s.install(
        &mut f.manager,
        InstallChoice::Add {
            name: "Phone".into(),
            set_default: false,
        },
    )
    .unwrap();
    assert!(
        !serde_json::to_string(f.manager.settings())
            .unwrap()
            .contains("PRIVATE-CANARY")
    );
}
