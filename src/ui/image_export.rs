//! Single-flight preview owner shared by capture and image review.
use crate::{
    app::error::AppError,
    editor::{
        EditorDocument,
        export::{ImageExportOptions, PreparedImageExport, prepare_export},
    },
};
use std::{
    sync::mpsc::{self, Receiver, TryRecvError},
    time::Duration,
};
#[derive(Default)]
pub struct ExportPreview {
    pending: Option<Receiver<Result<PreparedImageExport, AppError>>>,
    previous: Option<PreparedImageExport>,
    error: Option<AppError>,
}
impl ExportPreview {
    pub fn start(
        &mut self,
        document: &EditorDocument,
        options: &ImageExportOptions,
        wake: impl FnOnce() + Send + 'static,
    ) -> Result<(), AppError> {
        if self.pending.is_some() {
            return Err(AppError::InUse("an export preview is already preparing"));
        }
        let revision = document.export_revision();
        let options = options.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("vectrcap-image-export".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    prepare_export(revision, options)
                }))
                .unwrap_or(Err(AppError::Encoder("image preview worker failed")));
                let _ = tx.send(result);
                wake();
            })
            .map_err(|e| AppError::io("starting image export worker", &e))?;
        self.pending = Some(rx);
        self.error = None;
        Ok(())
    }
    pub fn is_preparing(&self) -> bool {
        self.pending.is_some()
    }
    pub fn ready(
        &self,
        document: &EditorDocument,
        options: &ImageExportOptions,
    ) -> Option<&PreparedImageExport> {
        if self.is_preparing() || self.error.is_some() {
            return None;
        }
        self.previous
            .as_ref()
            .filter(|p| p.matches(document, options))
    }
    pub fn previous(&self) -> Option<&PreparedImageExport> {
        self.previous.as_ref()
    }
    pub fn error(&self) -> Option<&AppError> {
        self.error.as_ref()
    }
    fn accept(
        &mut self,
        result: Result<PreparedImageExport, AppError>,
        document: &EditorDocument,
        options: &ImageExportOptions,
    ) -> Result<(), AppError> {
        self.pending = None;
        let result = result.and_then(|prepared| {
            if prepared.matches(document, options) {
                Ok(prepared)
            } else {
                Err(AppError::InvalidInput(
                    "preview is stale; prepare the current settings again",
                ))
            }
        });
        match result {
            Ok(prepared) => {
                self.previous = Some(prepared);
                self.error = None;
                Ok(())
            }
            Err(error) => {
                self.error = Some(error.clone());
                Err(error)
            }
        }
    }
    /// Nonblocking UI path. `wake` in start asks the owner to repaint on completion.
    pub fn poll(
        &mut self,
        document: &EditorDocument,
        options: &ImageExportOptions,
    ) -> Result<bool, AppError> {
        let Some(pending) = &self.pending else {
            return Ok(false);
        };
        let result = match pending.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return Ok(false),
            Err(TryRecvError::Disconnected) => {
                Err(AppError::Encoder("image export worker stopped"))
            }
        };
        self.accept(result, document, options)?;
        Ok(true)
    }
    /// Bounded waiting for non-UI callers and deterministic tests.
    pub fn wait(
        &mut self,
        document: &EditorDocument,
        options: &ImageExportOptions,
        timeout: Duration,
    ) -> Result<(), AppError> {
        let pending = self
            .pending
            .as_ref()
            .ok_or(AppError::InvalidInput("no export is preparing"))?;
        let result = match pending.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(AppError::InUse("image export is still preparing"));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(AppError::Encoder("image export worker stopped"))
            }
        };
        self.accept(result, document, options)
    }
}
