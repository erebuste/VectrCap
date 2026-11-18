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
    delete_local: Option<Vec<Uuid>>,
    previews: HistoryPreviews,
    last_layout: Option<HistoryLayout>,
    last_columns: Option<usize>,
    scroll_anchor: usize,
    queue_paused: bool,
}
impl HistoryPane {
    pub fn suspend(&mut self) {
        self.delete_local = None;
        self.previews.suspend();
    }
    /// Title, Gallery/List and Preview toggles, and "Upload file"
    /// (`ChooseUploadFile`, which the shell turns into the native picker).
    pub fn draw_header(&mut self, ui: &mut Ui, view: &DesktopView) -> Vec<AppCommand> {
        use super::{icons::Icon, tokens, widgets};
        let mut commands = vec![];
        let preferences = view
            .settings
            .as_ref()
            .map(|s| s.history.clone())
            .unwrap_or_default();
        let mut prefs = preferences.clone();
        let buttons = [
            (Icon::Grid, "Gallery"),
            (Icon::List, "List"),
            (Icon::PanelRight, "Preview"),
            (Icon::Upload, "Upload file"),
        ]
        .iter()
        .map(|(icon, label)| widgets::button_width(ui, Some(*icon), Some(label)))
        .sum::<f32>()
            + 4.0
            + 2.0 * 8.0;
        let title = |ui: &mut Ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(
                    egui::RichText::new("History")
                        .font(tokens::semibold(tokens::TITLE_SIZE))
                        .color(ui.visuals().text_color()),
                );
                ui.label(
                    egui::RichText::new("Everything you've captured or uploaded")
                        .size(tokens::BODY_SIZE)
                        .color(ui.visuals().weak_text_color()),
                );
            });
        };
        let mut actions = |ui: &mut Ui| {
            for (layout, icon, label) in [
                (HistoryLayout::Gallery, Icon::Grid, "Gallery"),
                (HistoryLayout::List, Icon::List, "List"),
            ] {
                ui.spacing_mut().item_spacing.x = 4.0;
                if widgets::toggle_button(ui, prefs.layout == layout, Some(icon), label).clicked() {
                    prefs.layout = layout;
                }
            }
            ui.add_space(4.0);
            ui.spacing_mut().item_spacing.x = 8.0;
            if widgets::toggle_button(ui, prefs.preview, Some(Icon::PanelRight), "Preview")
                .on_hover_text("Show or hide the preview")
                .clicked()
            {
                prefs.preview = !prefs.preview;
            }
            if widgets::primary_button(ui, Some(Icon::Upload), "Upload file").clicked() {
                commands.push(AppCommand::ChooseUploadFile);
            }
        };
        // Room for the title before the buttons wrap under it.
        if ui.available_width() >= 300.0 + buttons {
            ui.horizontal_top(|ui| {
                title(ui);
                ui.add_space(ui.available_width() - buttons);
                actions(ui);
            });
        } else {
            title(ui);
            ui.add_space(8.0);
            ui.horizontal_wrapped(&mut actions);
        }
        ui.add_space(16.0);
        if prefs != preferences {
            commands.push(AppCommand::SetHistoryPreferences(prefs));
        }
        commands
    }

    pub fn draw(&mut self, ui: &mut Ui, view: &DesktopView) -> Vec<AppCommand> {
        self.queue_paused = view.queue_paused;
        let mut commands = vec![];
        let prefs = view
            .settings
            .as_ref()
            .map(|s| s.history.clone())
            .unwrap_or_default();
        let muted = ui.visuals().weak_text_color();
        let mut changed = false;
        let mut dates_changed = false;
        let mut search_focused = false;
        ui.horizontal_wrapped(|ui| {
            let search_width = (ui.available_width() - 360.0).clamp(140.0, 520.0);
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.query.search)
                    .hint_text("Search captures, files, or links")
                    .desired_width(search_width)
                    .char_limit(64),
            );
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::F)) {
                search.request_focus();
            }
            changed = search.changed();
            search_focused = search.has_focus();
            egui::ComboBox::from_id_salt("history-status")
                .width(132.0)
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
            ui.label(egui::RichText::new("From").small().color(muted));
            dates_changed |= ui
                .add(
                    egui::TextEdit::singleline(&mut self.since)
                        .id(egui::Id::unique("history-since"))
                        .hint_text("YYYY-MM-DD")
                        .desired_width(96.0)
                        .char_limit(10),
                )
                .changed();
            ui.label(egui::RichText::new("Through").small().color(muted));
            dates_changed |= ui
                .add(
                    egui::TextEdit::singleline(&mut self.until)
                        .id(egui::Id::unique("history-until"))
                        .hint_text("YYYY-MM-DD")
                        .desired_width(96.0)
                        .char_limit(10),
                )
                .changed();
        });
        self.query.search.retain(|c| !c.is_control());
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
            let favorites = ui
                .selectable_label(self.query.favorites_only, "Favorites")
                .on_hover_text(
                    "Favorites bookmark records. They do not prevent cleanup or change retention.",
                );
            if favorites.clicked() {
                self.query.favorites_only = !self.query.favorites_only;
                changed = true;
            }
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
        let editing = search_focused
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
        if self
            .delete_local
            .as_ref()
            .is_some_and(|ids| ids != &self.selection)
        {
            self.delete_local = None;
        }
        if let Some(ids) = self.delete_local.clone() {
            ui.group(|ui| {
                ui.label("Permanently delete selected app-owned finalized local copies? Imported originals, remote uploads, links and favorites are kept. Active upload sources cannot be deleted.");
                if ui.button("Confirm local file deletion").clicked() {
                    commands.push(action(ids, HistoryAction::DeleteLocalFile));
                    self.previews.suspend();
                    self.delete_local = None;
                }
                if ui.button("Keep local files").clicked() { self.delete_local = None; }
            });
        }
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
        let selected = records.iter().find(|r| Some(r.id) == self.active);
        let available = ui.available_height();
        let workspace = if available.is_finite() {
            (available - 108.0).max(140.0)
        } else {
            480.0
        };
        if prefs.preview && ui.available_width() >= 720.0 {
            let total = ui.available_width();
            let inspector_width = (total * 0.34).clamp(220.0, 270.0);
            let results_width = (total - inspector_width - 12.0).max(160.0);
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(results_width, workspace),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| self.results(ui, records, &prefs, &mut commands),
                );
                ui.allocate_ui_with_layout(
                    egui::vec2(inspector_width, workspace),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("history-inspector")
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.inspector(ui, selected, &mut commands));
                    },
                );
            });
        } else {
            let results_height = if prefs.preview {
                (workspace * 0.58).max(120.0)
            } else {
                workspace
            };
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), results_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| self.results(ui, records, &prefs, &mut commands),
            );
            if prefs.preview {
                ui.separator();
                let inspector_height = (workspace - results_height).max(120.0);
                egui::ScrollArea::vertical()
                    .id_salt("history-inspector")
                    .max_height(inspector_height)
                    .show(ui, |ui| self.inspector(ui, selected, &mut commands));
            }
        }
        self.footer(ui, records, view.history_page.has_more, &mut commands);

        commands
    }
    fn results(
        &mut self,
        ui: &mut Ui,
        records: &[HistoryRecord],
        prefs: &crate::persistence::settings::HistoryPreferences,
        commands: &mut Vec<AppCommand>,
    ) {
        let muted = ui.visuals().weak_text_color();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Recent activity").small().color(muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!("{} items", records.len()))
                        .small()
                        .color(muted),
                );
            });
        });
        if records.is_empty() {
            ui.label("No matching items. Try another search or clear your filters.");
        }
        let gallery = prefs.layout == HistoryLayout::Gallery;
        let columns = if gallery {
            ((ui.available_width() / 168.0).floor() as usize).clamp(1, 6)
        } else {
            1
        };
        let row_height = if gallery { 184.0 } else { 52.0 };
        let card_height = if gallery { 172.0 } else { row_height };
        let available = ui.available_height();
        let max_height = if available.is_finite() && available > 48.0 {
            available
        } else {
            480.0
        };
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("history-results")
            .max_height(max_height)
            .auto_shrink([false, false]);
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
                    if gallery {
                        let width = (ui.available_width() / columns as f32 - 8.0).max(120.0);
                        ui.horizontal(|ui| {
                            for index in row * columns..((row + 1) * columns).min(records.len()) {
                                ui.allocate_ui_with_layout(
                                    egui::vec2(width, card_height),
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
    }
    fn inspector(
        &mut self,
        ui: &mut Ui,
        record: Option<&HistoryRecord>,
        commands: &mut Vec<AppCommand>,
    ) {
        ui.label(egui::RichText::new("Preview & details").strong());
        let Some(record) = record else {
            ui.label("Select a capture or file to see its preview and actions.");
            return;
        };
        ui.add_space(4.0);
        let _preview = self.picture(ui, record, ui.available_width(), 140.0);
        ui.label(egui::RichText::new(&record.name).strong());
        let when = created_label(record.created_ms);
        if !when.is_empty() {
            ui.label(when);
        }
        ui.label(&record.kind);
        let status = self.status_line(record);
        ui.colored_label(status_color(ui, status), status);
        ui.label(if record.local_available {
            "Local file available"
        } else {
            "Local file missing"
        });
        let confirmed = record
            .job
            .as_ref()
            .is_some_and(|job| matches!(job.status, JobStatus::Completed(_)));
        if let Some(url) = share_url(record) {
            ui.add(egui::Label::new(url).selectable(true).wrap());
        } else {
            ui.label(
                egui::RichText::new("No share link yet")
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
        }
        if record.retryable {
            if wide_button(ui, "Retry upload", true, true) {
                commands.push(action(vec![record.id], HistoryAction::RetryFailed));
            }
        } else if wide_button(ui, "Copy link", confirmed, true) {
            commands.push(action(vec![record.id], HistoryAction::CopyLinks));
        }
        if wide_button(ui, "Open link", confirmed, false) {
            commands.push(action(vec![record.id], HistoryAction::OpenLink));
        }
        let open_file =
            record.local_available && AppController::viewer_extension(&record.artifact).is_some();
        if wide_button(ui, "Open file", open_file, false) {
            commands.push(action(vec![record.id], HistoryAction::OpenLocal));
        }
        if wide_button(ui, "Show folder", record.local_available, false) {
            commands.push(action(vec![record.id], HistoryAction::ShowFolder));
        }
        let pin = record.local_available && crate::pin::source::is_raster_name(&record.name);
        if wide_button(ui, "Pin", pin, false) {
            commands.push(action(vec![record.id], HistoryAction::Pin));
        }
        if wide_button(ui, "Remove from history...", true, false) {
            self.remove = Some(vec![record.id]);
        }
        ui.separator();
        ui.label(format!("Size  {}", format_size(record.size)));
        ui.label(format!("Kind  {}", record.kind));
        if let Some(meta) = &record.recording {
            ui.label(format!(
                "{} x {}  ·  {:.1}s",
                meta.width,
                meta.height,
                meta.active_millis as f64 / 1000.0
            ));
        }
        if record.local_available {
            ui.add(
                egui::Label::new(record.artifact.path.to_string_lossy())
                    .selectable(true)
                    .wrap(),
            );
        }
        if let Some(job) = &record.job {
            ui.label(&job.destination.label);
            if let JobStatus::Failed { code } = &job.status {
                ui.label(format!("Failure: {code}"));
            }
        }
    }
    fn picture(
        &self,
        ui: &mut Ui,
        record: &HistoryRecord,
        width: f32,
        height: f32,
    ) -> egui::Response {
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
        if let Some(Some(texture)) = self.previews.images.get(&record.id) {
            let original = texture.size_vec2();
            if original.x > 0.0 && original.y > 0.0 {
                let size = original * ((width / original.x).min(height / original.y));
                let image_rect = egui::Rect::from_center_size(rect.center(), size);
                ui.painter().image(
                    texture.id(),
                    image_rect,
                    egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        } else {
            ui.painter().rect_filled(
                rect,
                egui::CornerRadius::same(4),
                ui.visuals().faint_bg_color,
            );
            if height > 36.0 {
                let font = egui::TextStyle::Small.resolve(ui.style());
                let color = ui.visuals().weak_text_color();
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    preview_placeholder(self, record),
                    font,
                    color,
                );
            }
        }
        response
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
            let active = self.active == Some(record.id);
            let stroke = if gallery && active {
                egui::Stroke::new(1.5, ui.visuals().selection.bg_fill)
            } else if gallery {
                ui.visuals().widgets.noninteractive.bg_stroke
            } else {
                egui::Stroke::NONE
            };
            let fill = if gallery {
                ui.visuals().extreme_bg_color
            } else if active {
                active_fill(ui)
            } else {
                egui::Color32::TRANSPARENT
            };
            egui::Frame::new()
                .fill(fill)
                .stroke(stroke)
                .corner_radius(egui::CornerRadius::same(5))
                .inner_margin(egui::Margin::symmetric(6, 4))
                .show(ui, |ui| {
                    if gallery {
                        self.gallery_card(ui, record, index, records, commands);
                    } else {
                        self.list_row(ui, record, index, records, commands);
                    }
                })
                .response
                .context_menu(|ui| item_actions(ui, record, commands, &mut self.remove));
        });
    }
    fn gallery_card(
        &mut self,
        ui: &mut Ui,
        record: &HistoryRecord,
        index: usize,
        records: &[HistoryRecord],
        commands: &mut Vec<AppCommand>,
    ) {
        ui.horizontal(|ui| {
            self.selection_box(ui, record.id);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.star(ui, record, commands);
            });
        });
        let thumb = self.picture(ui, record, ui.available_width(), 84.0);
        self.use_hit(ui, &thumb, record, index, records, commands);
        let name = self.name_button(ui, record);
        self.use_hit(ui, &name, record, index, records, commands);
        self.status_row(ui, record);
    }
    fn list_row(
        &mut self,
        ui: &mut Ui,
        record: &HistoryRecord,
        index: usize,
        records: &[HistoryRecord],
        commands: &mut Vec<AppCommand>,
    ) {
        let status = self.status_line(record);
        let meta = self.meta_line(record);
        let (thumb, name) = ui
            .horizontal(|ui| {
                self.selection_box(ui, record.id);
                let thumb = self.picture(ui, record, 28.0, 28.0);
                let name = ui
                    .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.star(ui, record, commands);
                        ui.label(
                            egui::RichText::new(format_size(record.size))
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                        ui.colored_label(status_color(ui, status), status);
                        ui.vertical(|ui| {
                            ui.set_max_width(ui.available_width());
                            let name = self.name_button(ui, record);
                            ui.label(
                                egui::RichText::new(&meta)
                                    .small()
                                    .color(ui.visuals().weak_text_color()),
                            );
                            name
                        })
                        .inner
                    })
                    .inner;
                (thumb, name)
            })
            .inner;
        self.use_hit(ui, &thumb, record, index, records, commands);
        self.use_hit(ui, &name, record, index, records, commands);
    }
    fn footer(
        &mut self,
        ui: &mut Ui,
        records: &[HistoryRecord],
        has_more: bool,
        commands: &mut Vec<AppCommand>,
    ) {
        ui.separator();
        let muted = ui.visuals().weak_text_color();
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("{} on this page", records.len()));
            let failed = page_failed(records);
            if failed > 0 {
                ui.colored_label(ui.visuals().error_fg_color, format!("{failed} failed"));
            }
            let busy = page_in_progress(records, self.queue_paused);
            if busy > 0 {
                ui.label(format!("{busy} in progress"));
            }
            if self.selection.is_empty() {
                ui.label(egui::RichText::new("Ctrl+F to search").small().color(muted));
            }
        });
        if !self.selection.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{} selected", self.selection.len()));
                if ui.button("Copy successful links").clicked() {
                    commands.push(action(self.selection.clone(), HistoryAction::CopyLinks));
                }
                if ui.button("Retry failed uploads").clicked() {
                    commands.push(action(self.selection.clone(), HistoryAction::RetryFailed));
                }
                if ui.button("Remove from history").clicked() {
                    self.remove = Some(self.selection.clone());
                }
                if self.selection.len() == 1 && ui.button("Delete local copy...").clicked() {
                    self.delete_local = Some(self.selection.clone());
                }
                if ui.button("Clear selection").clicked() {
                    self.selection.clear();
                    self.active = None;
                }
            });
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.previous.is_empty(), egui::Button::new("Previous"))
                .clicked()
            {
                self.show_previous(commands);
            }
            if ui
                .add_enabled(has_more, egui::Button::new("Next"))
                .clicked()
            {
                self.show_next(records, commands);
            }
        });
    }
    fn show_previous(&mut self, commands: &mut Vec<AppCommand>) {
        self.query.after = self.previous.pop().unwrap();
        commands.push(AppCommand::QueryHistory(self.query.clone()));
        self.scroll_anchor = 0;
        self.anchor = 0;
        self.last_layout = None;
    }
    fn show_next(&mut self, records: &[HistoryRecord], commands: &mut Vec<AppCommand>) {
        let Some(last) = records.last() else {
            return;
        };
        self.previous.push(self.query.after.clone());
        self.query.after = Some(HistoryCursor::at(last));
        commands.push(AppCommand::QueryHistory(self.query.clone()));
        self.scroll_anchor = 0;
        self.anchor = 0;
        self.last_layout = None;
    }
    fn selection_box(&mut self, ui: &mut Ui, id: Uuid) {
        let mut checked = self.selection.contains(&id);
        if ui.checkbox(&mut checked, "").changed() {
            self.set_checked(id, checked);
        }
    }
    fn set_checked(&mut self, id: Uuid, selected: bool) {
        let present = self.selection.contains(&id);
        if selected && !present && self.selection.len() < 100 {
            self.selection.push(id);
            self.active = Some(id);
        } else if !selected && present {
            self.selection.retain(|existing| *existing != id);
            if self.active == Some(id) {
                self.active = self.selection.last().copied();
            }
        }
    }
    fn choose(
        &mut self,
        id: Uuid,
        index: usize,
        records: &[HistoryRecord],
        modifiers: egui::Modifiers,
    ) {
        if modifiers.shift && !records.is_empty() {
            let end = self.anchor.max(index).min(records.len() - 1);
            let start = self.anchor.min(index).min(end);
            for row in &records[start..=end] {
                if !self.selection.contains(&row.id) && self.selection.len() < 100 {
                    self.selection.push(row.id);
                }
            }
        } else if modifiers.command {
            self.set_checked(id, !self.selection.contains(&id));
        } else {
            self.selection = vec![id];
        }
        self.active = Some(id);
        self.anchor = index;
    }
    fn use_hit(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        record: &HistoryRecord,
        index: usize,
        records: &[HistoryRecord],
        commands: &mut Vec<AppCommand>,
    ) {
        if response.clicked() {
            self.choose(record.id, index, records, ui.input(|i| i.modifiers));
        }
        if response.double_clicked()
            || (response.has_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space)))
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
    }
    fn name_button(&self, ui: &mut Ui, record: &HistoryRecord) -> egui::Response {
        ui.add_sized(
            [ui.available_width(), 18.0],
            egui::Button::new(&record.name).truncate().frame(false),
        )
        .on_hover_text(&record.name)
    }
    fn star(&mut self, ui: &mut Ui, record: &HistoryRecord, commands: &mut Vec<AppCommand>) {
        let mark = if record.favorite { "★" } else { "☆" };
        if ui
            .add(egui::Button::new(mark).frame(false))
            .on_hover_text(
                "Favorites bookmark records. They do not prevent cleanup or change retention.",
            )
            .clicked()
        {
            commands.push(AppCommand::SetHistoryFavorite {
                id: record.id,
                favorite: !record.favorite,
            });
        }
    }
    fn status_row(&self, ui: &mut Ui, record: &HistoryRecord) {
        let status = self.status_line(record);
        let color = status_color(ui, status);
        let muted = ui.visuals().weak_text_color();
        ui.horizontal(|ui| {
            ui.colored_label(color, "●");
            ui.label(egui::RichText::new(status).small().color(color));
            ui.label(
                egui::RichText::new(format_size(record.size))
                    .small()
                    .color(muted),
            );
        });
        if !record.local_available {
            ui.label(
                egui::RichText::new("Local file missing")
                    .small()
                    .color(warn_color(ui)),
            );
        }
    }
    fn status_line(&self, record: &HistoryRecord) -> &'static str {
        job_status(
            record.job.as_ref().map(|job| &job.status),
            record.job.as_ref().is_some_and(|job| job.paused),
            self.queue_paused,
        )
    }
    fn meta_line(&self, record: &HistoryRecord) -> String {
        let when = created_label(record.created_ms);
        let missing = if record.local_available {
            ""
        } else {
            " · Local file missing"
        };
        format!("{when} · {}{missing}", record.kind)
    }
}
fn preview_placeholder(pane: &HistoryPane, record: &HistoryRecord) -> &'static str {
    if !record.local_available {
        "Local preview unavailable"
    } else if pane.previews.images.contains_key(&record.id) {
        match record.kind.as_str() {
            "mp4" => "MP4 recording",
            "gif" => "GIF recording",
            "screenshot" => "Image preview unavailable",
            _ => "File",
        }
    } else {
        "Loading preview..."
    }
}
fn created_label(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|date| {
            date.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}
pub(super) fn format_size(size: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * 1024 * 1024;
    if size >= GB {
        format!("{:.1} GB", size as f64 / GB as f64)
    } else if size >= MB {
        format!("{:.1} MB", size as f64 / MB as f64)
    } else if size >= KB {
        format!("{:.1} KB", size as f64 / KB as f64)
    } else {
        format!("{size} B")
    }
}
fn warn_color(ui: &Ui) -> egui::Color32 {
    if ui.visuals().override_text_color.is_some() {
        ui.visuals().text_color()
    } else {
        ui.visuals().warn_fg_color
    }
}
fn status_color(ui: &Ui, label: &str) -> egui::Color32 {
    if ui.visuals().override_text_color.is_some() {
        return ui.visuals().text_color();
    }
    let visuals = ui.visuals();
    match label {
        "Failed" | "Cancelled" => visuals.error_fg_color,
        "Uncertain" => visuals.warn_fg_color,
        "Uploading" | "Pausing" | "Waiting" => visuals.selection.bg_fill,
        _ if label.starts_with("Paused") => visuals.warn_fg_color,
        _ => visuals.weak_text_color(),
    }
}
fn active_fill(ui: &Ui) -> egui::Color32 {
    let color = ui.visuals().selection.bg_fill;
    if ui.visuals().override_text_color.is_some() {
        color.linear_multiply(0.55)
    } else {
        color.linear_multiply(0.28)
    }
}
fn wide_button(ui: &mut Ui, label: &str, enabled: bool, primary: bool) -> bool {
    let button = if primary {
        egui::Button::new(egui::RichText::new(label).color(ui.visuals().selection.stroke.color))
            .fill(ui.visuals().selection.bg_fill)
    } else {
        egui::Button::new(label)
    };
    ui.add_enabled(
        enabled,
        button.min_size(egui::vec2(ui.available_width(), 22.0)),
    )
    .clicked()
}
fn share_url(record: &HistoryRecord) -> Option<String> {
    record.job.as_ref().and_then(|job| match &job.status {
        JobStatus::Completed(receipt) => Some(receipt.url().to_string()),
        _ => None,
    })
}
fn page_failed(records: &[HistoryRecord]) -> usize {
    records
        .iter()
        .filter(|record| {
            matches!(
                record.job.as_ref().map(|job| &job.status),
                Some(JobStatus::Failed { .. })
            )
        })
        .count()
}
fn page_in_progress(records: &[HistoryRecord], queue_paused: bool) -> usize {
    records
        .iter()
        .filter(|record| {
            let Some(job) = &record.job else {
                return false;
            };
            match &job.status {
                JobStatus::Running => true,
                JobStatus::Pending | JobStatus::Interrupted => !job.paused && !queue_paused,
                _ => false,
            }
        })
        .count()
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
            "Pin",
            HistoryAction::Pin,
            r.local_available && crate::pin::source::is_raster_name(&r.name),
        ),
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

fn job_status(status: Option<&JobStatus>, paused: bool, queue_paused: bool) -> &'static str {
    match status {
        None => "Local only",
        Some(JobStatus::Completed(_)) => "Uploaded",
        Some(JobStatus::Failed { .. }) => "Failed",
        Some(JobStatus::Cancelled) => "Cancelled",
        Some(JobStatus::Uncertain) => "Uncertain",
        Some(JobStatus::Running) if paused || queue_paused => "Pausing",
        Some(JobStatus::Running) => "Uploading",
        Some(_) if paused && queue_paused => "Paused by job and queue",
        Some(_) if paused => "Paused by job",
        Some(_) if queue_paused => "Paused by queue",
        Some(_) => "Waiting",
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
                    recording: None,
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
    fn changing_selection_cancels_local_deletion_confirmation() {
        let view = view();
        let id = view.history_page.records[0].id;
        let mut pane = HistoryPane {
            selection: vec![Uuid::new_v4()],
            delete_local: Some(vec![id]),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            pane.draw(ui, &view);
        });
        output.textures_delta.clear();
        assert!(pane.delete_local.is_none());
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

    #[test]
    fn paused_uploads_are_not_labeled_waiting() {
        assert_eq!(
            job_status(Some(&JobStatus::Pending), false, true),
            "Paused by queue"
        );
        assert_eq!(
            job_status(Some(&JobStatus::Interrupted), true, false),
            "Paused by job"
        );
        assert_eq!(
            job_status(Some(&JobStatus::Pending), true, true),
            "Paused by job and queue"
        );
        assert_eq!(
            job_status(Some(&JobStatus::Running), false, true),
            "Pausing"
        );
        assert_eq!(
            job_status(Some(&JobStatus::Running), false, false),
            "Uploading"
        );
        assert_eq!(
            job_status(Some(&JobStatus::Pending), false, false),
            "Waiting"
        );
        assert_eq!(
            job_status(Some(&JobStatus::Interrupted), false, false),
            "Waiting"
        );
        assert_eq!(job_status(None, false, true), "Local only");
        assert_eq!(
            job_status(
                Some(&JobStatus::Failed {
                    code: "network".into()
                }),
                false,
                true
            ),
            "Failed"
        );
        assert_eq!(
            job_status(Some(&JobStatus::Cancelled), true, true),
            "Cancelled"
        );
        assert_eq!(
            job_status(Some(&JobStatus::Uncertain), false, true),
            "Uncertain"
        );
    }
}
