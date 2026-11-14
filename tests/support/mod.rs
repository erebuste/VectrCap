#![allow(dead_code)]
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{ServerConfig, pki_types::PrivatePkcs8KeyDer},
};
use vectrcap::{
    persistence::settings::ServerOrigin,
    vectr::client::{UploadToken, VectrClient},
};

pub const ID: &str = "59c8493c-801b-408a-a332-efeb703520ff";
pub const RECEIPT: &str = r#"{"url":"https://links.example/private-canary","name":"remote.webp"}"#;
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: String,
    pub body: Vec<u8>,
}
pub struct Reply {
    pub code: u16,
    pub headers: String,
    pub body: Vec<u8>,
    pub drop: bool,
    pub barrier: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Semaphore>)>,
    pub streamed: bool,
}
impl Reply {
    pub fn json(code: u16, body: impl AsRef<[u8]>) -> Self {
        Self {
            code,
            headers: "Cache-Control: no-store\r\n".into(),
            body: body.as_ref().to_vec(),
            drop: false,
            barrier: None,
            streamed: false,
        }
    }
    pub fn blocked(mut self) -> (Self, Arc<tokio::sync::Notify>, Arc<tokio::sync::Semaphore>) {
        let arrived = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        self.barrier = Some((arrived.clone(), release.clone()));
        (self, arrived, release)
    }
    pub fn streamed(mut self) -> Self {
        self.streamed = true;
        self
    }
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push_str(&format!("{name}: {value}\r\n"));
        self
    }
    pub fn lost() -> Self {
        Self {
            drop: true,
            ..Self::json(200, "")
        }
    }
}
pub struct Fixture {
    pub origin: ServerOrigin,
    pub root: reqwest::Certificate,
    pub requests: Arc<Mutex<Vec<Request>>>,
    task: JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    pub async fn new(replies: Vec<Reply>) -> Self {
        let ca_key = KeyPair::generate().unwrap();
        let mut ca = CertificateParams::new(vec![]).unwrap();
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca_cert = ca.self_signed(&ca_key).unwrap();
        let issuer = Issuer::from_params(&ca, &ca_key);
        let key = KeyPair::generate().unwrap();
        let cert = CertificateParams::new(vec!["127.0.0.1".into()])
            .unwrap()
            .signed_by(&key, &issuer)
            .unwrap();
        let provider = tokio_rustls::rustls::crypto::ring::default_provider();
        let config = ServerConfig::builder_with_provider(Arc::new(provider))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
            )
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = ServerOrigin::parse(&format!(
            "https://127.0.0.1:{}",
            listener.local_addr().unwrap().port()
        ))
        .unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let logged = requests.clone();
        let task = tokio::spawn(async move {
            for reply in replies {
                let (socket, _) = listener.accept().await.unwrap();
                let Ok(mut stream) = acceptor.accept(socket).await else {
                    continue;
                };
                let mut raw = Vec::new();
                let end = loop {
                    let mut b = [0];
                    if stream.read_exact(&mut b).await.is_err() {
                        return;
                    }
                    raw.push(b[0]);
                    if raw.ends_with(b"\r\n\r\n") {
                        break raw.len();
                    }
                    assert!(raw.len() < 65536);
                };
                let head = String::from_utf8(raw[..end].to_vec()).unwrap();
                let len: usize = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                assert!(len <= 64 * 1024 * 1024 + 65536);
                let mut body = vec![0; len];
                stream.read_exact(&mut body).await.unwrap();
                let mut first = head.split_whitespace();
                logged.lock().unwrap().push(Request {
                    method: first.next().unwrap().into(),
                    path: first.next().unwrap().into(),
                    headers: head.to_ascii_lowercase(),
                    body,
                });
                if reply.drop {
                    continue;
                }
                if let Some((arrived, release)) = reply.barrier {
                    arrived.notify_one();
                    let _permit = release.acquire().await.unwrap();
                }
                let length = if reply.streamed {
                    "Transfer-Encoding: chunked".into()
                } else {
                    format!("Content-Length: {}", reply.body.len())
                };
                let head = format!(
                    "HTTP/1.1 {} Fixture\r\n{}\r\nConnection: close\r\n{}\r\n",
                    reply.code, length, reply.headers
                );
                let _ = stream.write_all(head.as_bytes()).await;
                if reply.streamed {
                    for chunk in reply.body.chunks(65536) {
                        let _ = stream
                            .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                            .await;
                        let _ = stream.write_all(chunk).await;
                        let _ = stream.write_all(b"\r\n").await;
                    }
                    let _ = stream.write_all(b"0\r\n\r\n").await;
                } else {
                    let _ = stream.write_all(&reply.body).await;
                }
                let _ = stream.shutdown().await;
            }
        });
        Self {
            origin,
            root: reqwest::Certificate::from_der(ca_cert.der()).unwrap(),
            requests,
            task,
        }
    }
    pub fn client(&self) -> VectrClient {
        self.client_token("synthetic-token")
    }
    pub fn client_token(&self, token: &str) -> VectrClient {
        VectrClient::with_root_certificate(
            self.origin.clone(),
            UploadToken::new(token.into()).unwrap(),
            self.root.clone(),
        )
        .unwrap()
    }
    pub fn log(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}
