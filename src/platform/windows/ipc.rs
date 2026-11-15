//! Bounded UTF-16 path frames and a per-logon local-only pipe.
use crate::app::{cli::*, error::AppError};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    os::windows::ffi::{OsStrExt, OsStringExt},
};
use uuid::Uuid;
pub const MAX_PAYLOAD: usize = 1024 * 1024;
const INVALID: AppError = AppError::InvalidInput("invalid path-only IPC frame");
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRequest {
    request_id: Uuid,
    request: WireKind,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum WireKind {
    UploadFiles { paths: Vec<Vec<u16>> },
    ImportDestination { path: Vec<u16> },
}
pub fn encode_frame(request: &LaunchRequest) -> Result<Vec<u8>, AppError> {
    request.validate()?;
    let kind = match &request.kind {
        LaunchRequestKind::UploadFiles { paths } => WireKind::UploadFiles {
            paths: paths.iter().map(|p| p.encode_wide().collect()).collect(),
        },
        LaunchRequestKind::ImportDestination { path } => WireKind::ImportDestination {
            path: path.encode_wide().collect(),
        },
    };
    let payload = serde_json::to_vec(&WireRequest {
        request_id: request.request_id,
        request: kind,
    })
    .map_err(|_| INVALID)?;
    if payload.len() > MAX_PAYLOAD {
        return Err(INVALID);
    }
    let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
    frame.extend(payload);
    Ok(frame)
}
pub fn decode_frame(frame: &[u8]) -> Result<LaunchRequest, AppError> {
    if frame.len() < 4 || frame.len() > MAX_PAYLOAD + 4 {
        return Err(INVALID);
    }
    let count = u32::from_le_bytes(frame[..4].try_into().map_err(|_| INVALID)?) as usize;
    if count != frame.len() - 4 {
        return Err(INVALID);
    }
    let wire: WireRequest = serde_json::from_slice(&frame[4..]).map_err(|_| INVALID)?;
    let kind = match wire.request {
        WireKind::UploadFiles { paths } => {
            if paths.len() > 100 {
                return Err(INVALID);
            }
            LaunchRequestKind::UploadFiles {
                paths: paths.iter().map(|p| OsString::from_wide(p)).collect(),
            }
        }
        WireKind::ImportDestination { path } => LaunchRequestKind::ImportDestination {
            path: OsString::from_wide(&path),
        },
    };
    let request = LaunchRequest {
        request_id: wire.request_id,
        kind,
    };
    request.validate()?;
    Ok(request)
}
pub struct PipePolicy {
    logon: String,
    session: u32,
}
impl PipePolicy {
    pub fn new(logon: &str, session: u32) -> Result<Self, AppError> {
        if !logon.starts_with("S-1-5-5-")
            || logon.len() > 100
            || !logon
                .bytes()
                .all(|b| b.is_ascii_digit() || b == b'-' || b == b'S')
        {
            return Err(INVALID);
        }
        Ok(Self {
            logon: logon.into(),
            session,
        })
    }
    pub fn instance_name(&self, namespace: &str) -> Result<String, AppError> {
        if namespace.is_empty()
            || namespace.len() > 80
            || !namespace
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(INVALID);
        }
        Ok(format!(
            "Local\\{namespace}-{}-{}",
            self.logon, self.session
        ))
    }
    pub fn sddl(&self) -> String {
        format!("D:P(D;;GA;;;NU)(A;;GA;;;{})", self.logon)
    }
    pub fn authorize(
        &self,
        remote: bool,
        session: u32,
        logon_member: bool,
    ) -> Result<(), AppError> {
        if remote || session != self.session || !logon_member {
            return Err(AppError::InvalidInput("foreign IPC client refused"));
        }
        Ok(())
    }
    fn name(&self) -> String {
        format!(r"\\.\pipe\VectrCap-v1-{}-{}", self.logon, self.session)
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum HandoffResult {
    ExitSecondary,
}
/// Every transport retry reuses the exact same request ID and immutable frame.
pub fn handoff_with_retry(
    request: &LaunchRequest,
    mut send: impl FnMut(&[u8]) -> Result<Uuid, AppError>,
) -> Result<HandoffResult, AppError> {
    let frame = encode_frame(request)?;
    let mut last = AppError::Network;
    for _ in 0..3 {
        match send(&frame) {
            Ok(ack) if ack == request.request_id => return Ok(HandoffResult::ExitSecondary),
            Ok(_) => return Err(INVALID),
            Err(error) => last = error,
        }
    }
    Err(last)
}

/// Shared framed transport for the native pipe and in-memory test adapters.
/// The OS adapter has already proved local logon membership with the DACL and
/// PIPE_REJECT_REMOTE_CLIENTS. Session identity is read from the pipe handle.
pub async fn receive_path_request<S, F, Fut>(
    stream: &mut S,
    policy: &PipePolicy,
    session: u32,
    accept: F,
) -> Result<(), AppError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    F: FnOnce(LaunchRequest) -> Fut,
    Fut: std::future::Future<Output = Result<Uuid, AppError>>,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    policy.authorize(false, session, true)?;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let count = stream.read_u32_le().await.map_err(|_| AppError::Network)? as usize;
        if count == 0 || count > MAX_PAYLOAD {
            return Err(INVALID);
        }
        let mut frame = vec![0; count + 4];
        frame[..4].copy_from_slice(&(count as u32).to_le_bytes());
        stream
            .read_exact(&mut frame[4..])
            .await
            .map_err(|_| AppError::Network)?;
        let request = decode_frame(&frame)?;
        let expected = request.request_id;
        let ack = accept(request).await?;
        if ack != expected {
            return Err(INVALID);
        }
        stream
            .write_all(ack.as_bytes())
            .await
            .map_err(|_| AppError::Network)?;
        Ok(())
    })
    .await
    .map_err(|_| AppError::Network)??;
    // Keep the server handle alive until the client consumes the ACK. Missing
    // confirmation cannot undo committed acceptance or mask its success.
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), stream.read_u8()).await;
    Ok(())
}

