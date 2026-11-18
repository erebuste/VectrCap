//! Explicit diagnostic controls. Constructing/drawing this page is pure: only
//! button actions submit work. It has no capture/audio or permission capability.
use crate::{
    app::{
        desktop::DesktopClient, destinations::DestinationId, error::AppError, events::AppCommand,
    },
    diagnostics::{checks::error_code, report::*},
    persistence::settings::Settings,
    platform::windows::{
        clipboard::WindowsClipboard,
        file_dialog::{self, PickerGate},
    },
};
use eframe::egui;
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, TryRecvError},
};

pub enum TroubleshootingAction {
    Refresh,
    TestConnection(DestinationId),
    CancelConnection,
    Preview,
    Save,
    Copy,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cannot_replace_displayed_preview_during_report_export() {
        let mut pane = TroubleshootingPane::default();
        pane.freeze_preview().unwrap();
        let before = pane.preview().unwrap().as_str().to_owned();
        pane.snapshot.pending_jobs = 99;
        let (_tx, rx) = mpsc::sync_channel(1);
        pane.file = Some(rx);
        assert!(pane.freeze_preview().is_err());
        assert_eq!(pane.preview().unwrap().as_str(), before);
        pane.file = None;
        pane.replace = Some(("unused".into(), pane.preview().unwrap().clone()));
        assert!(pane.freeze_preview().is_err());
        assert_eq!(pane.preview().unwrap().as_str(), before);
        pane.replace = None;
        pane.freeze_preview().unwrap();
        assert_ne!(pane.preview().unwrap().as_str(), before);
    }
}
enum FileResult {
    Saved,
    Copied,
    Cancelled,
    Replace(PathBuf, ReportPreview),
}
#[derive(Default)]
pub struct TroubleshootingPane {
    selected: Option<DestinationId>,
    revision: Option<u64>,
    snapshot: DiagnosticSnapshot,
    preview: Option<ReportPreview>,
    refresh: Option<Receiver<DiagnosticSnapshot>>,
    connection: Option<(DestinationId, u64, Receiver<ConnectionCheck>)>,
    connection_cancel: Option<tokio_util::sync::CancellationToken>,
    file: Option<Receiver<Result<FileResult, AppError>>>,
    replace: Option<(PathBuf, ReportPreview)>,
    status: Option<&'static str>,
    error: Option<ErrorCode>,
}
impl Drop for TroubleshootingPane {
    fn drop(&mut self) {
        self.cancel_connection();
    }
}
impl TroubleshootingPane {
    pub fn cancel_connection(&mut self) {
        if let Some(cancel) = &self.connection_cancel {
            cancel.cancel();
        }
    }
    fn freeze_preview(&mut self) -> Result<(), AppError> {
        if self.file.is_some() || self.replace.is_some() {
            return Err(AppError::InUse(
                "finish diagnostic export before replacing its preview",
            ));
        }
        let preview = ReportPreview::new(&self.snapshot)?;
        self.preview = Some(preview);
        Ok(())
    }
    pub fn preview(&self) -> Option<&ReportPreview> {
        self.preview.as_ref()
    }
    pub fn snapshot(&self) -> &DiagnosticSnapshot {
        &self.snapshot
    }
    /// The two Troubleshooting groups (mockup: "Connection check", "Diagnostic
    /// report"), one action per row. Drawing is pure; actions are returned.
    pub fn controls(
        &mut self,
        ui: &mut egui::Ui,
        settings: &Settings,
    ) -> Vec<TroubleshootingAction> {
        use super::widgets;
        let mut actions = Vec::new();
        if self
            .selected
            .is_none_or(|id| settings.destinations.get(id).is_none())
        {
            self.selected = settings.destinations.default_id();
        }
        let previous = self.selected;
        widgets::setting_group(
            ui,
            "Connection check",
            Some(
                "Sends a health request and a token-only uploader request. No test upload is created.",
            ),
            |ui| {
                widgets::setting_row(ui, "Connection", None, |ui| {
                    egui::ComboBox::from_id_salt("diagnostic-destination")
                        .selected_text(
                            self.selected
                                .and_then(|id| settings.destinations.get(id))
                                .map_or("Select a connection", |d| d.name.as_str()),
                        )
                        .show_ui(ui, |ui| {
                            for d in settings.destinations.iter() {
                                ui.selectable_value(&mut self.selected, Some(d.id), &d.name);
                            }
                        });
                });
                let name = self
                    .selected
                    .and_then(|id| settings.destinations.get(id))
                    .map_or("the connection", |d| d.name.as_str());
                widgets::setting_row(
                    ui,
                    "Test connection",
                    Some(&format!(
                        "Checks that {name} is reachable and the token works. Nothing is uploaded."
                    )),
                    |ui| {
                        if self.connection.is_some() {
                            if widgets::ghost_button(ui, None, "Cancel").clicked() {
                                actions.push(TroubleshootingAction::CancelConnection);
                            }
                            ui.spinner();
                        } else {
                            let run = ui
                                .add_enabled_ui(self.selected.is_some(), |ui| {
                                    widgets::toggle_button(ui, false, None, "Run test").clicked()
                                })
                                .inner;
                            if run && let Some(id) = self.selected {
                                actions.push(TroubleshootingAction::TestConnection(id));
                            }
                        }
                    },
                );
                let connection = &self.snapshot.connection;
                let tested = connection.health.availability != Availability::NotProbed
                    || connection.uploader.status != TokenStatus::NotTested;
                if tested {
                    widgets::card_block(ui, |ui| {
                        widgets::hint(
                            ui,
                            &format!(
                                "Service: {:?}. A healthy service doesn't prove the token works.",
                                connection.health.availability
                            ),
                        );
                        widgets::hint(
                            ui,
                            match connection.uploader.status {
                                TokenStatus::NotTested => "Token: not tested",
                                TokenStatus::NeedsSetup => "Token: needs setup",
                                TokenStatus::Verified => "Token: verified",
                                TokenStatus::UnverifiedManualDomain => {
                                    "Token: unverified (the server has no metadata endpoint); manual domain only"
                                }
                                TokenStatus::Failed => "Token: test failed",
                            },
                        );
                        for code in [connection.health.error, connection.uploader.error]
                            .into_iter()
                            .flatten()
                        {
                            widgets::hint(ui, &format!("Code: {code:?}"));
                        }
                    });
                }
            },
        );
        if previous != self.selected || self.revision != Some(settings.revision) {
            self.cancel_connection();
            self.connection_cancel = None;
            self.snapshot.connection = Default::default();
            self.connection = None;
            self.revision = Some(settings.revision);
        }
        ui.add_space(super::tokens::GROUP_GAP);
        widgets::setting_group(
            ui,
            "Diagnostic report",
            Some(
                "Counts and error codes only: no file names, links or tokens. Nothing is sent automatically.",
            ),
            |ui| {
                widgets::setting_row(
                    ui,
                    "Refresh counts",
                    Some(
                        "Reads history and storage totals. Capture, encoder and audio aren't probed.",
                    ),
                    |ui| {
                        let refresh = ui
                            .add_enabled_ui(self.refresh.is_none(), |ui| {
                                widgets::toggle_button(ui, false, None, "Refresh").clicked()
                            })
                            .inner;
                        if refresh {
                            actions.push(TroubleshootingAction::Refresh);
                        }
                    },
                );
                if self.snapshot.history.availability != Availability::NotProbed {
                    widgets::card_block(ui, |ui| {
                        widgets::hint(
                            ui,
                            &format!(
                                "History: {:?}; {} unfinished, {} completed uploads",
                                self.snapshot.history.availability,
                                self.snapshot.pending_jobs,
                                self.snapshot.completed_jobs
                            ),
                        );
                        for s in self.snapshot.storage {
                            widgets::hint(
                                ui,
                                &format!(
                                    "{:?}: {:?}, {} files, {} bytes{}",
                                    s.label,
                                    s.availability,
                                    s.files,
                                    s.bytes,
                                    if s.partial { " (partial scan)" } else { "" }
                                ),
                            );
                        }
                    });
                }
                widgets::setting_row(
                    ui,
                    "Preview a report",
                    Some("Freezes the current counts into the exact text Save and Copy use."),
                    |ui| {
                        let preview = ui
                            .add_enabled_ui(self.file.is_none() && self.replace.is_none(), |ui| {
                                widgets::toggle_button(ui, false, None, "Preview…").clicked()
                            })
                            .inner;
                        if preview {
                            actions.push(TroubleshootingAction::Preview);
                        }
                    },
                );
                if let Some(preview) = &self.preview {
                    widgets::card_block(ui, |ui| {
                        widgets::hint(
                            ui,
                            "Refresh never replaces this preview. Review it before sharing.",
                        );
                        egui::ScrollArea::vertical()
                            .id_salt("diagnostic-preview")
                            .max_height(240.0)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(preview.as_str()).monospace(),
                                    )
                                    .selectable(true),
                                );
                            });
                    });
                    widgets::card_footer(ui, |ui| {
                        let save = ui
                            .add_enabled_ui(self.file.is_none() && self.replace.is_none(), |ui| {
                                widgets::toggle_button(ui, false, None, "Save report…").clicked()
                            })
                            .inner;
                        if save {
                            actions.push(TroubleshootingAction::Save);
                        }
                        let copy = ui
                            .add_enabled_ui(self.file.is_none(), |ui| {
                                widgets::toggle_button(ui, false, None, "Copy report").clicked()
                            })
                            .inner;
                        if copy {
                            actions.push(TroubleshootingAction::Copy);
                        }
                    });
                }
            },
        );
        actions
    }
    pub(crate) fn poll(&mut self, settings: &Settings) {
        self.reconcile(settings);
    }
    fn reconcile(&mut self, settings: &Settings) {
        if let Some(rx) = &self.refresh {
            match rx.try_recv() {
                Ok(mut snapshot) => {
                    snapshot.connection = self.snapshot.connection;
                    self.snapshot = snapshot;
                    self.refresh = None;
                }
                Err(TryRecvError::Disconnected) => {
                    self.refresh = None;
                    self.error = Some(ErrorCode::WorkerUnavailable);
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some((id, revision, rx)) = &self.connection {
            match rx.try_recv() {
                Ok(result) => {
                    if self.selected == Some(*id) && settings.revision == *revision {
                        self.snapshot.connection = result;
                    }
                    self.connection = None;
                    self.connection_cancel = None;
                }
                Err(TryRecvError::Disconnected) => {
                    self.connection = None;
                    self.connection_cancel = None;
                    self.error = Some(ErrorCode::WorkerUnavailable);
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = &self.file {
            match rx.try_recv() {
                Ok(result) => {
                    self.file = None;
                    match result {
                        Ok(FileResult::Saved) => self.status = Some("Diagnostic report saved"),
                        Ok(FileResult::Copied) => self.status = Some("Diagnostic report copied"),
                        Ok(FileResult::Cancelled) => {
                            self.status = Some("Export canceled; frozen preview retained")
                        }
                        Ok(FileResult::Replace(path, preview)) => {
                            self.replace = Some((path, preview))
                        }
                        Err(e) => self.error = Some(error_code(&e)),
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.file = None;
                    self.error = Some(ErrorCode::WorkerUnavailable);
                }
                Err(TryRecvError::Empty) => {}
            }
        }
    }
    fn start_file(
        &mut self,
        ctx: &egui::Context,
        task: impl FnOnce() -> Result<FileResult, AppError> + Send + 'static,
    ) {
        let (tx, rx) = mpsc::sync_channel(1);
        let ctx = ctx.clone();
        match std::thread::Builder::new()
            .name("diagnostic-export".into())
            .spawn(move || {
                let _ = tx.try_send(task());
                ctx.request_repaint();
            }) {
            Ok(_) => {
                self.file = Some(rx);
                self.error = None;
                self.status = None;
            }
            Err(_) => self.error = Some(ErrorCode::WorkerUnavailable),
        }
    }
    pub fn draw(
        &mut self,
        ui: &mut egui::Ui,
        settings: &Settings,
        client: &DesktopClient,
        gate: &PickerGate,
    ) {
        self.draw_body(ui, settings, client, gate);
    }
    fn draw_body(
        &mut self,
        ui: &mut egui::Ui,
        settings: &Settings,
        client: &DesktopClient,
        gate: &PickerGate,
    ) {
        self.reconcile(settings);
        for action in self.controls(ui, settings) {
            match action {
                TroubleshootingAction::Refresh => {
                    let (tx, rx) = mpsc::sync_channel(1);
                    match client.submit(AppCommand::RefreshDiagnostics { reply: tx }) {
                        Ok(()) => {
                            self.refresh = Some(rx);
                            self.error = None;
                        }
                        Err(e) => self.error = Some(error_code(&e)),
                    }
                }
                TroubleshootingAction::TestConnection(id) => {
                    let (tx, rx) = mpsc::sync_channel(1);
                    let cancel = tokio_util::sync::CancellationToken::new();
                    match client.submit(AppCommand::TestDiagnosticConnection {
                        id,
                        revision: settings.revision,
                        cancel: cancel.clone(),
                        reply: tx,
                    }) {
                        Ok(()) => {
                            self.connection = Some((id, settings.revision, rx));
                            self.connection_cancel = Some(cancel);
                            self.snapshot.connection = Default::default();
                            self.error = None;
                        }
                        Err(e) => self.error = Some(error_code(&e)),
                    }
                }
                TroubleshootingAction::CancelConnection => self.cancel_connection(),
                TroubleshootingAction::Preview => match self.freeze_preview() {
                    Ok(()) => {}
                    Err(e) => self.error = Some(error_code(&e)),
                },
                TroubleshootingAction::Copy => {
                    if let Some(preview) = self.preview.clone() {
                        self.start_file(ui.ctx(), move || {
                            preview.copy(&WindowsClipboard)?;
                            Ok(FileResult::Copied)
                        });
                    }
                }
                TroubleshootingAction::Save => {
                    if let Some(preview) = self.preview.clone()
                        && let Some(guard) = gate.try_acquire()
                    {
                        self.start_file(ui.ctx(), move || {
                            let _guard = guard;
                            let Some(path) = file_dialog::pick_preferences_file(true)? else {
                                return Ok(FileResult::Cancelled);
                            };
                            if path
                                .try_exists()
                                .map_err(|e| AppError::io("checking diagnostic report", &e))?
                            {
                                return Ok(FileResult::Replace(path, preview));
                            }
                            preview.save(&path, false)?;
                            Ok(FileResult::Saved)
                        });
                    }
                }
            }
        }
        let replacing = self.replace.is_some() && self.file.is_none();
        let busy = self.refresh.is_some() || self.connection.is_some() || self.file.is_some();
        if replacing || busy || self.status.is_some() || self.error.is_some() {
            ui.add_space(14.0);
            super::widgets::card(ui, |ui| {
                super::widgets::card_block(ui, |ui| {
                    if replacing {
                        ui.label("Replace the existing report file with this preview?");
                        ui.horizontal_wrapped(|ui| {
                            if super::widgets::toggle_button(ui, false, None, "Replace").clicked()
                                && let Some((path, preview)) = self.replace.take()
                            {
                                self.start_file(ui.ctx(), move || {
                                    preview.save(&path, true)?;
                                    Ok(FileResult::Saved)
                                });
                            }
                            if super::widgets::ghost_button(ui, None, "Cancel").clicked() {
                                self.replace = None;
                            }
                        });
                    }
                    if let Some(status) = self.status {
                        ui.label(status);
                    }
                    if let Some(code) = self.error {
                        ui.label(format!(
                            "Diagnostic operation: {code:?}. Capture remains available."
                        ));
                    }
                    if busy {
                        ui.spinner();
                    }
                });
            });
        }
        if busy {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}
