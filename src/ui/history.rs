//! Bounded history list; persistence stays on the controller thread.
use crate::{
    app::{desktop::DesktopView, events::AppCommand},
    persistence::history::{JobStatus, browser::*},
};
use eframe::egui::{self, Ui};
#[derive(Default)]
pub struct HistoryPane {
    query: HistoryQuery,
    previous: Vec<Option<HistoryCursor>>,
    selected: Option<uuid::Uuid>,
}
impl HistoryPane {
    pub fn draw(&mut self, ui: &mut Ui, view: &DesktopView) -> Vec<AppCommand> {
        let mut commands = vec![];
        ui.heading("History");
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.query.search)
                .hint_text("Search filename, type, or sharing link")
                .char_limit(64),
        );
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::F)) {
            search.request_focus();
        }
        self.query.search.retain(|c| !c.is_control());
        let mut changed = search.changed();
        ui.horizontal_wrapped(|ui| {
            for (label, kind) in [
                ("All", HistoryType::All),
                ("Images", HistoryType::Images),
                ("Recordings", HistoryType::Recordings),
                ("Files", HistoryType::Files),
            ] {
                changed |= ui
                    .selectable_value(&mut self.query.kind, kind, label)
                    .changed();
            }
            changed |= ui
                .checkbox(&mut self.query.favorites_only, "Favorites only")
                .changed();
        });
        ui.small("Favorites bookmark records; they do not prevent cleanup or change retention.");
        if changed {
            self.query.after = None;
            self.previous.clear();
            self.selected = None;
            commands.push(AppCommand::QueryHistory(self.query.clone()));
        }
        if view.history_query != self.query {
            ui.label("Updating history...");
            return commands;
        }
        let records = &view.history_page.records;
        if records.is_empty() {
            ui.label("No matching history items.");
        }
        egui::ScrollArea::vertical()
            .id_salt("history-list")
            .max_height(360.0)
            .show_rows(ui, 56.0, records.len(), |ui, range| {
                for index in range {
                    let item = &records[index];
                    ui.push_id(item.id, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .button(if item.favorite { "Unstar" } else { "Star" })
                                .clicked()
                            {
                                commands.push(AppCommand::SetHistoryFavorite {
                                    id: item.id,
                                    favorite: !item.favorite,
                                });
                            }
                            if ui
                                .selectable_label(self.selected == Some(item.id), &item.name)
                                .clicked()
                            {
                                self.selected = Some(item.id);
                            }
                        });
                        ui.small(format!(
                            "{} | {} bytes | {}",
                            item.kind,
                            item.size,
                            status(item.job.as_ref().map(|j| &j.status))
                        ));
                    });
                }
            });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.previous.is_empty(), egui::Button::new("Previous"))
                .clicked()
            {
                self.query.after = self.previous.pop().unwrap();
                self.selected = None;
                commands.push(AppCommand::QueryHistory(self.query.clone()));
            }
            if ui
                .add_enabled(view.history_page.has_more, egui::Button::new("Next"))
                .clicked()
                && let Some(last) = records.last()
            {
                self.previous.push(self.query.after);
                self.query.after = Some(HistoryCursor {
                    created_ms: last.created_ms,
                    id: last.id,
                });
                self.selected = None;
                commands.push(AppCommand::QueryHistory(self.query.clone()));
            }
        });
        if let Some(item) = records.iter().find(|r| Some(r.id) == self.selected) {
            ui.separator();
            ui.strong(&item.name);
            if let Some(date) = chrono::DateTime::from_timestamp_millis(item.created_ms) {
                ui.label(
                    date.with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string(),
                );
            }
            ui.label(if item.local_available {
                "Local file available"
            } else {
                "Local file unavailable"
            });
            if let Some(job) = &item.job {
                ui.label(&job.destination.label);
                if let JobStatus::Completed(receipt) = &job.status {
                    ui.add(egui::Label::new(receipt.url()).selectable(true).wrap());
                }
            }
        }
        commands
    }
}
fn status(status: Option<&JobStatus>) -> &'static str {
    match status {
        None => "Local only",
        Some(JobStatus::Completed(_)) => "Uploaded",
        Some(JobStatus::Failed { .. }) => "Failed",
        Some(JobStatus::Cancelled) => "Cancelled",
        Some(JobStatus::Uncertain) => "Uncertain",
        Some(JobStatus::Running) => "Uploading",
        _ => "Waiting",
    }
}
