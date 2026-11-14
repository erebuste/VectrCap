use crate::{
    app::{destinations::CredentialKey, error::AppError},
    platform::CredentialStore,
    vectr::client::UploadToken,
};
use zeroize::Zeroizing;

pub const MAX_BLOB_BYTES: usize = 2560;
const OWNER: &str = "VectrCap/raw-ascii/v1";
const NAMESPACE: &str = "VectrCap/credentials/v1";
// Win32 has no create-if-absent credential operation. Serialize the check/write
// and check/delete pairs across adapter instances in this process. Repository
// leases and the application's single-owner controller exclude peer writers.
static MUTATIONS: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn validate_token(token: &UploadToken) -> Result<(), AppError> {
    if token.encoded().len() > MAX_BLOB_BYTES {
        return Err(AppError::Credential(
            "token exceeds protected storage capacity",
        ));
    }
    Ok(())
}

fn target(key: &CredentialKey) -> Result<String, AppError> {
    if key.destination().0.is_nil() || key.generation() == 0 {
        return Err(AppError::Credential("invalid credential binding"));
    }
    // Lossless hex encoding also avoids collisions in Windows' case-insensitive
    // target namespace. Names/defaults never participate in token identity.
    use std::fmt::Write;
    let mut encoded = String::new();
    for b in key.origin().as_str().bytes() {
        write!(&mut encoded, "{b:02x}").expect("String write");
    }
    Ok(format!(
        "{NAMESPACE}/{}/{}/{encoded}",
        key.destination().0,
        key.generation()
    ))
}

pub struct CredentialRecord {
    pub target: String,
    pub owner: String,
    pub kind: u32,
    pub persistence: u32,
    pub flags: u32,
    pub attributes: u32,
    pub blob: Zeroizing<Vec<u8>>,
}
impl std::fmt::Debug for CredentialRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialRecord(<redacted>)")
    }
}
fn owned(record: &CredentialRecord, target: &str) -> Result<(), AppError> {
    if record.target != target
        || record.owner != OWNER
        || record.kind != 1
        || record.persistence != 2
        || record.flags != 0
        || record.attributes != 0
    {
        return Err(AppError::Credential(
            "credential metadata does not match its binding",
        ));
    }
    Ok(())
}
pub trait CredentialBackend: Send + Sync {
    fn read(&self, target: &str) -> Result<Option<CredentialRecord>, AppError>;
    fn write(&self, record: &CredentialRecord) -> Result<(), AppError>;
    fn delete(&self, target: &str) -> Result<(), AppError>;
}
pub struct WindowsCredentialStore<B> {
    backend: B,
}
impl<B: CredentialBackend> WindowsCredentialStore<B> {
    pub fn with_backend(backend: B) -> Self {
        Self { backend }
    }
}
impl<B: CredentialBackend> CredentialStore for WindowsCredentialStore<B> {
    fn read(&self, key: &CredentialKey) -> Result<UploadToken, AppError> {
        let target = target(key)?;
        let r = self
            .backend
            .read(&target)?
            .ok_or(AppError::NeedsSetup("destination upload token is missing"))?;
        owned(&r, &target)?;
        if r.blob.is_empty()
            || r.blob.len() > MAX_BLOB_BYTES
            || !r.blob.iter().all(|b| (0x21..=0x7e).contains(b))
        {
            return Err(AppError::Credential("stored upload token is invalid"));
        }
        // Validate before allocating a String; all copies are zeroizing owners.
        let value = std::str::from_utf8(&r.blob)
            .map_err(|_| AppError::Credential("stored upload token is invalid"))?;
        UploadToken::new(value.to_owned())
    }
    fn write(&self, key: &CredentialKey, token: &UploadToken) -> Result<(), AppError> {
        validate_token(token)?;
        let target = target(key)?;
        let _guard = MUTATIONS
            .lock()
            .map_err(|_| AppError::Credential("protected credential mutation is unavailable"))?;
        if self.backend.read(&target)?.is_some() {
            return Err(AppError::Credential("credential generation already exists"));
        }
        self.backend.write(&CredentialRecord {
            target,
            owner: OWNER.into(),
            kind: 1,
            persistence: 2,
            flags: 0,
            attributes: 0,
            blob: Zeroizing::new(token.encoded().to_vec()),
        })
    }
    fn delete(&self, key: &CredentialKey) -> Result<(), AppError> {
        let target = target(key)?;
        let _guard = MUTATIONS
            .lock()
            .map_err(|_| AppError::Credential("protected credential mutation is unavailable"))?;
        if let Some(r) = self.backend.read(&target)? {
            owned(&r, &target)?;
            self.backend.delete(&target)?;
        }
        Ok(())
    }
}

