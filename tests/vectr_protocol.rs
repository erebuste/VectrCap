mod support;
use bytes::Bytes;
use support::*;
use vectrcap::{
    app::error::AppError,
    persistence::settings::LinkOrigin,
    vectr::{
        client::{UploadToken, VectrClient},
        protocol::{UploadStatus, piece_count},
    },
};

#[tokio::test]
async fn uses_raw_authorization_and_server_chunk_size() {
    let f = Fixture::new(vec![
        Reply::json(201, format!(r#"{{"id":"{ID}","chunkBytes":3}}"#)),
        Reply::json(204, ""),
        Reply::json(200, RECEIPT),
    ])
    .await;
    let c = f.client();
    let s = c.start("a.zip", 5, None).await.unwrap();
    assert_eq!(s.chunk_bytes, 3);
    c.put_piece(s.id, 0, Bytes::from_static(b"abc"))
        .await
        .unwrap();
    let r = c.complete(s.id).await.unwrap();
    assert_eq!(r.name(), "remote.webp");
    let log = f.log();
    assert_eq!(log[0].path, "/api/uploads");
    assert_eq!(log[0].method, "POST");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&log[0].body).unwrap(),
        serde_json::json!({"filename":"a.zip","size":5})
    );
    assert_eq!(log[1].path, format!("/api/uploads/{ID}/0"));
    assert_eq!(log[1].method, "PUT");
    assert_eq!(log[1].body, b"abc");
    assert_eq!(log[2].path, format!("/api/uploads/{ID}/complete"));
    for r in log {
        assert!(r.headers.contains("authorization: synthetic-token\r\n"));
        assert!(!r.headers.contains("cookie:"));
        assert!(!r.headers.contains("bearer"));
    }
}
#[tokio::test]
async fn rejects_redirect_without_forwarding_token() {
    let target = Fixture::new(vec![Reply::json(200, RECEIPT)]).await;
    let f = Fixture::new(vec![
        Reply::json(307, "").header("Location", target.origin.as_str()),
    ])
    .await;
    assert!(matches!(
        f.client().start("a", 1, None).await,
        Err(AppError::HttpStatus { code: 307, .. })
    ));
    assert!(target.log().is_empty());
}
#[tokio::test]
async fn rejects_bad_tls() {
    let f = Fixture::new(vec![Reply::json(200, RECEIPT)]).await;
    let c = VectrClient::new(
        f.origin.clone(),
        UploadToken::new("synthetic".into()).unwrap(),
    )
    .unwrap();
    assert_eq!(c.complete(ID.parse().unwrap()).await, Err(AppError::Tls));
    assert!(f.log().is_empty());
}
#[tokio::test]
async fn bounds_untrusted_protocol_fields() {
    assert_eq!(
        piece_count(u64::MAX, 1),
        Err(AppError::Protocol(
            "piece count exceeds compatibility limit"
        ))
    );
    for body in [
        r#"{"id":"bad","chunkBytes":3}"#.to_string(),
        format!(r#"{{"id":"{ID}","chunkBytes":0}}"#),
        format!(r#"{{"id":"{ID}","chunkBytes":67108865}}"#),
    ] {
        let f = Fixture::new(vec![Reply::json(201, body)]).await;
        assert!(matches!(
            f.client().start("a", 1, None).await,
            Err(AppError::Protocol(_))
        ));
    }
    for body in [
        r#"{"chunkBytes":3,"received":[-1]}"#,
        r#"{"chunkBytes":3,"received":[1,1]}"#,
        r#"{"chunkBytes":3,"received":[100000]}"#,
        r#"{"chunkBytes":3}"#,
        r#"{"chunkBytes":3,"received":[],"done":{"url":"http://bad/a","name":"a"}}"#,
    ] {
        let f = Fixture::new(vec![Reply::json(200, body)]).await;
        assert!(matches!(
            f.client().status(ID.parse().unwrap()).await,
            Err(AppError::Protocol(_))
        ));
    }
    let f = Fixture::new(vec![Reply::json(500, vec![b'x'; 1048577])]).await;
    assert!(matches!(
        f.client().complete(ID.parse().unwrap()).await,
        Err(AppError::Protocol(_))
    ));
}
#[tokio::test]
async fn empty_file_uses_multipart() {
    let f = Fixture::new(vec![Reply::json(200, RECEIPT)]).await;
    f.client().upload_empty("empty.zip", None).await.unwrap();
    let l = f.log();
    assert_eq!(l[0].path, "/api/upload");
    let b = String::from_utf8_lossy(&l[0].body);
    assert!(b.contains("name=\"file\"; filename=\"empty.zip\""));
    assert!(b.contains("\r\n\r\n\r\n--"));
}
#[tokio::test]
async fn domain_header_absent_when_inherited() {
    let f = Fixture::new(vec![Reply::json(
        201,
        format!(r#"{{"id":"{ID}","chunkBytes":3}}"#),
    )])
    .await;
    f.client().start("a", 1, None).await.unwrap();
    assert!(!f.log()[0].headers.contains("x-vectr-"));
}
#[tokio::test]
async fn domain_override_only_on_create_or_empty_upload() {
    let f = Fixture::new(vec![
        Reply::json(201, format!(r#"{{"id":"{ID}","chunkBytes":3}}"#)),
        Reply::json(200, r#"{"received":[],"chunkBytes":3}"#),
        Reply::json(204, ""),
        Reply::json(200, RECEIPT),
        Reply::json(200, RECEIPT),
    ])
    .await;
    let c = f.client();
    let d = LinkOrigin::parse("https://links.example").unwrap();
    let s = c.start("a", 1, Some(&d)).await.unwrap();
    assert!(matches!(
        c.status(s.id).await.unwrap(),
        UploadStatus::Pending { .. }
    ));
    c.put_piece(s.id, 0, Bytes::from_static(b"a"))
        .await
        .unwrap();
    c.complete(s.id).await.unwrap();
    c.upload_empty("a", Some(&d)).await.unwrap();
    for (i, r) in f.log().iter().enumerate() {
        assert_eq!(
            r.headers.contains("x-vectr-domain: https://links.example"),
            i == 0 || i == 4
        );
        for h in [
            "x-vectr-convert:",
            "x-vectr-expires:",
            "x-vectr-keep-name:",
            "x-vectr-purpose:",
        ] {
            assert!(!r.headers.contains(h));
        }
    }
}
#[tokio::test]
async fn rejected_domain_does_not_fallback_or_duplicate() {
    let f = Fixture::new(vec![Reply::json(400, "private-body-canary")]).await;
    let e = f
        .client()
        .start(
            "a",
            1,
            Some(&LinkOrigin::parse("https://links.example").unwrap()),
        )
        .await
        .unwrap_err();
    assert!(matches!(e, AppError::HttpStatus { code: 400, .. }));
    assert_eq!(f.log().len(), 1);
    assert!(!format!("{e:?} {e}").contains("private-body-canary"));
}
#[tokio::test]
async fn token_never_sent_to_link_domain() {
    let target = Fixture::new(vec![Reply::json(200, RECEIPT)]).await;
    let f = Fixture::new(vec![Reply::json(
        200,
        format!(
            r#"{{"url":"{}/secret","name":"a"}}"#,
            target.origin.as_str()
        ),
    )])
    .await;
    let r = f.client().upload_empty("a", None).await.unwrap();
    assert!(target.log().is_empty());
    assert!(!format!("{r:?}").contains("/secret"));
}
#[test]
fn token_is_bounded_validated_and_redacted() {
    for v in ["", "a\r\nb", " a", "a "] {
        assert!(UploadToken::new(v.into()).is_err());
    }
    assert!(
        !format!("{:?}", UploadToken::new("secret-canary".into()).unwrap())
            .contains("secret-canary")
    );
}
#[tokio::test]
async fn streamed_unknown_length_bodies_are_bounded() {
    let f = Fixture::new(vec![Reply::json(200, vec![b'x'; 1048577]).streamed()]).await;
    assert!(matches!(
        f.client().complete(ID.parse().unwrap()).await,
        Err(AppError::Protocol(_))
    ));
}
#[tokio::test]
async fn receipt_debug_cannot_echo_a_token_or_full_link_through_remote_name() {
    let f=Fixture::new(vec![Reply::json(200,r#"{"url":"https://links.example/private-canary","name":"https://links.example/secret-canary"}"#)]).await;
    let receipt = f.client().upload_empty("empty", None).await.unwrap();
    let debug = format!("{receipt:?}");
    assert!(!debug.contains("secret-canary"));
    assert!(!debug.contains("private-canary"));
}
#[tokio::test]
async fn refused_loopback_connection_is_transient_without_downgrading_tls() {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    let c = VectrClient::new(
        vectrcap::persistence::settings::ServerOrigin::parse(&format!("https://{address}"))
            .unwrap(),
        UploadToken::new("synthetic".into()).unwrap(),
    )
    .unwrap();
    assert_eq!(c.status(ID.parse().unwrap()).await, Err(AppError::Network));
}
