mod support;
use support::*;
use vectrcap::{
    app::{destinations::DestinationId, error::AppError},
    vectr::uploader_info::{MetadataKey, UploaderInfoSupport},
};
const INFO: &str = r#"{"name":"Phone","domains":["https://links.example","http://legacy.example"],"domain":"https://links.example","settings":{"expires":"1.5d","maxViews":12,"nameFormat":"random","convert":"off","keepName":false,"passwordProtected":true}}"#;
#[tokio::test]
async fn metadata_200_verifies_token_and_parses_safe_fields() {
    let f = Fixture::new(vec![Reply::json(200, INFO)]).await;
    let UploaderInfoSupport::Available(info) = f.client().uploader_info().await.unwrap() else {
        panic!("available expected")
    };
    assert_eq!(info.name, "Phone");
    assert!(matches!(
        info.domains[0],
        vectrcap::vectr::uploader_info::DomainChoice::Https(_)
    ));
    assert!(matches!(
        info.domains[1],
        vectrcap::vectr::uploader_info::DomainChoice::UnsupportedHttp(_)
    ));
    let r = &f.log()[0];
    assert_eq!(r.method, "GET");
    assert_eq!(r.path, "/api/uploader");
    assert!(r.headers.contains("authorization: synthetic-token"));
    assert!(!r.headers.contains("cookie:"));
    assert!(r.headers.contains("cache-control: no-store"));
}
#[tokio::test]
async fn metadata_expiry_matches_vectr_go_duration_and_day_grammar() {
    for value in [
        "never",
        "1h30m",
        "1.5d",
        "500ms",
        ".5s",
        "+1h",
        "0.123456789012345678901s",
    ] {
        let mut dto: serde_json::Value = serde_json::from_str(INFO).unwrap();
        dto["settings"]["expires"] = value.into();
        let f = Fixture::new(vec![Reply::json(200, dto.to_string())]).await;
        assert!(
            f.client().uploader_info().await.is_ok(),
            "valid expiry {value}"
        );
    }
    for value in [".5d", "1.d", "+1d", "-1h", "0h", "1h1d", "1d1h"] {
        let mut dto: serde_json::Value = serde_json::from_str(INFO).unwrap();
        dto["settings"]["expires"] = value.into();
        let f = Fixture::new(vec![Reply::json(200, dto.to_string())]).await;
        assert!(
            matches!(f.client().uploader_info().await, Err(AppError::Protocol(_))),
            "invalid expiry {value}"
        );
    }
}
#[tokio::test]
async fn metadata_404_only_compatibility_fallback() {
    let f = Fixture::new(vec![Reply::json(404, "old")]).await;
    assert_eq!(
        f.client().uploader_info().await.unwrap(),
        UploaderInfoSupport::Unsupported404
    );
    let f = Fixture::new(vec![Reply::json(403, "")]).await;
    assert!(matches!(
        f.client().uploader_info().await,
        Err(AppError::HttpStatus { code: 403, .. })
    ));
}
#[tokio::test]
async fn metadata_401_tls_and_malformed_never_downgrade() {
    for (code, body) in [(401, ""), (200, "bad"), (200, r#"{"name":"x"}"#)] {
        let f = Fixture::new(vec![Reply::json(code, body)]).await;
        assert!(matches!(
            f.client().uploader_info().await,
            Err(AppError::HttpStatus { code: 401, .. }) | Err(AppError::Protocol(_))
        ));
    }
    let f = Fixture::new(vec![Reply::json(200, INFO)]).await;
    let c = vectrcap::vectr::client::VectrClient::new(
        f.origin.clone(),
        vectrcap::vectr::client::UploadToken::new("synthetic".into()).unwrap(),
    )
    .unwrap();
    assert_eq!(c.uploader_info().await, Err(AppError::Tls));
}
#[tokio::test]
async fn metadata_bounds_domains_text_and_body() {
    let mut invalid = Vec::new();
    for (key, value) in [
        ("expires", serde_json::json!("-1h")),
        ("expires", serde_json::json!("999999999999999999999d")),
        ("maxViews", serde_json::json!(0)),
        ("nameFormat", serde_json::json!("unknown")),
        ("convert", serde_json::json!("exe")),
    ] {
        let mut v: serde_json::Value = serde_json::from_str(INFO).unwrap();
        v["settings"][key] = value;
        invalid.push(v.to_string());
    }
    let mut v: serde_json::Value = serde_json::from_str(INFO).unwrap();
    v["domains"] = serde_json::json!(vec!["https://a.example"; 257]);
    invalid.push(v.to_string());
    let mut v: serde_json::Value = serde_json::from_str(INFO).unwrap();
    v["name"] = serde_json::json!("x".repeat(101));
    invalid.push(v.to_string());
    let mut v: serde_json::Value = serde_json::from_str(INFO).unwrap();
    v["settings"].as_object_mut().unwrap().remove("maxViews");
    invalid.push(v.to_string());
    invalid.push("x".repeat(1048577));
    for b in invalid {
        let f = Fixture::new(vec![Reply::json(200, b)]).await;
        assert!(matches!(
            f.client().uploader_info().await,
            Err(AppError::Protocol(_))
        ));
    }
}
#[tokio::test]
async fn metadata_never_changes_policy_or_default_domain_choice() {
    let f = Fixture::new(vec![
        Reply::json(200, INFO),
        Reply::json(201, format!(r#"{{"id":"{ID}","chunkBytes":3}}"#)),
    ])
    .await;
    let c = f.client();
    c.uploader_info().await.unwrap();
    c.start("a", 1, None).await.unwrap();
    assert_eq!(f.log().len(), 2);
    for r in f.log() {
        assert!(!r.headers.contains("x-vectr-"));
    }
}
#[tokio::test]
async fn late_metadata_cannot_update_another_destination() {
    let f = Fixture::new(vec![Reply::json(200, INFO)]).await;
    let a = MetadataKey {
        destination: DestinationId::new_random(),
        origin: f.origin.clone(),
        revision: 1,
    };
    let response = f.client().metadata_for(a.clone()).await.unwrap();
    let mut b = a.clone();
    b.destination = DestinationId::new_random();
    assert!(response.clone().into_current(&b).is_none());
    b = a.clone();
    b.revision = 2;
    assert!(response.clone().into_current(&b).is_none());
    b = a.clone();
    b.origin =
        vectrcap::persistence::settings::ServerOrigin::parse("https://another.example").unwrap();
    assert!(response.clone().into_current(&b).is_none());
    assert!(response.into_current(&a).is_some());
}
#[tokio::test]
async fn metadata_debug_does_not_echo_token_shaped_uploader_name() {
    let mut dto: serde_json::Value = serde_json::from_str(INFO).unwrap();
    dto["name"] = "secret-canary".into();
    let f = Fixture::new(vec![Reply::json(200, dto.to_string())]).await;
    let info = f.client().uploader_info().await.unwrap();
    assert!(!format!("{info:?}").contains("secret-canary"));
}