mod native {
    #![allow(unsafe_code)]
    use super::*;
    use std::{ptr, sync::Arc, time::Duration};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::windows::named_pipe::{ClientOptions, ServerOptions},
    };
    use tokio_util::sync::CancellationToken;
    use windows_sys::Win32::{
        Foundation::*,
        Security::{Authorization::*, *},
        System::Threading::*,
    };
    fn error() -> AppError {
        AppError::Persistence("secured instance pipe unavailable")
    }
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    pub fn current_policy() -> Result<PipePolicy, AppError> {
        unsafe {
            let mut token = ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(error());
            }
            let token = Handle(token);
            let mut count = 0;
            GetTokenInformation(token.0, TokenGroups, ptr::null_mut(), 0, &mut count);
            if count == 0 || count > 65536 {
                return Err(error());
            }
            let mut storage = vec![0usize; (count as usize).div_ceil(size_of::<usize>())];
            if GetTokenInformation(
                token.0,
                TokenGroups,
                storage.as_mut_ptr().cast(),
                count,
                &mut count,
            ) == 0
            {
                return Err(error());
            }
            let groups = &*storage.as_ptr().cast::<TOKEN_GROUPS>();
            let entries =
                std::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize);
            let group = entries
                .iter()
                .find(|g| g.Attributes & 0xc0000000 == 0xc0000000)
                .ok_or_else(error)?;
            let mut sid = ptr::null_mut();
            if ConvertSidToStringSidW(group.Sid, &mut sid) == 0 {
                return Err(error());
            }
            let mut len = 0;
            while *sid.add(len) != 0 {
                len += 1;
            }
            let logon =
                String::from_utf16(std::slice::from_raw_parts(sid, len)).map_err(|_| error());
            LocalFree(sid.cast());
            let mut session = 0;
            if windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId(
                GetCurrentProcessId(),
                &mut session,
            ) == 0
            {
                return Err(error());
            }
            PipePolicy::new(&logon?, session)
        }
    }
    struct Descriptor(PSECURITY_DESCRIPTOR);
    impl Drop for Descriptor {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
    pub struct PipeServer {
        stop: CancellationToken,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl PipeServer {
        pub fn start(
            accept: Arc<dyn Fn(LaunchRequest) -> Result<Uuid, AppError> + Send + Sync>,
        ) -> Result<Self, AppError> {
            let policy = current_policy()?;
            let stop = CancellationToken::new();
            let shutdown = stop.clone();
            let (ready, rx) = std::sync::mpsc::sync_channel(1);
            let thread = std::thread::Builder::new()
                .name("instance-ingress".into())
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build();
                    match runtime {
                        Ok(runtime) => runtime.block_on(serve(policy, accept, shutdown, ready)),
                        Err(_) => {
                            let _ = ready.send(Err(error()));
                        }
                    }
                })
                .map_err(|_| error())?;
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Ok(())) => Ok(Self {
                    stop,
                    thread: Some(thread),
                }),
                _ => {
                    stop.cancel();
                    let _ = thread.join();
                    Err(error())
                }
            }
        }
    }
    async fn serve(
        policy: PipePolicy,
        accept: Arc<dyn Fn(LaunchRequest) -> Result<Uuid, AppError> + Send + Sync>,
        shutdown: CancellationToken,
        ready: std::sync::mpsc::SyncSender<Result<(), AppError>>,
    ) {
        let sddl: Vec<u16> = policy.sddl().encode_utf16().chain([0]).collect();
        let mut descriptor = ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut(),
            )
        } == 0
        {
            let _ = ready.send(Err(error()));
            return;
        }
        let descriptor = Descriptor(descriptor);
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        // Keep the first instance handle for the service lifetime. Reusing it
        // avoids a namespace/activation gap between clients and prevents an
        // old client handle from colliding with first-instance creation.
        let server = unsafe {
            ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .max_instances(1)
                .in_buffer_size(65536)
                .out_buffer_size(4096)
                .create_with_security_attributes_raw(
                    policy.name(),
                    (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
                )
        };
        let Ok(mut server) = server else {
            let _ = ready.send(Err(error()));
            return;
        };
        let _ = ready.send(Ok(()));
        loop {
            tokio::select! {
                _=shutdown.cancelled()=>break,
                result=server.connect()=>if result.is_err(){eprintln!("secured instance pipe connection failed");return;}
            }
            use std::os::windows::io::AsRawHandle;
            let mut session = 0;
            if unsafe {
                windows_sys::Win32::System::Pipes::GetNamedPipeClientSessionId(
                    server.as_raw_handle().cast(),
                    &mut session,
                )
            } != 0
            {
                let callback = accept.clone();
                let _ = receive_path_request(
                    &mut server,
                    &policy,
                    session,
                    move |request| async move {
                        tokio::task::spawn_blocking(move || callback(request))
                            .await
                            .map_err(|_| error())?
                    },
                )
                .await;
            }
            if let Err(error) = server.disconnect()
                && error.raw_os_error() != Some(233)
            {
                eprintln!("secured instance pipe disconnect failed; restart required");
                return;
            }
        }
    }

    impl Drop for PipeServer {
        fn drop(&mut self) {
            self.stop.cancel();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
    pub fn send_request(request: &LaunchRequest) -> Result<HandoffResult, AppError> {
        let policy = current_policy()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| error())?;
        handoff_with_retry(request, |frame| {
            runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(4), async {
                    let mut pipe = loop {
                        match ClientOptions::new().open(policy.name()) {
                            Ok(pipe) => break pipe,
                            Err(e) if matches!(e.raw_os_error(), Some(2 | 231)) => {
                                tokio::time::sleep(Duration::from_millis(100)).await
                            }
                            Err(_) => return Err(error()),
                        }
                    };
                    pipe.write_all(frame).await.map_err(|_| error())?;
                    let mut ack = [0; 16];
                    pipe.read_exact(&mut ack).await.map_err(|_| error())?;
                    // ACK is authoritative; receipt-write failure is cosmetic.
                    let _ = pipe.write_u8(1).await;
                    Ok(Uuid::from_bytes(ack))
                })
                .await
                .map_err(|_| error())?
            })
        })
    }
}
pub use native::{PipeServer, current_policy, send_request};
