//! Explicit per-user registration controls; opening Settings never writes keys.
use super::widgets::{self, PillTone};
use crate::{
    app::error::AppError,
    platform::windows::{destination_association::DestinationAssociation, explorer::*},
};
use eframe::egui::Ui;
#[derive(Default)]
pub struct IntegrationsPane {
    status: Option<Result<(RegistrationStatus, RegistrationStatus), AppError>>,
    error: Option<AppError>,
}
fn status_pill(ui: &mut Ui, status: RegistrationStatus) {
    // On/off is the switch; a pill only flags the states a switch can't show.
    let text = match status {
        RegistrationStatus::Enabled | RegistrationStatus::Disabled => return,
        RegistrationStatus::Stale => "Needs repair",
        RegistrationStatus::Foreign => "Another app",
    };
    widgets::pill(ui, text, PillTone::Warning);
}
impl IntegrationsPane {
    pub fn draw(&mut self, ui: &mut Ui) {
        let executable = std::env::current_exe().map_err(|e| AppError::Io {
            stage: "locating executable",
            kind: e.kind(),
        });
        let Ok(executable) = executable else {
            widgets::hint(
                ui,
                "VectrCap's location is unavailable, so integrations can't be changed.",
            );
            return;
        };
        let registry = NativeRegistry;
        let explorer = ExplorerIntegration::new(&registry, executable.clone());
        let association = DestinationAssociation::new(&registry, executable);
        // Read (never write) the registration once, so the switches show the
        // real state; every change still waits for a switch flip.
        if self.status.is_none() {
            self.status = Some(
                explorer
                    .status()
                    .and_then(|a| association.status().map(|b| (a, b))),
            );
        }
        let known = match &self.status {
            Some(Ok(pair)) => Some(*pair),
            _ => None,
        };
        // 0/1 enable/disable Explorer, 2/3 enable/disable .vectrcap.
        let mut action = None;
        widgets::setting_group(
            ui,
            "Windows integration",
            Some(
                "Turn these on after VectrCap is in its permanent folder. Windows defaults you chose yourself are kept; another app's default may need Open with.",
            ),
            |ui| {
                for (index, label, description, status) in [
                    (
                        0,
                        "\"Upload with Vectr\" in Explorer",
                        "Adds the action to the right-click menu for files (Show more options on Windows 11).",
                        known.map(|(explorer, _)| explorer),
                    ),
                    (
                        2,
                        "Open .vectrcap files with VectrCap",
                        "Lets you double-click a downloaded config to add a connection.",
                        known.map(|(_, association)| association),
                    ),
                ] {
                    ui.push_id(index, |ui| {
                        widgets::setting_row_with(
                            ui,
                            |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        eframe::egui::RichText::new(label)
                                            .font(super::tokens::medium(super::tokens::BODY_SIZE))
                                            .color(ui.visuals().text_color()),
                                    );
                                    if let Some(status) = status {
                                        status_pill(ui, status);
                                    }
                                });
                                widgets::hint(ui, description);
                            },
                            |ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                // Stale still points at VectrCap, just an old path.
                                let registered = matches!(
                                    status,
                                    Some(RegistrationStatus::Enabled | RegistrationStatus::Stale)
                                );
                                let mut on = registered;
                                if widgets::toggle_switch(ui, &mut on, label).changed() {
                                    action = Some(if on { index } else { index + 1 });
                                }
                                if status == Some(RegistrationStatus::Stale)
                                    && widgets::ghost_button(ui, None, "Repair").clicked()
                                {
                                    action = Some(index);
                                }
                            },
                        );
                    });
                }
                // Only shown when something needs saying: a failed read or change.
                let read_error = match &self.status {
                    Some(Err(error)) => Some(error.to_string()),
                    _ => None,
                };
                if read_error.is_some() || self.error.is_some() {
                    widgets::card_footer(ui, |ui| {
                        for message in read_error
                            .iter()
                            .chain(self.error.as_ref().map(|e| e.to_string()).iter())
                        {
                            ui.colored_label(ui.visuals().error_fg_color, message);
                        }
                        if widgets::ghost_button(ui, None, "Check again").clicked() {
                            self.status = None;
                        }
                    });
                }
            },
        );
        if let Some(action) = action {
            let result = match action {
                0 => explorer.enable(),
                1 => explorer.disable(),
                2 => association.enable(),
                _ => association.disable(),
            };
            match result {
                Ok(()) => {
                    self.error = None;
                    self.status = Some(
                        explorer
                            .status()
                            .and_then(|a| association.status().map(|b| (a, b))),
                    );
                }
                Err(error) => self.error = Some(error),
            }
        }
    }
}
