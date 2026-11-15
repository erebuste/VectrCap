use std::{
    ffi::OsString,
    os::windows::ffi::{OsStrExt, OsStringExt},
};
use task11_support::stage_selection;
use vectrcap::app::{cli::*, ingress::*};
use vectrcap::platform::windows::ipc::*;

fn upload(paths: &[&str]) -> LaunchRequest {
    LaunchRequest::upload(paths.iter().map(OsString::from).collect()).unwrap()
}
#[test]
fn zip_and_metacharacter_paths_are_data_only() {
    let paths = [r"C:\data\a & $(evil).zip", r"C:\data\;whoami.zip"];
    let args = [OsString::from("--upload"), "--".into()]
        .into_iter()
        .chain(paths.map(OsString::from));
    let LaunchMode::Request(request) = parse_launch_args(args).unwrap() else {
        panic!()
    };
    assert!(request.kind == upload(&paths).kind);
    assert!(decode_frame(&encode_frame(&request).unwrap()).unwrap().kind == request.kind);
}
#[test]
fn com_activation_marker_is_accepted_only_for_shell_server() {
    assert!(matches!(
        parse_launch_args(["--shell-server", "-Embedding"].map(Into::into)).unwrap(),
        LaunchMode::ShellServer
    ));
    for args in [
        vec!["-Embedding"],
        vec!["--shell-server", "-Embedding", "--upload"],
        vec!["--upload", "-Embedding", "--", r"C:\a.zip"],
    ] {
        assert!(parse_launch_args(args.into_iter().map(Into::into)).is_err());
    }
}
#[test]
fn unicode_and_leading_hyphen_paths_roundtrip() {
    let path = OsString::from_wide(&[67, 58, 92, 45, 0xd800, 0x00e4, 0x4e2d]);
    let request = LaunchRequest::upload(vec![path.clone()]).unwrap();
    let decoded = decode_frame(&encode_frame(&request).unwrap()).unwrap();
    let LaunchRequestKind::UploadFiles { paths } = decoded.kind else {
        panic!()
    };
    assert_eq!(
        paths[0].encode_wide().collect::<Vec<_>>(),
        path.encode_wide().collect::<Vec<_>>()
    );
    assert!(
        parse_launch_args(["--upload", "--import-destination", "--", "x"].map(OsString::from))
            .is_err()
    );
}
#[test]
fn invalid_sibling_does_not_drop_valid_files() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.zip");
    std::fs::write(&file, b"PK\x03\x04unchanged").unwrap();
    let store =
        vectrcap::persistence::artifacts::ArtifactStore::new(dir.path().join("spool")).unwrap();
    let outcomes = stage_selection(
        &store,
        &[
            file.clone().into_os_string(),
            dir.path().join("missing").into_os_string(),
        ],
    );
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes[0].is_ok());
    assert!(outcomes[1].is_err());
    assert_eq!(
        std::fs::read(&outcomes[0].as_ref().unwrap().path).unwrap(),
        b"PK\x03\x04unchanged"
    );
    assert!(file.exists());
}
#[test]
fn missing_token_preserves_selection_for_setup() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ingress.db");
    let request = upload(&[r"C:\data\pending.zip"]);
    let id = request.request_id;
    {
        let mut ingress = Ingress::open(&db).unwrap();
        ingress.accept(request).unwrap();
    }
    let ingress = Ingress::open(&db).unwrap();
    let pending = ingress.pending().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request.request_id, id);
    assert!(pending[0].destination.is_none());
}
#[test]
fn second_instance_handoff_has_no_second_tray() {
    let request = upload(&[r"C:\a.zip"]);
    let mut attempts = 0;
    let mode = handoff_with_retry(&request, |frame| {
        attempts += 1;
        assert_eq!(decode_frame(frame).unwrap().request_id, request.request_id);
        Ok(request.request_id)
    })
    .unwrap();
    assert_eq!(mode, HandoffResult::ExitSecondary);
    assert_eq!(attempts, 1);
}
#[test]
fn ack_loss_does_not_duplicate_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ingress.db");
    let request = upload(&[r"C:\a.zip"]);
    let mut ingress = Ingress::open(&db).unwrap();
    let mut attempts = 0;
    handoff_with_retry(&request, |frame| {
        attempts += 1;
        let ack = ingress.accept(decode_frame(frame)?)?;
        if attempts == 1 {
            Err(vectrcap::app::error::AppError::Network)
        } else {
            Ok(ack.request_id)
        }
    })
    .unwrap();
    assert_eq!(attempts, 2);
    assert_eq!(ingress.pending().unwrap().len(), 1);
    ingress.complete(request.request_id).unwrap();
    drop(ingress);
    let mut ingress = Ingress::open(&db).unwrap();
    assert!(ingress.accept(request).unwrap().duplicate);
    assert!(ingress.pending().unwrap().is_empty());
}
#[test]
fn completed_ingress_keeps_dedup_without_retaining_source_path() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ingress.db");
    let mut ingress = Ingress::open(&db).unwrap();
    let request = upload(&[r"C:\PRIVATE-PATH-CANARY.zip"]);
    let id = request.request_id;
    ingress.accept(request.clone()).unwrap();
    ingress.complete(id).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    let stored: Vec<u8> = conn
        .query_row(
            "SELECT frame FROM ingress WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored.len(), 32);
    assert!(ingress.accept(request).unwrap().duplicate);
}
#[test]
fn foreign_session_or_remote_pipe_clients_refused() {
    let policy = PipePolicy::new("S-1-5-5-11-22", 7).unwrap();
    assert!(policy.authorize(false, 7, true).is_ok());
    for (remote, session, logon) in [(true, 7, true), (false, 8, true), (false, 7, false)] {
        assert!(policy.authorize(remote, session, logon).is_err());
    }
    assert!(policy.sddl().contains("S-1-5-5-11-22"));
    assert!(!policy.sddl().contains(";;;WD"));
    assert_ne!(
        policy.instance_name("VectrCap-v1").unwrap(),
        PipePolicy::new("S-1-5-5-11-23", 7)
            .unwrap()
            .instance_name("VectrCap-v1")
            .unwrap()
    );
}
#[test]
fn oversized_payload_and_path_count_refused() {
    assert!(LaunchRequest::upload(vec![]).is_err());
    assert!(LaunchRequest::upload(vec![r"C:\a".into(); 101]).is_err());
    assert!(decode_frame(&vec![0; 1024 * 1024 + 5]).is_err());
    for path in [
        r"\\server\a",
        r"\\.\NUL",
        r"C:\a:stream",
        "https://example.org/a",
    ] {
        assert!(upload_path(path.as_ref()).is_err());
    }
}
#[test]
fn source_write_during_staging_is_refused() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.zip");
    std::fs::write(&path, b"zip").unwrap();
    let _writer = std::fs::OpenOptions::new()
        .write(true)
        .share_mode(7)
        .open(&path)
        .unwrap();
    let store =
        vectrcap::persistence::artifacts::ArtifactStore::new(dir.path().join("spool")).unwrap();
    assert!(stage_selection(&store, &[path.into_os_string()])[0].is_err());
}
#[test]
fn staging_never_follows_a_redirected_source_directory() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("a.zip"), b"original").unwrap();
    let link = dir.path().join("link");
    task11_support::junction(&link, &target);
    let store =
        vectrcap::persistence::artifacts::ArtifactStore::new(dir.path().join("spool")).unwrap();
    assert!(stage_selection(&store, &[link.join("a.zip").into_os_string()])[0].is_err());
    assert_eq!(std::fs::read(target.join("a.zip")).unwrap(), b"original");
    std::fs::remove_dir(link).unwrap();
}
mod task11_support;
#[test]
fn controller_ack_precedes_setup_and_recovery_preserves_request() {
    let dir = tempfile::tempdir().unwrap();
    let request = upload(&[r"C:\data\pending.zip"]);
    let id = request.request_id;
    {
        let mut app = task11_support::controller(dir.path());
        let (ack, effects) = app.accept_launch(request.clone()).unwrap();
        assert_eq!(ack.request_id, id);
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, vectrcap::app::events::AppEffect::StageFiles { .. }))
        );
    }
    let mut app = task11_support::controller(dir.path());
    let (ack, _) = app.accept_launch(request).unwrap();
    assert!(ack.duplicate);
    assert_eq!(app.pending_ingress_count().unwrap(), 1);
}
#[test]
fn pending_selection_cancel_commits_before_removal() {
    use vectrcap::app::events::*;
    let dir = tempfile::tempdir().unwrap();
    let mut app = task11_support::controller(dir.path());
    let request = upload(&[r"C:\data\pending.zip"]);
    let id = request.request_id;
    app.accept_launch(request).unwrap();
    let conn = rusqlite::Connection::open(dir.path().join("ingress.db")).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_cancel BEFORE UPDATE OF done ON ingress BEGIN SELECT RAISE(FAIL,'fixture'); END;").unwrap();
    let effects = app.dispatch(AppCommand::CancelPendingIngress(id));
    assert!(effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
    assert_eq!(app.pending_ingress_count().unwrap(), 1);
    conn.execute_batch("DROP TRIGGER fail_cancel").unwrap();
    let effects = app.dispatch(AppCommand::CancelPendingIngress(id));
    assert!(!effects.iter().any(|e| matches!(e, AppEffect::Error(_))));
    assert_eq!(app.pending_ingress_count().unwrap(), 0);
}
#[tokio::test(start_paused = true)]
async fn pipe_adapter_bounds_reads_and_commits_before_ack() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let policy = PipePolicy::new("S-1-5-5-11-22", 7).unwrap();
    let (mut client, mut server) = tokio::io::duplex(4096);
    client.write_u32_le(1024 * 1024 + 1).await.unwrap();
    assert!(
        receive_path_request(&mut server, &policy, 7, |_| async {
            panic!("oversized frame reached acceptance")
        })
        .await
        .is_err()
    );
    let (mut client, mut server) = tokio::io::duplex(4096);
    client.write_u32_le(12).await.unwrap();
    assert!(
        receive_path_request(&mut server, &policy, 7, |_| async {
            panic!("incomplete frame reached acceptance")
        })
        .await
        .is_err()
    );
    let dir = tempfile::tempdir().unwrap();
    let mut ingress = Ingress::open(&dir.path().join("ingress.db")).unwrap();
    let request = upload(&[r"C:\a.zip"]);
    let id = request.request_id;
    let frame = encode_frame(&request).unwrap();
    let (mut client, mut server) = tokio::io::duplex(4096);
    let reader = async {
        client.write_all(&frame).await.unwrap();
        let mut ack = [0; 16];
        client.read_exact(&mut ack).await.unwrap();
        assert_eq!(uuid::Uuid::from_bytes(ack), id);
        client.write_u8(1).await.unwrap();
    };
    let receiver = receive_path_request(&mut server, &policy, 7, |request| async {
        let ack = ingress.accept(request)?;
        assert_eq!(ingress.pending()?.len(), 1);
        Ok(ack.request_id)
    });
    let (_, result) = tokio::join!(reader, receiver);
    result.unwrap();
    assert_eq!(ingress.pending().unwrap().len(), 1);
}
