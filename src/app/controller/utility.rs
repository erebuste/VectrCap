use super::*;
use crate::{
    tools::{
        UtilitySnapshot,
        color_result::ColorResultInput,
        session::{UtilityEffect, UtilityInput, UtilityKind, UtilitySession},
    },
    ui::screen_sampler::UtilitySurface,
};
pub(super) struct UtilityWorkflow {
    id: Uuid,
    kind: UtilityKind,
    pending: bool,
    closing: bool,
    surface: Option<Arc<Mutex<UtilitySurface>>>,
}
impl AppController {
    pub fn utility_surface(&self) -> Option<Arc<Mutex<UtilitySurface>>> {
        self.utility.as_ref().and_then(|w| w.surface.clone())
    }
    pub(super) fn start_utility(&mut self, kind: UtilityKind) -> Result<Vec<AppEffect>, AppError> {
        if !self.pending_acquisitions.is_empty()
            || self.utility.is_some()
            || self.capture.is_some()
            || self.scrolling.is_some()
            || self
                .recording
                .as_ref()
                .is_some_and(|w| w.session.acquiring())
        {
            return Err(AppError::InUse(
                "finish the active acquisition before starting a utility",
            ));
        }
        self.manager.ensure_not_corrupted()?;
        let id = Uuid::new_v4();
        // This controller is the sole serialized owner: publish the reservation
        // before returning any effect capable of acquiring source pixels.
        self.utility = Some(UtilityWorkflow {
            id,
            kind,
            pending: true,
            closing: false,
            surface: None,
        });
        Ok(vec![AppEffect::AcquireUtility { session: id }])
    }
    pub(super) fn utility_acquired(
        &mut self,
        id: Uuid,
        result: Result<UtilitySnapshot, AppError>,
    ) -> Vec<AppEffect> {
        let Some(w) = self.utility.as_mut().filter(|w| w.id == id && w.pending) else {
            return vec![];
        };
        w.pending = false;
        if w.closing || self.lifecycle != Lifecycle::Running {
            self.utility = None;
            return vec![AppEffect::UtilitySurface(None)];
        }
        match result.and_then(|snapshot| {
            let bounds = snapshot.bounds();
            let point =
                snapshot
                    .launch_pointer()
                    .unwrap_or(crate::capture::geometry::PhysicalPoint {
                        x: snapshot.monitors()[0].bounds().left(),
                        y: snapshot.monitors()[0].bounds().top(),
                    });
            UtilitySurface::new(id, UtilitySession::new(w.kind, snapshot, point), bounds)
        }) {
            Ok(surface) => {
                let surface = Arc::new(Mutex::new(surface));
                w.surface = Some(surface.clone());
                vec![AppEffect::UtilitySurface(Some(surface))]
            }
            Err(error) => {
                self.utility = None;
                vec![AppEffect::UtilitySurface(None), AppEffect::Error(error)]
            }
        }
    }
    pub(super) fn close_utility(&mut self) -> Vec<AppEffect> {
        let Some(w) = &mut self.utility else {
            return vec![];
        };
        let mut effects = vec![
            AppEffect::CancelUtilityAcquisition,
            AppEffect::UtilitySurface(None),
        ];
        if let Some(surface) = w.surface.take() {
            match surface.lock() {
                Ok(mut s) => s.release(),
                Err(poison) => {
                    poison.into_inner().release();
                    effects.push(AppEffect::Error(AppError::Capture(
                        "utility surface lock poisoned; source released",
                    )));
                }
            }
        }
        if w.pending {
            w.closing = true;
        } else {
            self.utility = None;
        }
        effects
    }
    pub(super) fn utility_input(&mut self, id: Uuid, input: UtilityInput) -> Vec<AppEffect> {
        let Some(w) = self
            .utility
            .as_ref()
            .filter(|w| w.id == id && !w.closing && !w.pending)
        else {
            return vec![];
        };
        let Some(surface) = w.surface.clone() else {
            return vec![];
        };
        let Ok(mut state) = surface.lock() else {
            return vec![AppEffect::Error(AppError::Capture(
                "utility surface unavailable",
            ))];
        };
        let effects = state.session.handle(input);
        state.working = false;
        let mut output = vec![];
        let mut refresh = false;
        let mut close = false;
        for effect in effects {
            match effect {
                UtilityEffect::ShowColorResult(_) => {
                    state.release_presentation();
                    output.push(AppEffect::UtilitySurface(Some(surface.clone())));
                }
                UtilityEffect::CopyText(text) => {
                    let outcome = self.clipboard.write_utility_text(&text);
                    if state.session.result.is_some() {
                        state.session.handle(UtilityInput::Result(
                            ColorResultInput::ClipboardOutcome(outcome),
                        ));
                    } else {
                        state.copy_feedback = outcome
                            .map_or_else(crate::tools::color_result::CopyFeedback::Error, |()| {
                                crate::tools::color_result::CopyFeedback::Copied
                            });
                    }
                }
                UtilityEffect::RefreshSource => refresh = true,
                UtilityEffect::Close => close = true,
                UtilityEffect::Error(error) => output.push(AppEffect::Error(error)),
            }
        }
        drop(state);
        if close {
            output.extend(self.close_utility());
        } else if refresh {
            output.extend(self.close_utility());
            // A fresh identity prevents old viewport input or clipboard outcomes
            // from acting on the replacement source.
            match self.start_utility(UtilityKind::Sampler) {
                Ok(e) => output.extend(e),
                Err(e) => output.push(AppEffect::Error(e)),
            }
        }
        output
    }
}
