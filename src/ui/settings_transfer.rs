//! Preferences backup review. File pickers/read/write run on bounded workers;
//! only an explicit Apply submits a validated preview to the controller.
use crate::{
    app::{desktop::DesktopClient, error::AppError, events::AppCommand},
    persistence::{settings::Settings, settings_transfer::*},
    platform::windows::file_dialog::{self, PickerGate},
};
use eframe::egui;
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, TryRecvError},
};

enum FileResult {
    Imported(Option<ImportPreview>),
    Exported,
    Cancelled,
    Replace(PathBuf, Settings),
}
#[derive(Default)]
pub struct SettingsTransferPane {
    preview: Option<ImportPreview>,
    selection: ImportSelection,
    worker: Option<Receiver<Result<FileResult, AppError>>>,
    applying: Option<Receiver<Result<(), AppError>>>,
    replace: Option<(PathBuf, Settings)>,
    status: Option<String>,
}
impl SettingsTransferPane {
    fn start(
        &mut self,
        ctx: &egui::Context,
        task: impl FnOnce() -> Result<FileResult, AppError> + Send + 'static,
    ) {
        let (tx, rx) = mpsc::sync_channel(1);
        let ctx = ctx.clone();
        match std::thread::Builder::new()
            .name("preferences-file".into())
            .spawn(move || {
                let result = task();
                let _ = tx.send(result);
                ctx.request_repaint();
            }) {
            Ok(_) => {
                self.worker = Some(rx);
                self.status = None;
            }
            Err(_) => self.status = Some("Preferences file worker unavailable".into()),
        }
    }
    pub fn reconcile(&mut self) {
        if let Some(rx) = &self.worker {
            let result = rx.try_recv();
            match result {
                Ok(result) => {
                    self.worker = None;
                    match result {
                        Ok(FileResult::Imported(Some(preview))) => {
                            self.preview = Some(preview);
                            self.selection = ImportSelection::safe_preferences();
                            self.status=Some("Preview ready. Select groups and review destination mappings before applying.".into());
                        }
                        Ok(FileResult::Replace(path, settings)) => {
                            self.replace = Some((path, settings))
                        }
                        Ok(FileResult::Exported) => {
                            self.status = Some("Preferences backup exported".into())
                        }
                        Ok(FileResult::Imported(None) | FileResult::Cancelled) => {}
                        Err(error) => self.status = Some(error.to_string()),
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.worker = None;
                    self.status = Some("Preferences file worker stopped".into());
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = &self.applying {
            match rx.try_recv() {
                Ok(result) => {
                    self.applying = None;
                    match result {
                        Ok(()) => {
                            self.preview = None;
                            self.status = Some("Selected preferences imported".into());
                        }
                        Err(error) => self.status = Some(error.to_string()),
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.applying = None;
                    self.status = Some(
                        "Preferences controller stopped; refresh settings before retrying".into(),
                    );
                }
                Err(TryRecvError::Empty) => {}
            }
        }
    }
    pub fn draw(
        &mut self,
        ui: &mut egui::Ui,
        settings: &Settings,
        client: &DesktopClient,
        gate: &PickerGate,
    ) {
        ui.heading("Backup");
        ui.label("Back up preferences and destination links. Tokens, history and device identifiers are excluded. Install destinations with the separate .vectrcap action.");
        let busy = self.worker.is_some() || self.applying.is_some();
        ui.add_enabled_ui(!busy && self.replace.is_none(), |ui| {
            self.draw_file_actions(ui, settings, gate)
        });
        if self.replace.is_some() {
            ui.label("Replace the selected backup file?");
            if let Some((path, _)) = &self.replace {
                ui.label(path.display().to_string());
            }
            if ui.button("Confirm replacement").clicked()
                && let Some((path, settings)) = self.replace.take()
            {
                self.start(ui.ctx(), move || {
                    write_export(&path, &settings, true)?;
                    Ok(FileResult::Exported)
                });
            }
            if ui.button("Cancel export").clicked() {
                self.replace = None;
            }
        }
        if let Some(message) = &self.status {
            ui.label(message);
        }
        if busy {
            ui.spinner();
            ui.label("Waiting for the file operation or active capture session to end...");
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.draw_preview(ui, settings, client, busy);
    }
    fn draw_file_actions(&mut self, ui: &mut egui::Ui, settings: &Settings, gate: &PickerGate) {
        ui.horizontal(|ui| {
            if ui.button("Export preferences...").clicked()
                && let Some(guard) = gate.try_acquire()
            {
                let snapshot = settings.clone();
                self.start(ui.ctx(), move || {
                    let _guard = guard;
                    let Some(path) = file_dialog::pick_preferences_file(true)? else {
                        return Ok(FileResult::Cancelled);
                    };
                    if path
                        .try_exists()
                        .map_err(|e| AppError::io("checking preferences backup", &e))?
                    {
                        return Ok(FileResult::Replace(path, snapshot));
                    }
                    write_export(&path, &snapshot, false)?;
                    Ok(FileResult::Exported)
                });
            }
            if ui.button("Import preferences...").clicked()
                && let Some(guard) = gate.try_acquire()
            {
                let snapshot = settings.clone();
                self.start(ui.ctx(), move || {
                    let _guard = guard;
                    file_dialog::pick_preferences_file(false)?
                        .map(|p| read_import(&p, &snapshot))
                        .transpose()
                        .map(FileResult::Imported)
                });
            }
        });
    }
    fn draw_preview(
        &mut self,
        ui: &mut egui::Ui,
        settings: &Settings,
        client: &DesktopClient,
        busy: bool,
    ) {
        let Some(preview) = &self.preview else {
            return;
        };
        ui.separator();
        ui.label(format!(
            "Preview based on settings revision {}",
            preview.base_revision()
        ));
        for d in preview.diff() {
            let suffix = if d.requires_opt_in {
                " (separate selection)"
            } else {
                ""
            };
            ui.collapsing(format!("{}: {}{suffix}", d.group, d.summary), |ui| {
                for line in &d.details {
                    ui.label(line);
                }
            });
        }
        for conflict in &preview.conflicts {
            ui.label(*conflict);
        }
        let stale = preview.base_revision() != settings.revision;
        if stale {
            ui.label("Settings changed. Refresh this preview before applying.");
        }
        ui.add_enabled_ui(!busy, |ui| {
            draw_selection(ui, &mut self.selection, preview, settings)
        });
        let refresh = ui
            .add_enabled(
                !busy,
                egui::Button::new("Refresh preview against current settings"),
            )
            .clicked();
        let apply = ui
            .add_enabled(!busy && !stale, egui::Button::new("Apply selected groups"))
            .clicked();
        let discard = ui
            .add_enabled(!busy, egui::Button::new("Discard preview"))
            .clicked();
        if refresh {
            match preview.refresh(settings) {
                Ok(p) => self.preview = Some(p),
                Err(e) => self.status = Some(e.to_string()),
            }
        } else if apply {
            let (tx, rx) = mpsc::sync_channel(1);
            if let Some(preview) = self.preview.clone() {
                match client.submit(AppCommand::ApplySettingsImport {
                    preview: Box::new(preview),
                    selection: self.selection.clone(),
                    reply: tx,
                }) {
                    Ok(()) => self.applying = Some(rx),
                    Err(e) => self.status = Some(e.to_string()),
                }
            }
        } else if discard {
            self.preview = None;
        }
    }
}
fn draw_selection(
    ui: &mut egui::Ui,
    g: &mut ImportSelection,
    preview: &ImportPreview,
    settings: &Settings,
) {
    ui.label("Select preference groups");
    for (value, label) in [
        (&mut g.destinations, "Destinations"),
        (&mut g.shortcuts, "Hotkeys and tool bindings"),
        (&mut g.tasks, "Task profiles"),
        (&mut g.capture, "Capture and region"),
        (&mut g.recording, "Recording"),
        (&mut g.image, "Image export"),
        (&mut g.tools, "Annotation tool defaults"),
        (&mut g.pin, "Pin display defaults"),
        (&mut g.filename, "Filename preset"),
        (&mut g.appearance, "UI scale"),
        (&mut g.archive, "Archive storage and path"),
        (&mut g.notifications, "Notifications"),
    ] {
        ui.checkbox(value, label);
    }
    ui.separator();
    ui.label("Separate selections (off by default)");
    ui.add_enabled_ui(g.tasks, |ui| {
        ui.checkbox(
            &mut g.automation,
            "Import automatic upload and open-link behavior",
        );
    });
    ui.add_enabled_ui(g.archive, |ui| {
        ui.checkbox(&mut g.cleanup, "Import age-based cleanup policy");
    });
    ui.checkbox(
        &mut g.default_destination,
        "Import default destination (required for a first connection)",
    );
    ui.add_enabled_ui(g.recording, |ui| {
        ui.checkbox(
            &mut g.audio_enablement,
            "Import audio mode and resolve local devices",
        );
    });
    ui.label("Startup, Explorer and .vectrcap association registration use their own settings.");
    ui.label("New destinations need a token. An explicit exact-origin mapping retains that local connection and its credentials.");
    for d in preview.destinations() {
        ui.push_id(d.key, |ui| {
            let mut selected = g.destination_mapping.get(&d.key).copied();
            let label = selected
                .and_then(|id| settings.destinations.get(id))
                .map(|d| d.name.as_str())
                .unwrap_or("New connection ... needs setup");
            ui.label(format!("{} ({})", d.name, d.origin.as_str()));
            egui::ComboBox::from_id_salt("preferences-destination-map")
                .selected_text(label)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut selected, None, "New connection ... needs setup");
                    for old in settings
                        .destinations
                        .iter()
                        .filter(|old| old.origin == d.origin)
                    {
                        ui.selectable_value(&mut selected, Some(old.id), &old.name);
                    }
                });
            if let Some(id) = selected {
                g.destination_mapping.insert(d.key, id);
            } else {
                g.destination_mapping.remove(&d.key);
            }
        });
    }
}