pub struct Win32CredentialBackend;
impl CredentialBackend for Win32CredentialBackend {
    fn read(&self, target: &str) -> Result<Option<CredentialRecord>, AppError> {
        native::read(target)
    }
    fn write(&self, record: &CredentialRecord) -> Result<(), AppError> {
        native::write(record)
    }
    fn delete(&self, target: &str) -> Result<(), AppError> {
        native::delete(target)
    }
}

// Only this module crosses the documented Win32 allocation boundary. The
// injectable backend above lets fault tests exercise policy without Win32.
#[allow(unsafe_code)]
mod native {
    use super::*;
    use std::ptr::{self, NonNull};
    use windows_sys::Win32::{
        Foundation::{ERROR_NOT_FOUND, GetLastError},
        Security::Credentials::*,
    };
    use zeroize::Zeroize;
    const _: () = assert!(CRED_MAX_CREDENTIAL_BLOB_SIZE as usize == MAX_BLOB_BYTES);
    const INVALID: AppError = AppError::Credential("protected credential data is invalid");

    fn wide(target: &str) -> Result<Vec<u16>, AppError> {
        if !(target.starts_with(&format!("{NAMESPACE}/"))
            || target
                .strip_prefix("VectrCap/TEST/")
                .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok()))
            || target.contains('\0')
            || target.encode_utf16().count() > 32767
        {
            return Err(INVALID);
        }
        Ok(target.encode_utf16().chain(Some(0)).collect())
    }
    struct ReadBuffer(NonNull<CREDENTIALW>);
    impl Drop for ReadBuffer {
        fn drop(&mut self) {
            // SAFETY: CredReadW returned this single live allocation. Its blob
            // is writable per CREDENTIALW, valid until CredFree, and exclusively
            // owned here. Refuse unexpected lengths instead of forming a slice.
            unsafe {
                let r = self.0.as_ref();
                let len = r.CredentialBlobSize as usize;
                if len <= MAX_BLOB_BYTES
                    && len > 0
                    && !r.CredentialBlob.is_null()
                    && (r.CredentialBlob as usize).checked_add(len).is_some()
                {
                    std::slice::from_raw_parts_mut(r.CredentialBlob, len).zeroize();
                }
                CredFree(self.0.as_ptr().cast());
            }
        }
    }
    /// SAFETY: caller supplies a Win32-returned NUL-terminated string, live
    /// through its owning ReadBuffer. The bound is the documented field limit.
    unsafe fn text(pointer: *const u16, max: usize) -> Result<String, AppError> {
        if pointer.is_null() {
            return Ok(String::new());
        }
        for len in 0..=max {
            // SAFETY: Win32 promises a NUL-terminated field; the maximum bound
            // prevents unbounded scanning of malformed metadata.
            if unsafe { *pointer.add(len) } == 0 {
                return String::from_utf16(unsafe { std::slice::from_raw_parts(pointer, len) })
                    .map_err(|_| INVALID);
            }
        }
        Err(INVALID)
    }
    pub(super) fn read(target: &str) -> Result<Option<CredentialRecord>, AppError> {
        let name = wide(target)?;
        let mut raw = ptr::null_mut();
        // SAFETY: name is NUL terminated and alive; raw is a valid out pointer.
        if unsafe { CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut raw) } == 0 {
            return if unsafe { GetLastError() } == ERROR_NOT_FOUND {
                Ok(None)
            } else {
                Err(AppError::Credential("protected credential read failed"))
            };
        }
        let buffer = ReadBuffer(NonNull::new(raw).ok_or(INVALID)?);
        // SAFETY: buffer owns the API allocation and every embedded pointer.
        let r = unsafe { buffer.0.as_ref() };
        let len = r.CredentialBlobSize as usize;
        if len > MAX_BLOB_BYTES
            || (len > 0 && r.CredentialBlob.is_null())
            || (r.CredentialBlob as usize).checked_add(len).is_none()
        {
            return Err(INVALID);
        }
        // SAFETY: metadata strings are documented NUL-terminated pointers in
        // this allocation; no reference escapes the owning buffer's lifetime.
        let (name, owner, user, alias) = unsafe {
            (
                text(r.TargetName, 32767)?,
                text(r.Comment, 256)?,
                text(r.UserName, 513)?,
                text(r.TargetAlias, 256)?,
            )
        };
        if !user.is_empty() || !alias.is_empty() {
            return Err(INVALID);
        }
        let blob = if len == 0 {
            Vec::new()
        } else {
            // SAFETY: validated nonnull/length/overflow above; allocation and
            // blob length are furnished by CredReadW, not a caller pointer.
            unsafe { std::slice::from_raw_parts(r.CredentialBlob, len) }.to_vec()
        };
        Ok(Some(CredentialRecord {
            target: name,
            owner,
            kind: r.Type,
            persistence: r.Persist,
            flags: r.Flags,
            attributes: r.AttributeCount,
            blob: Zeroizing::new(blob),
        }))
    }
    pub(super) fn write(record: &CredentialRecord) -> Result<(), AppError> {
        let mut name = wide(&record.target)?;
        owned(record, &record.target)?;
        if record.blob.is_empty()
            || record.blob.len() > MAX_BLOB_BYTES
            || !record.blob.iter().all(|b| (0x21..=0x7e).contains(b))
        {
            return Err(INVALID);
        }
        let mut owner: Vec<u16> = OWNER.encode_utf16().chain(Some(0)).collect();
        let mut blob = Zeroizing::new(record.blob.to_vec());
        let r = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: name.as_mut_ptr(),
            Comment: owner.as_mut_ptr(),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        // SAFETY: all pointers refer to owned, correctly encoded buffers live
        // for the entire synchronous call. Flags=0; no impersonation/admin.
        if unsafe { CredWriteW(&r, 0) } == 0 {
            return Err(AppError::Credential("protected credential write failed"));
        }
        Ok(())
    }
    pub(super) fn delete(target: &str) -> Result<(), AppError> {
        let name = wide(target)?;
        // SAFETY: exact NUL-terminated name, generic type only, flags reserved=0.
        if unsafe { CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0
            && unsafe { GetLastError() } != ERROR_NOT_FOUND
        {
            return Err(AppError::Credential("protected credential deletion failed"));
        }
        Ok(())
    }
}
impl WindowsCredentialStore<Win32CredentialBackend> {
    pub fn current_user() -> Self {
        Self::with_backend(Win32CredentialBackend)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "explicit synthetic-only current-user Windows native smoke"]
    fn native_synthetic_test_namespace_roundtrip() {
        let target = format!("VectrCap/TEST/{}", uuid::Uuid::new_v4());
        let backend = Win32CredentialBackend;
        assert!(backend.read(&target).unwrap().is_none());
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = Win32CredentialBackend.delete(&self.0);
            }
        }
        let cleanup = Cleanup(target.clone());
        let record = CredentialRecord {
            target: target.clone(),
            owner: OWNER.into(),
            kind: 1,
            persistence: 2,
            flags: 0,
            attributes: 0,
            blob: Zeroizing::new(b"synthetic-native-smoke".to_vec()),
        };
        backend.write(&record).unwrap();
        let read = backend.read(&target).unwrap().unwrap();
        owned(&read, &target).unwrap();
        assert_eq!(read.blob.as_slice(), b"synthetic-native-smoke");
        backend.delete(&target).unwrap();
        assert!(backend.read(&target).unwrap().is_none());
        drop(cleanup);
    }
}
