use crate::app::{error::AppError, types::UploadReceipt};
use uuid::Uuid;

pub const MAX_BODY: usize = 1024 * 1024;
pub const MAX_CHUNK: u64 = 64 * 1024 * 1024;
pub const MAX_PIECES: u32 = 100_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadSession {
    pub id: Uuid,
    pub chunk_bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadStatus {
    Pending {
        received: Vec<u32>,
        chunk_bytes: u64,
    },
    Done(UploadReceipt),
}

pub fn piece_count(size: u64, chunk: u64) -> Result<u32, AppError> {
    validate_chunk(chunk)?;
    if size == 0 {
        return Err(AppError::Protocol(
            "chunked upload requires nonempty source",
        ));
    }
    let count = (size - 1) / chunk + 1;
    if count > u64::from(MAX_PIECES) {
        return Err(AppError::Protocol(
            "piece count exceeds compatibility limit",
        ));
    }
    Ok(count as u32)
}
pub fn validate_chunk(chunk: u64) -> Result<(), AppError> {
    if chunk == 0 || chunk > MAX_CHUNK {
        return Err(AppError::Protocol("chunk size exceeds compatibility limit"));
    }
    Ok(())
}
pub fn validate_received(received: &[u32], count: u32) -> Result<(), AppError> {
    if received.len() > MAX_PIECES as usize
        || received.iter().any(|&i| i >= count)
        || received.windows(2).any(|p| p[0] >= p[1])
    {
        return Err(AppError::Protocol("invalid received piece list"));
    }
    Ok(())
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReceiptDto {
    pub url: String,
    pub name: String,
}
impl ReceiptDto {
    pub fn validate(self) -> Result<UploadReceipt, AppError> {
        UploadReceipt::new(&self.url, &self.name)
            .map_err(|_| AppError::Protocol("invalid upload receipt"))
    }
}
