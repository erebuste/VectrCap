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
    let (text, tone) = match status {
        RegistrationStatus::Enabled => ("On", PillTone::Success),
        RegistrationStatus::Disabled => ("Off", PillTone::Neutral),
        RegistrationStatus::Stale => ("Needs repair", PillTone::Warning),
        RegistrationStatus::Foreign => ("Another app", PillTone::Warning),
    };
    widgets::pill(ui, text, tone);
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
                                ui.spacing_mut().item_spacing.x = 6.0;
                                if widgets::ghost_button(ui, None, "Turn off").clicked() {
                                    action = Some(index + 1);
                                }
                                if widgets::toggle_button(ui, false, None, "Turn on / repair")
                                    .clicked()
                                {
                                    action = Some(index);
                                }
                            },
                        );
                    });
                }
                widgets::card_footer(ui, |ui| {
                    if widgets::toggle_button(ui, false, None, "Check status").clicked() {
                        self.status = Some(
                            explorer
                                .status()
                                .and_then(|a| association.status().map(|b| (a, b))),
                        );
                    }
                    match &self.status {
                        None => {
                            widgets::hint(ui, "Status is read only when you ask.");
                        }
                        Some(Err(error)) => {
                            ui.colored_label(ui.visuals().error_fg_color, error.to_string());
                        }
                        Some(Ok(_)) => {}
                    }
                    if let Some(error) = &self.error {
                        ui.colored_label(ui.visuals().error_fg_color, error.to_string());
                    }
                });
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
