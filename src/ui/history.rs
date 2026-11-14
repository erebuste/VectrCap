//! Shared History browsing, selection, and typed actions. No disk/network on UI thread.
use super::history_previews::HistoryPreviews;
use crate::{
    app::{
        controller::AppController,
        desktop::DesktopView,
        events::{AppCommand, HistoryAction},
    },
    persistence::{
        history::{JobStatus, browser::*},
        settings::HistoryLayout,
    },
};
use eframe::egui::{self, Ui};
use uuid::Uuid;
#[derive(Default)]
pub struct HistoryPane {
    query: HistoryQuery,
    previous: Vec<Option<HistoryCursor>>,
    selection: Vec<Uuid>,
    active: Option<Uuid>,
    anchor: usize,
    since: String,
    until: String,
    date_error: Option<&'static str>,
    remove: Option<Vec<Uuid>>,
    previews: HistoryPreviews,
    last_layout: Option<HistoryLayout>,
    last_columns: Option<usize>,
    scroll_anchor: usize,
}
impl HistoryPane {
    pub fn suspend(&mut self) {
        self.previews.suspend();
    }
    pub fn draw(&mut self, ui: &mut Ui, view: &DesktopView) -> Vec<AppCommand> {
        let mut commands = vec![];
        let preferences = view
            .settings
            .as_ref()
            .map(|s| s.history.clone())
            .unwrap_or_default();
        let mut prefs = preferences.clone();
        ui.horizontal_wrapped(|ui| {
            ui.heading("History");
            ui.selectable_value(&mut prefs.layout, HistoryLayout::Gallery, "Gallery");
            ui.selectable_value(&mut prefs.layout, HistoryLayout::List, "List");
            ui.checkbox(&mut prefs.preview, "Preview");
        });
        if prefs != preferences {
            commands.push(AppCommand::SetHistoryPreferences(prefs.clone()));
        }
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
                .checkbox(&mut self.query.favorites_only, "Favorites")
                .changed();
        });
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("history-status")
                .selected_text(status_filter(self.query.status))
                .show_ui(ui, |ui| {
                    for value in [
                        HistoryStatus::Any,
                        HistoryStatus::Uploaded,
                        HistoryStatus::Failed,
                        HistoryStatus::Waiting,
                        HistoryStatus::Uncertain,
                        HistoryStatus::LocalOnly,
                        HistoryStatus::Cancelled,
                    ] {
                        changed |= ui
                            .selectable_value(&mut self.query.status, value, status_filter(value))
                            .changed();
                    }
                });
            egui::ComboBox::from_id_salt("history-sort")
                .selected_text(sort_label(self.query.sort))
                .show_ui(ui, |ui| {
                    for value in [
                        HistorySort::Newest,
                        HistorySort::Oldest,
                        HistorySort::Name,
                        HistorySort::Size,
                    ] {
                        changed |= ui
                            .selectable_value(&mut self.query.sort, value, sort_label(value))
                            .changed();
                    }
                });
        });
        let mut dates_changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("From");
            dates_changed |= ui
                .add(
                    egui::TextEdit::singleline(&mut self.since)
                        .id(egui::Id::unique("history-since"))
                        .hint_text("YYYY-MM-DD")
                        .desired_width(100.0)
                        .char_limit(10),
                )
                .changed();
            ui.label("Through");
            dates_changed |= ui
                .add(
                    egui::TextEdit::singleline(&mut self.until)
                        .id(egui::Id::unique("history-until"))
                        .hint_text("YYYY-MM-DD")
                        .desired_width(100.0)
                        .char_limit(10),
                )
                .changed();
        });
        if dates_changed {
            match date_range(&self.since, &self.until) {
                Ok((since, until)) => {
                    self.query.since_ms = since;
                    self.query.until_ms = until;
                    self.date_error = None;
                    changed = true;
                }
                Err(error) => self.date_error = Some(error),
            }
        }
        ui.small("Favorites bookmark records; they do not prevent cleanup or change retention.");
        if let Some(error) = self.date_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
            return commands;
        }
        if changed {
            self.query.after = None;
            self.previous.clear();
            self.selection.clear();
            self.active = None;
            self.scroll_anchor = 0;
            self.anchor = 0;
            self.last_layout = None;
            commands.push(AppCommand::QueryHistory(self.query.clone()));
        }
        if view.history_query != self.query {
            ui.label("Updating history...");
            self.previews.suspend();
            return commands;
        }
        if let Some((queued, skipped)) = view.history_retry_report {
            ui.label(format!(
                "{queued} uploads queued; {skipped} selected items skipped."
            ));
        }
        let records = &view.history_page.records;
        self.previews.update(ui.ctx(), records);
        let editing = search.has_focus()
            || ui.memory(|m| {
                m.has_focus(egui::Id::unique("history-since"))
                    || m.has_focus(egui::Id::unique("history-until"))
            });
        if !editing {
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::A)) {
                self.selection = records.iter().map(|r| r.id).collect();
            }
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete))
                && !self.selection.is_empty()
            {
                self.remove = Some(self.selection.clone());
            }
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("{} selected", self.selection.len()));
            if ui
                .add_enabled(
                    !self.selection.is_empty(),
                    egui::Button::new("Copy successful links"),
                )
                .clicked()
            {
                commands.push(action(self.selection.clone(), HistoryAction::CopyLinks));
            }
            if ui
                .add_enabled(
                    !self.selection.is_empty(),
                    egui::Button::new("Retry failed uploads"),
                )
                .clicked()
            {
                commands.push(action(self.selection.clone(), HistoryAction::RetryFailed));
            }
            if ui
                .add_enabled(
                    !self.selection.is_empty(),
                    egui::Button::new("Remove from history"),
                )
                .clicked()
            {
                self.remove = Some(self.selection.clone());
            }
            if ui.button("Clear selection").clicked() {
                self.selection.clear();
                self.active = None;
            }
        });
        if let Some(ids) = self.remove.clone() {
            ui.group(|ui| {
                ui.label(format!("Remove {} history records? Local files and remote uploads will be kept. Unfinished uploads must be settled or cancelled first.", ids.len()));
                ui.horizontal(|ui| {
                    if ui.button("Remove records").clicked() {
                        commands.push(action(ids, HistoryAction::Remove));
                        self.remove = None;
                        self.selection.clear();
                        self.active = None;
                        self.query.after = None;
                        self.previous.clear();
                        commands.push(AppCommand::QueryHistory(self.query.clone()));
                        self.previews.suspend();
                    }
                    if ui.button("Keep records").clicked() { self.remove = None; }
                });
            });
        }
        if records.is_empty() {
            ui.label("No matching history items.");
        }
        let selected = records.iter().find(|r| Some(r.id) == self.active);
        if prefs.preview && ui.available_width() >= 900.0 && selected.is_some() {
            let width = ui.available_width();
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(width - 350.0, 560.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        self.results(
                            ui,
                            records,
                            &prefs,
                            view.history_page.has_more,
                            &mut commands,
                        )
                    },
                );
                ui.separator();
                ui.allocate_ui_with_layout(
                    egui::vec2(320.0, 560.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| self.inspector(ui, selected.unwrap(), &mut commands),
                );
            });
        } else {
            self.results(
                ui,
                records,
                &prefs,
                view.history_page.has_more,
                &mut commands,
            );
            if prefs.preview
                && let Some(record) = records.iter().find(|r| Some(r.id) == self.active)
            {
                self.inspector(ui, record, &mut commands);
            }
        }

        commands
    }
    fn results(
        &mut self,
        ui: &mut Ui,
        records: &[HistoryRecord],
        prefs: &crate::persistence::settings::HistoryPreferences,
        has_more: bool,
        commands: &mut Vec<AppCommand>,
    ) {
        let columns = if prefs.layout == HistoryLayout::Gallery {
            ((ui.available_width() / 160.0).floor() as usize).clamp(1, 6)
        } else {
            1
        };
        let row_height = if prefs.layout == HistoryLayout::Gallery {
            210.0
        } else {
            82.0
        };
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("history-results")
            .max_height(if prefs.preview { 340.0 } else { 520.0 });
        if self.last_layout != Some(prefs.layout) || self.last_columns != Some(columns) {
            scroll =
                scroll.vertical_scroll_offset((self.scroll_anchor / columns) as f32 * row_height);
            self.last_layout = Some(prefs.layout);
            self.last_columns = Some(columns);
        }
        scroll.show_rows(
            ui,
            row_height,
            records.len().div_ceil(columns),
            |ui, range| {
                self.scroll_anchor = range.start * columns;
                for row in range {
                    if prefs.layout == HistoryLayout::Gallery {
                        let width = (ui.available_width() / columns as f32 - 8.0).max(120.0);
                        ui.horizontal(|ui| {
                            for index in row * columns..((row + 1) * columns).min(records.len()) {
                                ui.allocate_ui_with_layout(
                                    egui::vec2(width, 200.0),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        self.item(
                                            ui,
                                            &records[index],
                                            index,
                                            records,
                                            true,
                                            commands,
                                        )
                                    },
                                );
                            }
                        });
                    } else {
                        self.item(ui, &records[row], row, records, false, commands);
                    }
                }
            },
        );
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.previous.is_empty(), egui::Button::new("Previous"))
                .clicked()
            {
                self.query.after = self.previous.pop().unwrap();
                commands.push(AppCommand::QueryHistory(self.query.clone()));
                self.scroll_anchor = 0;
                self.anchor = 0;
                self.last_layout = None;
            }
            if ui
                .add_enabled(has_more, egui::Button::new("Next"))
                .clicked()
                && let Some(last) = records.last()
            {
                self.previous.push(self.query.after.clone());
                self.query.after = Some(HistoryCursor::at(last));
                commands.push(AppCommand::QueryHistory(self.query.clone()));
                self.scroll_anchor = 0;
                self.anchor = 0;
                self.last_layout = None;
            }
        });
    }
    fn inspector(&mut self, ui: &mut Ui, record: &HistoryRecord, commands: &mut Vec<AppCommand>) {
        ui.separator();
        ui.heading(&record.name);
        self.picture(ui, record, ui.available_width().min(640.0), 360.0);
        if let Some(date) = chrono::DateTime::from_timestamp_millis(record.created_ms) {
            ui.label(
                date.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string(),
            );
        }
        ui.label(format!(
            "{} | {} bytes | {}",
            record.kind,
            record.size,
            status(record.job.as_ref().map(|j| &j.status))
        ));
        ui.label(if record.local_available {
            "Local file available"
        } else {
            "Local file unavailable"
        });
        if record.local_available {
            ui.add(
                egui::Label::new(record.artifact.path.to_string_lossy())
                    .selectable(true)
                    .wrap(),
            );
        }
        if let Some(job) = &record.job {
            ui.label(&job.destination.label);
            ui.label(
                job.destination
                    .link_domain
                    .as_ref()
                    .map(|o| o.as_str())
                    .unwrap_or("uploader default"),
            );
            if let JobStatus::Completed(receipt) = &job.status {
                ui.add(egui::Label::new(receipt.url()).selectable(true).wrap());
            }
            if let JobStatus::Failed { code } = &job.status {
                ui.label(format!("Failure: {code}"));
            }
        }
        ui.horizontal_wrapped(|ui| item_actions(ui, record, commands, &mut self.remove));
    }
    fn picture(&self, ui: &mut Ui, record: &HistoryRecord, width: f32, height: f32) {
        if let Some(Some(texture)) = self.previews.images.get(&record.id) {
            let original = texture.size_vec2();
            let size = original * ((width / original.x).min(height / original.y));
            ui.image((texture.id(), size));
        } else {
            ui.allocate_ui(egui::vec2(width, height.min(105.0)), |ui| {
                ui.centered_and_justified(|ui| {
                    ui.label(if !record.local_available {
                        "Local preview unavailable"
                    } else if self.previews.images.contains_key(&record.id) {
                        match record.kind.as_str() {
                            "mp4" => "MP4 recording",
                            "gif" => "GIF recording",
                            "screenshot" => "Image preview unavailable",
                            _ => "File",
                        }
                    } else {
                        "Loading preview..."
                    });
                });
            });
        }
    }
    fn item(
        &mut self,
        ui: &mut Ui,
        record: &HistoryRecord,
        index: usize,
        records: &[HistoryRecord],
        gallery: bool,
        commands: &mut Vec<AppCommand>,
    ) {
        ui.push_id(record.id, |ui| {
            if gallery {
                self.picture(ui, record, ui.available_width(), 105.0);
            }
            let response = ui
                .add(
                    egui::Button::selectable(self.selection.contains(&record.id), &record.name)
                        .truncate(),
                )
                .on_hover_text(&record.name);
            if response.clicked() {
                let modifiers = ui.input(|i| i.modifiers);
                if modifiers.shift {
                    for row in &records
                        [self.anchor.min(index)..=self.anchor.max(index).min(records.len() - 1)]
                    {
                        if !self.selection.contains(&row.id) && self.selection.len() < 100 {
                            self.selection.push(row.id);
                        }
                    }
                } else if modifiers.command {
                    if let Some(position) = self.selection.iter().position(|id| *id == record.id) {
                        self.selection.remove(position);
                    } else if self.selection.len() < 100 {
                        self.selection.push(record.id);
                    }
                } else {
                    self.selection = vec![record.id];
                }
                self.active = Some(record.id);
                self.anchor = index;
            }
            if response.double_clicked()
                || (response.has_focus()
                    && ui.input(|i| {
                        i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space)
                    }))
            {
                self.active = Some(record.id);
                self.selection = vec![record.id];
                commands.push(AppCommand::SetHistoryPreferences(
                    crate::persistence::settings::HistoryPreferences {
                        layout: self.last_layout.unwrap_or(HistoryLayout::Gallery),
                        preview: true,
                    },
                ));
            }
            response.context_menu(|ui| item_actions(ui, record, commands, &mut self.remove));
            let date = if gallery {
                String::new()
            } else {
                chrono::DateTime::from_timestamp_millis(record.created_ms)
                    .map(|d| {
                        d.with_timezone(&chrono::Local)
                            .format(" | %Y-%m-%d %H:%M")
                            .to_string()
                    })
                    .unwrap_or_default()
            };
            let metadata = format!(
                "{} | {} bytes | {}{date}",
                record.kind,
                record.size,
                status(record.job.as_ref().map(|j| &j.status))
            );
            ui.add(egui::Label::new(egui::RichText::new(&metadata).small()).truncate())
                .on_hover_text(metadata);
            if ui
                .small_button(if record.favorite { "Unstar" } else { "Star" })
                .clicked()
            {
                commands.push(AppCommand::SetHistoryFavorite {
                    id: record.id,
                    favorite: !record.favorite,
                });
            }
        });
    }
}
fn action(ids: Vec<Uuid>, action: HistoryAction) -> AppCommand {
    AppCommand::HistoryAction { ids, action }
}
fn item_actions(
    ui: &mut Ui,
    r: &HistoryRecord,
    commands: &mut Vec<AppCommand>,
    remove: &mut Option<Vec<Uuid>>,
) {
    let confirmed = r
        .job
        .as_ref()
        .is_some_and(|j| matches!(j.status, JobStatus::Completed(_)));
    let offered = [
        (
            "Open file",
            HistoryAction::OpenLocal,
            r.local_available && AppController::viewer_extension(&r.artifact).is_some(),
        ),
        ("Show folder", HistoryAction::ShowFolder, r.local_available),
        ("Open link", HistoryAction::OpenLink, confirmed),
        ("Copy link", HistoryAction::CopyLinks, confirmed),
        ("Retry upload", HistoryAction::RetryFailed, r.retryable),
    ];
    for (label, kind, enabled) in offered {
        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
            commands.push(action(vec![r.id], kind));
        }
    }
    if ui.button("Remove from history...").clicked() {
        *remove = Some(vec![r.id]);
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
fn status_filter(s: HistoryStatus) -> &'static str {
    match s {
        HistoryStatus::Any => "All outcomes",
        HistoryStatus::Uploaded => "Uploaded",
        HistoryStatus::Failed => "Failed",
        HistoryStatus::Waiting => "Pending / uploading",
        HistoryStatus::Uncertain => "Uncertain",
        HistoryStatus::LocalOnly => "Local only",
        HistoryStatus::Cancelled => "Cancelled",
    }
}
fn sort_label(s: HistorySort) -> &'static str {
    match s {
        HistorySort::Newest => "Newest first",
        HistorySort::Oldest => "Oldest first",
        HistorySort::Name => "Filename A-Z",
        HistorySort::Size => "Largest first",
    }
}
pub fn date_range(since: &str, until: &str) -> Result<(Option<i64>, Option<i64>), &'static str> {
    fn parse(s: &str, end: bool) -> Result<Option<i64>, &'static str> {
        if s.is_empty() {
            return Ok(None);
        }
        let date = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| "Use dates in YYYY-MM-DD format")?;
        let date = if end {
            date.checked_add_days(chrono::Days::new(1))
                .ok_or("Date outside supported range")?
        } else {
            date
        };
        let date = date
            .and_hms_opt(0, 0, 0)
            .and_then(|v| v.and_local_timezone(chrono::Local).single())
            .ok_or("Date is ambiguous in the local time zone")?;
        Ok(Some(date.timestamp_millis() - i64::from(end)))
    }
    let (a, b) = (parse(since, false)?, parse(until, true)?);
    if a.zip(b).is_some_and(|(a, b)| a > b) {
        return Err("Start date must not follow end date");
    }
    Ok((a, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn view() -> DesktopView {
        let id = Uuid::new_v4();
        let artifact = crate::app::types::Artifact {
            id: crate::app::types::ArtifactId(Uuid::new_v4()),
            path: std::path::PathBuf::from("C:\\missing-history-test.png"),
            display_name: "History selection.png".into(),
            kind: crate::app::types::ArtifactKind::Screenshot,
            size: 0,
            sha256: [0; 32],
        };
        DesktopView {
            settings: Some(crate::persistence::settings::Settings::default()),
            history_page: HistoryPage {
                records: vec![HistoryRecord {
                    id,
                    name: artifact.display_name.clone(),
                    kind: "screenshot".into(),
                    size: 0,
                    created_ms: 100,
                    favorite: false,
                    local_available: false,
                    retryable: false,
                    artifact,
                    job: None,
                }],
                has_more: false,
            },
            ..Default::default()
        }
    }
    #[test]
    fn gallery_list_and_responsive_inspector_keep_selection_and_query() {
        let mut view = view();
        let id = view.history_page.records[0].id;
        let mut pane = HistoryPane {
            active: Some(id),
            selection: vec![id],
            ..Default::default()
        };
        let ctx = egui::Context::default();
        for width in [380.0, 1100.0] {
            for layout in [HistoryLayout::Gallery, HistoryLayout::List] {
                view.settings.as_mut().unwrap().history.layout = layout;
                let mut commands = vec![];
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 900.0),
                        )),
                        ..Default::default()
                    },
                    |ui| commands = pane.draw(ui, &view),
                );
                output.textures_delta.clear();
                assert!(commands.is_empty());
                assert_eq!(pane.selection, vec![id]);
                assert_eq!(pane.active, Some(id));
                assert!(pane.query == view.history_query);
            }
        }
        pane.suspend();
    }
}
