use super::{
    protocol::{self, MAX_BODY, MAX_CHUNK, MAX_PIECES, ReceiptDto, UploadSession, UploadStatus},
    uploader_info::{UploaderInfoSupport, ValidatedUploaderInfo},
};
use crate::{
    app::{error::AppError, types::UploadReceipt},
    persistence::settings::{LinkOrigin, ServerOrigin},
};
use bytes::Bytes;
use reqwest::{
    Method, RequestBuilder, Response,
    header::{AUTHORIZATION, CACHE_CONTROL, HeaderValue},
};
use serde::de::DeserializeOwned;
use std::{
    error::Error as _,
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Clone, Copy, Default)]
pub(crate) struct BodySnapshot {
    pub bytes: u64,
    pub first: Option<tokio::time::Instant>,
    pub finished: Option<tokio::time::Instant>,
}

/// Cumulative outgoing-body meter. Timestamps are taken at body-frame polling,
/// so DNS/connect time and waiting for the response are never transfer time.
pub(crate) struct BodyMeter {
    expected: u64,
    state: Mutex<BodySnapshot>,
}
impl BodyMeter {
    pub fn new(expected: u64) -> Self {
        Self {
            expected,
            state: Mutex::new(BodySnapshot::default()),
        }
    }
    pub fn record(&self, bytes: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = tokio::time::Instant::now();
        state.first.get_or_insert(now);
        state.bytes += bytes;
        if state.bytes == self.expected {
            state.finished = Some(now);
        }
    }
    pub fn snapshot(&self) -> BodySnapshot {
        *self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Never serialized. The source buffer is wiped on drop; HTTP libraries own
/// additional header copies which are marked sensitive.
/// The 8192-byte ceiling validates transport only. Destination setup/import must
/// separately enforce the secure-store adapter's encoded-byte capacity before
/// installation (Windows CredentialBlobSize: 2560 bytes). Encoding is the
/// adapter's contract; this type alone does not prove a token can be persisted.
pub struct UploadToken(Zeroizing<String>);
impl fmt::Debug for UploadToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UploadToken(<redacted>)")
    }
}
impl UploadToken {
    pub fn new(value: String) -> Result<Self, AppError> {
        let value = Zeroizing::new(value);
        if value.is_empty()
            || value.len() > 8192
            || !value.bytes().all(|b| (0x21..=0x7e).contains(&b))
        {
            return Err(AppError::Credential("invalid upload token"));
        }
        Ok(Self(value))
    }
    fn header(&self) -> Result<HeaderValue, AppError> {
        let mut value = HeaderValue::from_str(&self.0)
            .map_err(|_| AppError::Credential("invalid upload token"))?;
        value.set_sensitive(true);
        Ok(value)
    }
}
/// Exactly one origin and token. No external request builder, cookie store,
/// proxy credentials, automatic redirects or library-level retries.
pub struct VectrClient {
    origin: ServerOrigin,
    token: UploadToken,
    http: reqwest::Client,
}
impl VectrClient {
    pub fn new(origin: ServerOrigin, token: UploadToken) -> Result<Self, AppError> {
        Self::build(origin, token, None)
    }
    /// Explicit client-scoped trust for private CAs. Never changes OS trust or
    /// disables verification. Fixture construction is the only current caller.
    pub fn with_root_certificate(
        origin: ServerOrigin,
        token: UploadToken,
        root: reqwest::Certificate,
    ) -> Result<Self, AppError> {
        Self::build(origin, token, Some(root))
    }
    fn build(
        origin: ServerOrigin,
        token: UploadToken,
        root: Option<reqwest::Certificate>,
    ) -> Result<Self, AppError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut builder = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .referer(false)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(300));
        if let Some(root) = root {
            builder = builder.tls_certs_only([root]);
        }
        let http = builder.build().map_err(|_| AppError::Tls)?;
        Ok(Self {
            origin,
            token,
            http,
        })
    }
    pub fn origin(&self) -> &ServerOrigin {
        &self.origin
    }
    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder, AppError> {
        Ok(self
            .http
            .request(method, format!("{}{path}", self.origin.as_str()))
            .header(AUTHORIZATION, self.token.header()?))
    }
    fn domain(request: RequestBuilder, domain: Option<&LinkOrigin>) -> RequestBuilder {
        if let Some(domain) = domain {
            request.header("X-Vectr-Domain", domain.as_str())
        } else {
            request
        }
    }
    pub async fn start(
        &self,
        filename: &str,
        size: u64,
        domain: Option<&LinkOrigin>,
    ) -> Result<UploadSession, AppError> {
        validate_filename(filename)?;
        if size == 0 || size > i64::MAX as u64 {
            return Err(AppError::InvalidInput("invalid upload size"));
        }
        let request = Self::domain(self.request(Method::POST, "/api/uploads")?, domain)
            .json(&serde_json::json!({"filename":filename,"size":size}));
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Start {
            id: Uuid,
            chunk_bytes: u64,
        }
        let dto: Start = decode(self.send(request, 201).await?)?;
        if dto.id.is_nil() {
            return Err(AppError::Protocol("invalid session identifier"));
        }
        protocol::piece_count(size, dto.chunk_bytes)?;
        Ok(UploadSession {
            id: dto.id,
            chunk_bytes: dto.chunk_bytes,
        })
    }
    pub async fn status(&self, id: Uuid) -> Result<UploadStatus, AppError> {
        validate_id(id)?;
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Status {
            chunk_bytes: u64,
            received: Vec<u32>,
            done: Option<ReceiptDto>,
        }
        let dto: Status = decode(
            self.send(
                self.request(Method::GET, &format!("/api/uploads/{id}"))?,
                200,
            )
            .await?,
        )?;
        protocol::validate_chunk(dto.chunk_bytes)?;
        protocol::validate_received(&dto.received, MAX_PIECES)?;
        if let Some(done) = dto.done {
            if !dto.received.is_empty() {
                return Err(AppError::Protocol("completed status contains pieces"));
            }
            Ok(UploadStatus::Done(done.validate()?))
        } else {
            Ok(UploadStatus::Pending {
                received: dto.received,
                chunk_bytes: dto.chunk_bytes,
            })
        }
    }
    pub async fn put_piece(&self, id: Uuid, index: u32, bytes: Bytes) -> Result<(), AppError> {
        let meter = Arc::new(BodyMeter::new(bytes.len() as u64));
        self.put_piece_observed(id, index, bytes, meter).await
    }
    /// Counts bounded body frames handed to the HTTP transport, including retries
    /// and failures. This is outgoing payload, never server-accepted progress.
    pub(crate) async fn put_piece_observed(
        &self,
        id: Uuid,
        index: u32,
        bytes: Bytes,
        meter: Arc<BodyMeter>,
    ) -> Result<(), AppError> {
        validate_id(id)?;
        if index >= MAX_PIECES || bytes.is_empty() || bytes.len() as u64 > MAX_CHUNK {
            return Err(AppError::InvalidInput("invalid upload piece"));
        }
        let len = bytes.len();
        let stream =
            futures_util::stream::unfold((bytes, 0, meter), |(bytes, offset, meter)| async move {
                if offset == bytes.len() {
                    return None;
                }
                let end = bytes.len().min(offset + 65536);
                let frame = bytes.slice(offset..end);
                meter.record(frame.len() as u64);
                Some((Ok::<_, std::io::Error>(frame), (bytes, end, meter)))
            });
        self.send(
            self.request(Method::PUT, &format!("/api/uploads/{id}/{index}"))?
                .header(reqwest::header::CONTENT_LENGTH, len)
                .body(reqwest::Body::wrap_stream(stream)),
            204,
        )
        .await?;
        Ok(())
    }
    pub async fn complete(&self, id: Uuid) -> Result<UploadReceipt, AppError> {
        validate_id(id)?;
        decode::<ReceiptDto>(
            self.send(
                self.request(Method::POST, &format!("/api/uploads/{id}/complete"))?,
                200,
            )
            .await?,
        )?
        .validate()
    }
    pub async fn upload_empty(
        &self,
        name: &str,
        domain: Option<&LinkOrigin>,
    ) -> Result<UploadReceipt, AppError> {
        validate_filename(name)?;
        let form = reqwest::multipart::Form::new().part(
            "file",
            reqwest::multipart::Part::bytes(Vec::new()).file_name(name.to_owned()),
        );
        decode::<ReceiptDto>(
            self.send(
                Self::domain(self.request(Method::POST, "/api/upload")?, domain).multipart(form),
                200,
            )
            .await?,
        )?
        .validate()
    }
    pub async fn uploader_info(&self) -> Result<UploaderInfoSupport, AppError> {
        match self
            .send(
                self.request(Method::GET, "/api/uploader")?
                    .header(CACHE_CONTROL, "no-store"),
                200,
            )
            .await
        {
            Ok(body) => Ok(UploaderInfoSupport::Available(
                ValidatedUploaderInfo::parse(&body)?,
            )),
            Err(AppError::HttpStatus { code: 404, .. }) => Ok(UploaderInfoSupport::Unsupported404),
            Err(e) => Err(e),
        }
    }
    pub async fn metadata_for(
        &self,
        key: super::uploader_info::MetadataKey,
    ) -> Result<super::uploader_info::CorrelatedMetadata, AppError> {
        if key.origin != self.origin {
            return Err(AppError::InvalidInput(
                "metadata origin does not match destination",
            ));
        }
        let info = self.uploader_info().await?;
        Ok(super::uploader_info::CorrelatedMetadata { key, info })
    }
    async fn send(&self, request: RequestBuilder, expected: u16) -> Result<Vec<u8>, AppError> {
        let response = request.send().await.map_err(transport_error)?;
        let code = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| retry_after(s, SystemTime::now()));
        let body = bounded_body(response).await?;
        if code != expected {
            return Err(AppError::HttpStatus { code, retry_after });
        }
        Ok(body)
    }
}
fn validate_id(id: Uuid) -> Result<(), AppError> {
    if id.is_nil() {
        Err(AppError::InvalidInput("invalid session identifier"))
    } else {
        Ok(())
    }
}
fn validate_filename(name: &str) -> Result<(), AppError> {
    if name.is_empty()
        || name.len() > 1024
        || name.chars().count() > 255
        || name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return Err(AppError::InvalidInput("invalid upload filename"));
    }
    Ok(())
}
fn decode<T: DeserializeOwned>(body: Vec<u8>) -> Result<T, AppError> {
    serde_json::from_slice(&body).map_err(|_| AppError::Protocol("malformed response"))
}
async fn bounded_body(mut response: Response) -> Result<Vec<u8>, AppError> {
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        return Err(AppError::Protocol("response exceeds compatibility limit"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        if chunk.len() > MAX_BODY - body.len() {
            return Err(AppError::Protocol("response exceeds compatibility limit"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
fn transport_error(error: reqwest::Error) -> AppError {
    let mut source = error.source();
    let mut known_network = false;
    while let Some(e) = source {
        if e.is::<rustls::Error>() {
            return AppError::Tls;
        }
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            known_network |= matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::HostUnreachable
                    | std::io::ErrorKind::NetworkUnreachable
            );
        }
        source = e.source();
    }
    // Unknown connector failures are conservatively non-retryable: platform
    // verifier errors need not expose rustls::Error in their source chain.
    if error.is_connect() && !error.is_timeout() && !known_network {
        AppError::Tls
    } else {
        AppError::Network
    }
}
pub fn retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    value
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        .or_else(|| {
            httpdate::parse_http_date(value)
                .ok()
                .map(|date| date.duration_since(now).unwrap_or_default())
        })
}
