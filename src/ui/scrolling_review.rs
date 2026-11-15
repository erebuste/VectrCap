use crate::{app::error::AppError, scrolling::session::ScrollReview};
/// Partial status survives acceptance and all failed output actions.
pub struct ScrollingReview {
    pub editor: super::image_review::ImageReview,
    reason: Option<&'static str>,
    accepted: bool,
    pub segments: u32,
}
impl ScrollingReview {
    pub fn new(review: ScrollReview) -> Result<Self, AppError> {
        Ok(Self {
            editor: super::image_review::ImageReview::from_image(std::sync::Arc::new(
                review.image,
            ))?,
            reason: review.partial_reason,
            accepted: review.partial_reason.is_none(),
            segments: review.accepted_segments,
        })
    }
    pub fn partial_reason(&self) -> Option<&'static str> {
        self.reason
    }
    pub fn accepted(&self) -> bool {
        self.accepted
    }
    pub fn accept_partial(&mut self) {
        self.accepted = true;
    }
}

#[derive(Clone)]
pub struct ScrollReviewStatus {
    pub reason: Option<&'static str>,
    pub accepted: bool,
    pub segments: u32,
}
impl ScrollingReview {
    pub fn into_parts(self) -> (super::capture_overlay::CaptureOverlay, ScrollReviewStatus) {
        (
            self.editor.into_editor(),
            ScrollReviewStatus {
                reason: self.reason,
                accepted: self.accepted,
                segments: self.segments,
            },
        )
    }
}
