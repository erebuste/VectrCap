use crate::app::error::AppError;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
pub const RETRIES: u32 = 4;
pub fn eligible(error: &AppError) -> bool {
    matches!(
        error,
        AppError::Network
            | AppError::HttpStatus {
                code: 429 | 500..=599,
                ..
            }
    )
}
pub fn failure_code(error: &AppError) -> &'static str {
    match error {
        AppError::RetryExhausted => "retry_exhausted",
        AppError::HttpStatus { code: 404, .. } => "expired",
        AppError::HttpStatus {
            code: 401 | 403, ..
        }
        | AppError::Credential(_)
        | AppError::NeedsSetup(_) => "authentication",
        AppError::Tls => "tls",
        AppError::HttpStatus { code: 429, .. } => "rate_limited",
        e if eligible(e) => "transient",
        AppError::Protocol(_) => "protocol",
        _ => "validation",
    }
}
pub fn delay(failure: u32, error: &AppError) -> Duration {
    if let AppError::HttpStatus {
        code: 429,
        retry_after: Some(delay),
    } = error
    {
        return *delay;
    }
    let ceiling = (1u64 << failure.saturating_sub(1).min(5)).min(30) * 1000;
    Duration::from_millis(rand::random_range(0..=ceiling))
}
pub async fn wait(delay: Duration, cancel: &CancellationToken) -> Result<(), AppError> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(AppError::Cancelled),
        _ = tokio::time::sleep(delay) => Ok(()),
    }
}
