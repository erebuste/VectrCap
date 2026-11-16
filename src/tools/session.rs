use super::{
    UtilitySnapshot,
    color_result::{ColorResultInput, ColorResultState},
    ruler::{RulerState, measure_rect, measure_run},
    sampler::{SampledColor, SamplerState, sample_pixel},
};
use crate::{
    app::error::AppError,
    capture::geometry::{PhysicalPoint, PhysicalRect},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UtilityKind {
    Sampler,
    Ruler,
}
/// Exhaustive allowlist: there is deliberately no artifact, workflow or upload effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UtilityEffect {
    ShowColorResult(SampledColor),
    CopyText(String),
    RefreshSource,
    Close,
    Error(AppError),
}
pub enum UtilityInput {
    Pointer(PhysicalPoint),
    Move { dx: i32, dy: i32, shift: bool },
    Select,
    AddRect(PhysicalRect),
    ReplaceRect { index: usize, rect: PhysicalRect },
    SetAxis(super::ruler::Axis),
    ToggleAxis,
    Tolerance(i8),
    CopySelected,
    Clear,
    RemoveOrClose,
    Result(ColorResultInput),
    DisplayChanged,
    Locked,
    Close,
}
pub struct UtilitySession {
    snapshot: Option<UtilitySnapshot>,
    pub kind: UtilityKind,
    pub sampler: SamplerState,
    pub ruler: RulerState,
    pub result: Option<ColorResultState>,
    closed: bool,
    pub error: Option<AppError>,
}
impl UtilitySession {
    pub fn new(kind: UtilityKind, snapshot: UtilitySnapshot, point: PhysicalPoint) -> Self {
        Self {
            snapshot: Some(snapshot),
            kind,
            sampler: SamplerState::new(point),
            ruler: RulerState::default(),
            result: None,
            closed: false,
            error: None,
        }
    }
    pub fn snapshot(&self) -> Option<&UtilitySnapshot> {
        self.snapshot.as_ref()
    }
    pub fn close(&mut self) {
        self.snapshot = None;
        self.result = None;
        self.ruler.clear();
        self.closed = true;
    }
    fn validate_measurement(&self, rect: PhysicalRect) -> Result<(), AppError> {
        let source = self
            .snapshot
            .as_ref()
            .ok_or(AppError::Capture("utility source unavailable"))?;
        if !source.bounds().contains_rect(rect) {
            return Err(AppError::InvalidInput("measurement exceeds frozen source"));
        }
        Ok(())
    }
    pub fn handle(&mut self, input: UtilityInput) -> Vec<UtilityEffect> {
        if self.closed {
            return vec![];
        }
        self.error = None;
        let outcome = (|| -> Result<Vec<UtilityEffect>, AppError> {
            match input {
                UtilityInput::Close | UtilityInput::Locked | UtilityInput::DisplayChanged => {
                    self.close();
                    Ok(vec![UtilityEffect::Close])
                }
                UtilityInput::Result(input) => Ok(self
                    .result
                    .as_mut()
                    .map_or_else(Vec::new, |r| r.handle(input))),
                _ if self.result.is_some() => Ok(vec![]),
                UtilityInput::Pointer(p) => {
                    self.sampler.point = p;
                    Ok(vec![])
                }
                UtilityInput::Move { dx, dy, shift } => {
                    if self.kind == UtilityKind::Ruler && self.ruler.selected().is_some() {
                        let current = self
                            .ruler
                            .selected()
                            .ok_or(AppError::InvalidInput("select a measurement"))?;
                        let step = if shift { 10 } else { 1 };
                        let delta = dx
                            .checked_mul(step)
                            .zip(dy.checked_mul(step))
                            .ok_or(AppError::InvalidInput("ruler movement overflows"))?;
                        let next = current.translated(delta.0, delta.1)?;
                        self.validate_measurement(next)?;
                        self.ruler.move_selected(dx, dy, shift)?;
                    } else {
                        self.sampler.move_by(dx, dy, shift)?;
                    }
                    Ok(vec![])
                }
                UtilityInput::Select => {
                    let snapshot = self
                        .snapshot
                        .as_ref()
                        .ok_or(AppError::Capture("utility source unavailable"))?;
                    if self.kind == UtilityKind::Sampler {
                        let color = sample_pixel(snapshot, self.sampler.point)?;
                        self.result = Some(ColorResultState::new(color));
                        self.snapshot = None;
                        Ok(vec![UtilityEffect::ShowColorResult(color)])
                    } else {
                        let run = measure_run(
                            snapshot,
                            self.sampler.point,
                            self.ruler.axis,
                            self.ruler.tolerance_percent,
                        )?;
                        self.ruler.add(run.rect()?)?;
                        Ok(vec![])
                    }
                }
                UtilityInput::AddRect(r) => {
                    self.validate_measurement(r)?;
                    self.ruler.add(r)?;
                    Ok(vec![])
                }
                UtilityInput::ReplaceRect { index, rect } => {
                    self.validate_measurement(rect)?;
                    self.ruler.replace(index, rect)?;
                    Ok(vec![])
                }
                UtilityInput::SetAxis(axis) => {
                    self.ruler.axis = axis;
                    Ok(vec![])
                }
                UtilityInput::ToggleAxis => {
                    self.ruler.axis = self.ruler.axis.toggle();
                    Ok(vec![])
                }
                UtilityInput::Tolerance(delta) => {
                    self.ruler.adjust_tolerance(delta);
                    Ok(vec![])
                }
                UtilityInput::CopySelected => {
                    if let Some(r) = self.ruler.selected() {
                        Ok(vec![UtilityEffect::CopyText(measure_rect(r)?.text())])
                    } else {
                        Ok(vec![])
                    }
                }
                UtilityInput::Clear => {
                    self.ruler.clear();
                    Ok(vec![])
                }
                UtilityInput::RemoveOrClose => {
                    if self.ruler.remove_selected() {
                        Ok(vec![])
                    } else {
                        self.close();
                        Ok(vec![UtilityEffect::Close])
                    }
                }
            }
        })();
        let effects = outcome.unwrap_or_else(|e| {
            self.error = Some(e.clone());
            vec![UtilityEffect::Error(e)]
        });
        if effects.contains(&UtilityEffect::Close) {
            self.close();
        }
        effects
    }
}
