use super::{sampler::SampledColor, session::UtilityEffect};
use crate::app::error::AppError;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyFeedback {
    None,
    Copying,
    Copied,
    Error(AppError),
}
pub enum ColorResultInput {
    CopyHex,
    CopyRgb,
    PickAgain,
    Close,
    ClipboardOutcome(Result<(), AppError>),
}
pub struct ColorResultState {
    color: SampledColor,
    feedback: CopyFeedback,
}
impl ColorResultState {
    pub fn new(color: SampledColor) -> Self {
        Self {
            color,
            feedback: CopyFeedback::None,
        }
    }
    pub fn color(&self) -> SampledColor {
        self.color
    }
    pub fn feedback(&self) -> &CopyFeedback {
        &self.feedback
    }
    pub fn handle(&mut self, input: ColorResultInput) -> Vec<UtilityEffect> {
        match input {
            ColorResultInput::CopyHex | ColorResultInput::CopyRgb => {
                let text = if matches!(input, ColorResultInput::CopyHex) {
                    self.color.hex_text()
                } else {
                    self.color.rgb_text()
                };
                self.feedback = CopyFeedback::Copying;
                vec![UtilityEffect::CopyText(text)]
            }
            ColorResultInput::PickAgain => vec![UtilityEffect::RefreshSource],
            ColorResultInput::Close => vec![UtilityEffect::Close],
            ColorResultInput::ClipboardOutcome(outcome) => {
                self.feedback = outcome.map_or_else(CopyFeedback::Error, |()| CopyFeedback::Copied);
                vec![]
            }
        }
    }
}
