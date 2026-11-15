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

pub struct ImageFinish {
    pub action: super::capture_overlay::OverlayAction,
    pub prepared: PreparedImageExport,
    pub screen_location: Option<crate::capture::geometry::PhysicalPoint>,
}
#[derive(Default)]
pub struct ImageExportPane {
    options: ImageExportOptions,
    preview: ExportPreview,
    attempted: Option<(uuid::Uuid, u64, ImageExportOptions)>,
    validation_error: Option<AppError>,
    worker_error: Option<AppError>,
    viewport: super::image_review::ImageViewport,
    finishing: bool,
}
impl ImageExportPane {
    pub fn options(&self) -> &ImageExportOptions {
        &self.options
    }
    pub fn options_mut(&mut self) -> &mut ImageExportOptions {
        &mut self.options
    }
    pub fn set_finishing(&mut self, finishing: bool) {
        self.finishing = finishing;
    }
    pub fn is_preparing(&self) -> bool {
        self.preview.is_preparing()
    }
    pub fn ready(&self, document: &EditorDocument) -> Option<&PreparedImageExport> {
        if self.validation_error.is_some() || self.worker_error.is_some() {
            return None;
        }
        self.preview.ready(document, &self.options)
    }
    pub fn previous(&self) -> Option<&PreparedImageExport> {
        self.preview.previous()
    }
    pub fn error(&self) -> Option<&AppError> {
        self.validation_error
            .as_ref()
            .or(self.worker_error.as_ref())
            .or_else(|| self.preview.error())
    }
    pub fn retry(&mut self) {
        self.attempted = None;
        self.worker_error = None;
    }
    pub fn update(&mut self, document: &EditorDocument, wake: impl FnOnce() + Send + 'static) {
        let _ = self.preview.poll(document, &self.options);
        self.validation_error = document
            .crop()
            .ok_or(AppError::InvalidInput(
                "select an output region before finishing",
            ))
            .and_then(|r| self.options.dimensions(r.width(), r.height()))
            .err();
        let key = (document.id(), document.revision(), self.options.clone());
        if self.validation_error.is_none()
            && !self.preview.is_preparing()
            && self.preview.ready(document, &self.options).is_none()
            && self.attempted.as_ref() != Some(&key)
        {
            self.attempted = Some(key);
            self.worker_error = None;
            if let Err(error) = self.preview.start(document, &self.options, wake) {
                self.worker_error = Some(error);
            }
        }
    }
    /// Pure confirmation boundary: all finish actions share these exact immutable
    /// encoded bytes and decoded pixels. The host owns artifact and OS effects.
    pub fn confirm(
        &self,
        document: &EditorDocument,
        action: super::capture_overlay::OverlayAction,
    ) -> Result<ImageFinish, AppError> {
        use super::capture_overlay::OverlayAction;
        if self.finishing {
            return Err(AppError::InUse("an image finish is already in progress"));
        }
        if !matches!(
            action,
            OverlayAction::Upload | OverlayAction::Copy | OverlayAction::Save | OverlayAction::Pin
        ) {
            return Err(AppError::InvalidInput("choose an image finish action"));
        }
        let crop = document.crop().ok_or(AppError::InvalidInput(
            "select an output region before finishing",
        ))?;
        self.options.dimensions(crop.width(), crop.height())?;
        let prepared = self
            .ready(document)
            .ok_or(AppError::InUse(
                "prepare the current output before finishing",
            ))?
            .clone();
        Ok(ImageFinish {
            action,
            prepared,
            screen_location: document.screen_location(),
        })
    }
    pub fn draw(
        &mut self,
        ui: &mut eframe::egui::Ui,
        document: &EditorDocument,
    ) -> Option<ImageFinish> {
        use super::capture_overlay::OverlayAction;
        use crate::editor::export::{ExportFormat, Resize};
        use eframe::egui;
        let mut controls_error = None;
        ui.add_enabled_ui(!self.finishing, |ui| {
            ui.label("Format");
            let mut format = self.options.format();
            egui::ComboBox::from_id_salt("image-format")
                .selected_text(format_label(format))
                .show_ui(ui, |ui| {
                    for value in [
                        ExportFormat::Png,
                        ExportFormat::Jpeg,
                        ExportFormat::WebpLossy,
                        ExportFormat::WebpLossless,
                    ] {
                        ui.selectable_value(&mut format, value, format_label(value));
                    }
                });
            if format != self.options.format()
                && let Err(error) = self.options.set_format(format)
            {
                controls_error = Some(error);
            }
            if let Some(mut quality) = self.options.quality()
                && ui
                    .add(egui::Slider::new(&mut quality, 1..=100).text("Quality"))
                    .changed()
                && let Err(error) = self.options.set_quality(quality)
            {
                controls_error = Some(error);
            }
            if format == ExportFormat::Jpeg {
                ui.label("JPEG composites transparency on white.");
            }
            let old = self.options.resize();
            let mut resize = old;
            let width = document.crop().map_or(1, |r| r.width());
            let height = document.crop().map_or(1, |r| r.height());
            ui.label("Resize");
            egui::ComboBox::from_id_salt("image-resize")
                .selected_text(resize_label(resize))
                .show_ui(ui, |ui| {
                    for value in [
                        Resize::Original,
                        Resize::Percent(50),
                        Resize::Width((width / 2).max(1)),
                        Resize::Height((height / 2).max(1)),
                    ] {
                        if ui
                            .selectable_label(
                                std::mem::discriminant(&resize) == std::mem::discriminant(&value),
                                resize_label(value),
                            )
                            .clicked()
                        {
                            resize = value;
                        }
                    }
                });
            match &mut resize {
                Resize::Original => {}
                Resize::Percent(p) => {
                    ui.add(egui::Slider::new(p, 1..=100).text("Resize %"));
                }
                Resize::Width(w) => {
                    ui.add(
                        egui::DragValue::new(w)
                            .range(1..=width)
                            .prefix("Width ")
                            .suffix(" px"),
                    );
                }
                Resize::Height(h) => {
                    ui.add(
                        egui::DragValue::new(h)
                            .range(1..=height)
                            .prefix("Height ")
                            .suffix(" px"),
                    );
                }
            }
            ui.label("Aspect ratio locked; downscale only.");
            if resize != old
                && let Err(error) = self.options.set_resize(resize)
            {
                controls_error = Some(error);
            }
        });
        let ctx = ui.ctx().clone();
        self.update(document, move || ctx.request_repaint());
        if controls_error.is_some() {
            self.validation_error = controls_error;
        }
        if self.preview.is_preparing() {
            ui.spinner();
            ui.label("Preparing decoded output...");
        }
        if let Some(error) = self.error() {
            ui.colored_label(ui.visuals().error_fg_color, error.to_string());
            if ui.button("Retry preview").clicked() {
                self.retry();
                ui.ctx().request_repaint();
            }
        }
        let ready = self.ready(document).is_some() && !self.finishing;
        let mut finish = None;
        ui.horizontal_wrapped(|ui| {
            for (action, label) in [
                (OverlayAction::Upload, "Upload"),
                (OverlayAction::Copy, "Copy image"),
                (OverlayAction::Save, "Save..."),
                (OverlayAction::Pin, "Pin"),
            ] {
                if ui.add_enabled(ready, egui::Button::new(label)).clicked() {
                    match self.confirm(document, action) {
                        Ok(output) => {
                            self.finishing = true;
                            finish = Some(output);
                        }
                        Err(error) => self.validation_error = Some(error),
                    }
                }
            }
        });
        if let Some(prepared) = self.preview.previous() {
            let (width, height) = prepared.dimensions();
            ui.label(format!(
                "{width} × {height} px · {} encoded bytes · {}",
                prepared.encoded_size(),
                prepared.mime()
            ));
            if !ready {
                ui.label("Previous decoded output; confirmation is disabled until the current output is ready.");
            }
            if let Err(error) = self.viewport.show(ui, prepared.pixels()) {
                ui.colored_label(ui.visuals().error_fg_color, error.to_string());
            }
        }
        finish
    }
}
fn format_label(format: crate::editor::export::ExportFormat) -> &'static str {
    use crate::editor::export::ExportFormat;
    match format {
        ExportFormat::Png => "PNG (lossless)",
        ExportFormat::Jpeg => "JPEG",
        ExportFormat::WebpLossy => "WebP (lossy)",
        ExportFormat::WebpLossless => "WebP (lossless)",
    }
}
fn resize_label(resize: crate::editor::export::Resize) -> &'static str {
    use crate::editor::export::Resize;
    match resize {
        Resize::Original => "Original size",
        Resize::Percent(_) => "Percentage",
        Resize::Width(_) => "Target width",
        Resize::Height(_) => "Target height",
    }
}
