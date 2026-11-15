//! Submission-order results. Uncertain outcomes never authorize automatic copy.
use crate::app::error::AppError;
#[derive(Clone, PartialEq, Eq)]
pub enum BatchOutcome {
    Success(String),
    Failed,
    Cancelled,
    Uncertain,
}
pub struct UploadBatch {
    slots: Vec<Option<BatchOutcome>>,
    automatic: bool,
    copied: bool,
}
impl UploadBatch {
    pub fn new(count: usize, automatic: bool) -> Result<Self, AppError> {
        if !(1..=100).contains(&count) {
            return Err(AppError::InvalidInput("invalid batch size"));
        }
        Ok(Self {
            slots: vec![None; count],
            automatic,
            copied: false,
        })
    }
    pub fn resolve(&mut self, index: usize, outcome: BatchOutcome) -> Result<(), AppError> {
        let slot = self
            .slots
            .get_mut(index)
            .ok_or(AppError::InvalidInput("invalid batch slot"))?;
        if slot.is_some() {
            return Err(AppError::InvalidInput("batch slot already resolved"));
        }
        if let BatchOutcome::Success(url) = &outcome {
            crate::app::types::UploadReceipt::new(url, "batch")?;
        }
        *slot = Some(outcome);
        Ok(())
    }
    pub fn available_links(&self) -> String {
        self.slots
            .iter()
            .filter_map(|s| match s {
                Some(BatchOutcome::Success(url)) => Some(url.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    pub fn take_automatic_copy(&mut self) -> Option<String> {
        if !self.automatic
            || self.copied
            || self
                .slots
                .iter()
                .any(|s| s.is_none() || matches!(s, Some(BatchOutcome::Uncertain)))
        {
            return None;
        }
        self.copied = true;
        let text = self.available_links();
        (!text.is_empty()).then_some(text)
    }
    pub fn retryable_indices(&self) -> Vec<usize> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| matches!(s, Some(BatchOutcome::Failed)).then_some(i))
            .collect()
    }
}
