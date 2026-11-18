//! Shared History browsing, selection, and typed actions. No disk/network on UI thread.
use super::history_previews::HistoryPreviews;
use crate::{
    app::{
        controller::AppController,
        desktop::DesktopView,
        events::{AppCommand, HistoryAction, HistoryFocus},
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

    /// Scroll the active card into view on the next frame (set by focus).
    reveal: bool,
    queue_paused: bool,
    /// Upload progress (0..1) by job, refreshed from the view every frame.
    progress: std::collections::HashMap<crate::app::types::JobId, f32>,
    date_preset: DatePreset,
    /// The `view.history_focus` last acted on, so each focus applies once.
    applied_focus: Option<HistoryFocus>,
    /// A focus target that isn't on this page, shown in the inspector instead.
    pinned: Option<HistoryFocus>,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum DatePreset {
    #[default]
    Any,
    Today,
    Week,
    Month,
    Custom,
}
impl DatePreset {
    const ALL: [Self; 5] = [
        Self::Any,
        Self::Today,
        Self::Week,
        Self::Month,
        Self::Custom,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::Any => "Any date",
            Self::Today => "Today",
            Self::Week => "Last 7 days",
            Self::Month => "Last 30 days",
            Self::Custom => "Custom range…",
        }
    }
    /// Start of the range in local time; `None` for Any and Custom.
    fn since_ms(self) -> Option<i64> {
        let now = chrono::Local::now();
        let midnight = |date: chrono::NaiveDate| {
            date.and_hms_opt(0, 0, 0)
                .and_then(|d| d.and_local_timezone(chrono::Local).earliest())
                .map(|d| d.timestamp_millis())
        };
        match self {
            Self::Today => midnight(now.date_naive()),
            Self::Week => midnight(now.date_naive() - chrono::Days::new(6)),
            Self::Month => midnight(now.date_naive() - chrono::Days::new(29)),
            Self::Any | Self::Custom => None,
        }
    }
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
        // `toggles`: Gallery / List / Preview; `upload`: the primary action.
        let mut actions = |ui: &mut Ui, toggles: bool, upload: bool| {
            if toggles {
                for (layout, icon, label) in [
                    (HistoryLayout::Gallery, Icon::Grid, "Gallery"),
                    (HistoryLayout::List, Icon::List, "List"),
                ] {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if widgets::toggle_button(ui, prefs.layout == layout, Some(icon), label)
                        .clicked()
                    {
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
            }
            if upload && widgets::primary_button(ui, Some(Icon::Upload), "Upload file").clicked() {
                commands.push(AppCommand::ChooseUploadFile);
            }
        };
        // Wraps like the mockup's flex row: what doesn't fit beside the title
        // moves to the next line, Upload file first.
        let upload = widgets::button_width(ui, Some(Icon::Upload), Some("Upload file")) + 8.0;
        let toggles = buttons - upload;
        let title_width = ui
            .painter()
            .layout_no_wrap(
                "Everything you've captured or uploaded".into(),
                egui::FontId::proportional(tokens::BODY_SIZE),
                egui::Color32::PLACEHOLDER,
            )
            .size()
            .x
            + 16.0;
        let room = ui.available_width();
        if room >= title_width + buttons {
            ui.horizontal_top(|ui| {
                title(ui);
                ui.add_space(ui.available_width() - buttons);
                actions(ui, true, true);
            });
        } else if room >= title_width + toggles {
            ui.horizontal_top(|ui| {
                title(ui);
                ui.add_space(ui.available_width() - toggles);
                actions(ui, true, false);
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| actions(ui, false, true));
        } else {
            title(ui);
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| actions(ui, true, true));
        }
        ui.add_space(16.0);
        if prefs != preferences {
            commands.push(AppCommand::SetHistoryPreferences(prefs));
        }
        commands
    }

    pub fn draw(&mut self, ui: &mut Ui, view: &DesktopView) -> Vec<AppCommand> {
        self.queue_paused = view.queue_paused;
        self.progress = view
            .transfers
            .iter()
            .filter_map(|(id, (sent, _))| {
                let job = view.jobs.iter().find(|j| j.id == *id)?;
                Some((*id, *sent as f32 / job.artifact.size.max(1) as f32))
            })
            .collect();
        let mut commands = vec![];
        let prefs = view
            .settings
            .as_ref()
            .map(|s| s.history.clone())
            .unwrap_or_default();
        if view.history_focus != self.applied_focus {
            self.applied_focus = view.history_focus;
            self.pinned = None;
            if let Some(focus) = view.history_focus {
                self.apply_focus(focus, view, &mut commands);
            }
        }
        if let Some(focus) = self.pinned
            && !focus_exists(focus, view)
        {
            self.pinned = None;
            commands.push(AppCommand::ClearHistoryFocus);
        }
        let (mut changed, search_focused) = self.search_bar(ui);
        changed |= self.filters(ui);
        if let Some(error) = self.date_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
            return commands;
        }
        if changed {
            self.query.after = None;
            self.previous.clear();
            self.selection.clear();
            self.active = None;
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
        // A pinned target that has since arrived on this page (the refresh
        // caught up) becomes the selection.
        if let Some(focus) = self.pinned
            && locate(focus, view).is_some()
        {
            self.pinned = None;
            self.apply_focus(focus, view, &mut commands);
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
        self.last_layout = Some(prefs.layout);
        // The page scrolls as a whole (the shell's ScrollArea), like the mockup;
        // a page holds at most HISTORY_PAGE_SIZE records, so no virtualizing.
        let wide_window = ui.ctx().content_rect().width() >= tokens::COLLAPSE_BELOW;
        if prefs.preview && wide_window && ui.available_width() >= 640.0 {
            let total = ui.available_width();
            let results_width = total - tokens::INSPECTOR_WIDTH - tokens::INSPECTOR_GAP;
            let top = ui.cursor().top();
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let results = ui.allocate_ui_with_layout(
                    egui::vec2(results_width, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(results_width);
                        self.results(ui, records, &prefs, &mut commands)
                    },
                );
                ui.add_space(tokens::INSPECTOR_GAP);
                let edge = ui.cursor().left();
                let inspector = ui.allocate_ui_with_layout(
                    egui::vec2(tokens::INSPECTOR_WIDTH, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::Frame::NONE
                            .inner_margin(egui::Margin {
                                left: tokens::INSPECTOR_GAP as i8,
                                ..Default::default()
                            })
                            .show(ui, |ui| {
                                ui.set_width(tokens::INSPECTOR_WIDTH - tokens::INSPECTOR_GAP);
                                self.inspector(ui, selected, view, &mut commands)
                            });
                    },
                );
                let bottom = results
                    .response
                    .rect
                    .bottom()
                    .max(inspector.response.rect.bottom());
                ui.painter().vline(
                    edge,
                    top..=bottom,
                    ui.visuals().widgets.noninteractive.bg_stroke,
                );
            });
        } else {
            self.results(ui, records, &prefs, &mut commands);
            if prefs.preview {
                ui.add_space(14.0);
                divider(ui);
                ui.add_space(14.0);
                self.inspector(ui, selected, view, &mut commands);
            }
        }
        self.footer(ui, records, view.history_page.has_more, &mut commands);

        commands
    }

    /// Selects the focus target if it is on this page, otherwise pins it in the
    /// inspector; a target that no longer exists clears the focus.
    fn apply_focus(
        &mut self,
        focus: HistoryFocus,
        view: &DesktopView,
        commands: &mut Vec<AppCommand>,
    ) {
        if focus == HistoryFocus::InProgress {
            // The In progress section is always at the top of the page.
            commands.push(AppCommand::ClearHistoryFocus);
            return;
        }
        let records = &view.history_page.records;
        if let Some(index) = locate(focus, view) {
            let id = records[index].id;
            self.active = Some(id);
            self.selection = vec![id];
            self.anchor = index;
            self.reveal = true;
            self.last_layout = None;
            commands.push(AppCommand::ClearHistoryFocus);
        } else if focus_exists(focus, view) {
            self.active = None;
            self.selection.clear();
            self.pinned = Some(focus);
        } else {
            commands.push(AppCommand::ClearHistoryFocus);
        }
    }

    /// Search field, status and date combos; returns (query changed, search focused).
    fn search_bar(&mut self, ui: &mut Ui) -> (bool, bool) {
        let mut changed = false;
        let mut focused = false;
        let mut preset = self.date_preset;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.spacing_mut().interact_size.y = tokens::BUTTON_HEIGHT;
            let combo = 128.0;
            let room = ui.available_width() - 2.0 * (combo + 8.0);
            // Narrow: the search takes the whole row and the combos wrap under it.
            let width = if room < 160.0 {
                ui.available_width()
            } else {
                room
            };
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.query.search)
                    .hint_text("Search captures, files, or links")
                    .font(egui::FontId::proportional(tokens::SMALL_SIZE))
                    .margin(egui::Margin {
                        left: 30,
                        right: 10,
                        top: 8,
                        bottom: 8,
                    })
                    .desired_width(width - 40.0)
                    .min_size(egui::vec2(width, tokens::BUTTON_HEIGHT))
                    .char_limit(64),
            );
            let glyph = egui::Rect::from_min_size(
                egui::pos2(search.rect.left() + 10.0, search.rect.center().y - 7.0),
                egui::vec2(14.0, 14.0),
            );
            icons::paint(
                ui.painter(),
                glyph,
                Icon::Search,
                ui.visuals().weak_text_color(),
            );
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::F)) {
                search.request_focus();
            }
            changed = search.changed();
            focused = search.has_focus();
            egui::ComboBox::from_id_salt("history-status")
                .width(combo)
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
            egui::ComboBox::from_id_salt("history-date")
                .width(combo)
                .selected_text(preset.label())
                .show_ui(ui, |ui| {
                    for value in DatePreset::ALL {
                        ui.selectable_value(&mut preset, value, value.label());
                    }
                });
        });
        if preset != self.date_preset {
            self.date_preset = preset;
            self.date_error = None;
            self.since.clear();
            self.until.clear();
            if preset != DatePreset::Custom {
                self.query.since_ms = preset.since_ms();
                self.query.until_ms = None;
                changed = true;
            }
        }
        if self.date_preset == DatePreset::Custom {
            let mut dates_changed = false;
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let muted = ui.visuals().weak_text_color();
                ui.label(
                    egui::RichText::new("From")
                        .size(tokens::SMALL_SIZE)
                        .color(muted),
                );
                dates_changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut self.since)
                            .id(egui::Id::unique("history-since"))
                            .hint_text("YYYY-MM-DD")
                            .desired_width(96.0)
                            .char_limit(10),
                    )
                    .changed();
                ui.label(
                    egui::RichText::new("Through")
                        .size(tokens::SMALL_SIZE)
                        .color(muted),
                );
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
        }
        self.query.search.retain(|c| !c.is_control());
        ui.add_space(10.0);
        (changed, focused)
    }

    /// Type chips, Favorites and the sort combo; returns whether the query changed.
    fn filters(&mut self, ui: &mut Ui) -> bool {
        let mut changed = false;
        let narrow = ui.available_width() < 520.0;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
            for (label, kind) in [
                ("All", HistoryType::All),
                ("Images", HistoryType::Images),
                ("Recordings", HistoryType::Recordings),
                ("Files", HistoryType::Files),
            ] {
                if widgets::chip(ui, self.query.kind == kind, label).clicked()
                    && self.query.kind != kind
                {
                    self.query.kind = kind;
                    changed = true;
                }
            }
            if widgets::chip(ui, self.query.favorites_only, "Favorites")
                .on_hover_text(
                    "Favorites bookmark records. They do not prevent cleanup or change retention.",
                )
                .clicked()
            {
                self.query.favorites_only = !self.query.favorites_only;
                changed = true;
            }
            let mut sort = |ui: &mut Ui| {
                ui.spacing_mut().interact_size.y = tokens::BUTTON_HEIGHT_SMALL;
                egui::ComboBox::from_id_salt("history-sort")
                    .selected_text(
                        egui::RichText::new(sort_label(self.query.sort)).size(tokens::SMALL_SIZE),
                    )
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
            };
            // Wide: sort sits at the right end; narrow: it wraps with the chips.
            if narrow {
                sort(ui);
            } else {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), sort);
            }
        });
        ui.add_space(14.0);
        changed
    }

    fn results(
        &mut self,
        ui: &mut Ui,
        records: &[HistoryRecord],
        prefs: &crate::persistence::settings::HistoryPreferences,
        commands: &mut Vec<AppCommand>,
    ) {
        let visuals = ui.visuals().clone();
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(page_label(records, self.query.sort))
                    .font(tokens::semibold(tokens::BODY_SIZE))
                    .color(visuals.text_color()),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!("{} items", records.len()))
                        .size(tokens::SMALL_SIZE)
                        .color(visuals.weak_text_color()),
                );
            });
        });
        ui.add_space(8.0);
        if records.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(
                        "No matching items. Try another search or clear your filters.",
                    )
                    .color(visuals.weak_text_color()),
                );
            });
            return;
        }
        let gallery = prefs.layout == HistoryLayout::Gallery;
        let width = ui.available_width();
        let gap = tokens::GALLERY_GAP;
        let columns = if gallery {
            // Mockup: 3 columns, 4 without the preview, 2 on narrow pages.
            match (width < tokens::STACK_BELOW - 60.0, prefs.preview) {
                (true, _) => 2,
                (false, true) => 3,
                (false, false) => 4,
            }
        } else {
            1
        };
        let rows = records.len().div_ceil(columns);
        let mut body = |ui: &mut Ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for row in 0..rows {
                let height = match (gallery, row + 1 == rows) {
                    (false, _) => LIST_ROW,
                    (true, true) => CARD_HEIGHT,
                    (true, false) => CARD_HEIGHT + gap,
                };
                let (line, _) =
                    ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
                if gallery {
                    let card_width = (width - gap * (columns - 1) as f32) / columns as f32;
                    for column in 0..columns {
                        let index = row * columns + column;
                        let Some(record) = records.get(index) else {
                            break;
                        };
                        let rect = egui::Rect::from_min_size(
                            egui::pos2(
                                line.left() + column as f32 * (card_width + gap),
                                line.top(),
                            ),
                            egui::vec2(card_width, CARD_HEIGHT),
                        );
                        self.card(ui, rect, record, index, records, commands);
                    }
                } else {
                    let place = Place {
                        index: row,
                        last: row + 1 == records.len(),
                    };
                    self.list_row(ui, line, &records[row], place, records, commands);
                }
            }
        };
        if gallery {
            body(ui);
        } else {
            egui::Frame::NONE
                .stroke(visuals.widgets.noninteractive.bg_stroke)
                .corner_radius(egui::CornerRadius::same(tokens::RADIUS_SURFACE))
                .show(ui, &mut body);
        }
    }

    /// Paints a record's thumbnail into `rect`: the cropped preview when one
    /// loaded, else a file glyph; `tags` adds the format/duration tag.
    fn thumbnail(
        &self,
        ui: &Ui,
        rect: egui::Rect,
        record: &HistoryRecord,
        radius: egui::CornerRadius,
        glyph: f32,
        tags: bool,
    ) {
        let visuals = ui.visuals();
        let painter = ui.painter();
        painter.rect_filled(rect, radius, appearance::thumbnail_fill(visuals));
        match self.previews.images.get(&record.id) {
            Some(Some(texture)) if texture.size_vec2().min_elem() > 0.0 => {
                let size = texture.size_vec2();
                let scale = (rect.width() / size.x).max(rect.height() / size.y);
                let uv = egui::Rect::from_center_size(
                    egui::pos2(0.5, 0.5),
                    egui::vec2(
                        rect.width() / (size.x * scale),
                        rect.height() / (size.y * scale),
                    ),
                );
                egui::Image::new(texture)
                    .uv(uv)
                    .corner_radius(radius)
                    .paint_at(ui, rect);
            }
            // Still decoding a local image: keep the backdrop quiet.
            None if record.local_available && record.kind == "screenshot" => {}
            _ => icons::paint(
                painter,
                egui::Rect::from_center_size(rect.center(), egui::vec2(glyph, glyph)),
                Icon::File,
                visuals.weak_text_color(),
            ),
        }
        if !tags {
            return;
        }
        let tag = match &record.recording {
            Some(meta) => {
                let seconds = meta.active_millis / 1000;
                Some(format!(
                    "{} {:02}:{:02}",
                    format_tag(record),
                    seconds / 60,
                    seconds % 60
                ))
            }
            None if !record.local_available => Some("File missing".to_owned()),
            None if record.kind != "screenshot" => Some(format_tag(record)),
            None => None,
        };
        if let Some(tag) = tag {
            let galley =
                painter.layout_no_wrap(tag, egui::FontId::proportional(10.0), visuals.text_color());
            let size = galley.size() + egui::vec2(10.0, 2.0);
            let pill =
                egui::Rect::from_min_size(rect.right_bottom() - size - egui::vec2(6.0, 6.0), size);
            painter.rect(
                pill,
                egui::CornerRadius::same(4),
                visuals.extreme_bg_color,
                visuals.widgets.noninteractive.bg_stroke,
                egui::StrokeKind::Inside,
            );
            painter.galley(
                pill.min + egui::vec2(5.0, 1.0),
                galley,
                visuals.text_color(),
            );
        }
    }

    fn card(
        &mut self,
        ui: &mut Ui,
        rect: egui::Rect,
        record: &HistoryRecord,
        index: usize,
        records: &[HistoryRecord],
        commands: &mut Vec<AppCommand>,
    ) {
        let id = ui.make_persistent_id(("history-card", record.id));
        let response = ui.interact(rect, id, egui::Sense::click());
        let active = self.active == Some(record.id);
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::Button, true, active, &record.name)
        });
        let visuals = ui.visuals().clone();
        let radius = egui::CornerRadius::same(tokens::RADIUS_SURFACE);
        ui.painter()
            .rect_filled(rect, radius, visuals.extreme_bg_color);
        let thumb =
            egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), tokens::THUMB_HEIGHT));
        let top = egui::CornerRadius {
            nw: tokens::RADIUS_SURFACE,
            ne: tokens::RADIUS_SURFACE,
            sw: 0,
            se: 0,
        };
        self.thumbnail(ui, thumb, record, top, 28.0, true);
        let name = elided(
            ui,
            &record.name,
            tokens::medium(tokens::SMALL_SIZE),
            rect.width() - 18.0,
        );
        let painter = ui.painter();
        let name_top = thumb.bottom() + 8.0;
        let name_height = name.size().y;
        painter.galley(
            egui::pos2(rect.left() + 9.0, name_top),
            name,
            visuals.text_color(),
        );
        let meta_y = name_top + name_height + 4.0 + 7.0;
        let (status, color) = self.status(ui, record);
        painter.circle_filled(egui::pos2(rect.left() + 12.0, meta_y), 3.0, color);
        painter.text(
            egui::pos2(rect.left() + 20.0, meta_y),
            egui::Align2::LEFT_CENTER,
            status,
            egui::FontId::proportional(tokens::META_SIZE),
            color,
        );
        painter.text(
            egui::pos2(rect.right() - 9.0, meta_y),
            egui::Align2::RIGHT_CENTER,
            format_size(record.size),
            egui::FontId::proportional(tokens::META_SIZE),
            visuals.weak_text_color(),
        );
        let border = if active {
            egui::Stroke::new(2.0, visuals.selection.bg_fill)
        } else if response.hovered() {
            egui::Stroke::new(1.0, appearance::card_hover_border(&visuals))
        } else {
            visuals.widgets.noninteractive.bg_stroke
        };
        painter.rect_stroke(rect, radius, border, egui::StrokeKind::Inside);
        // The star sits over the card, so it takes clicks before the card does.
        let star_rect = egui::Rect::from_min_size(
            egui::pos2(thumb.right() - 22.0, thumb.top() + 3.0),
            egui::vec2(19.0, 19.0),
        );
        let star = ui
            .interact(star_rect, id.with("star"), egui::Sense::click())
            .on_hover_text(if record.favorite {
                "Remove from favorites"
            } else {
                "Add to favorites"
            });
        star.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::CheckBox, true, record.favorite, "Favorite")
        });
        if record.favorite || response.hovered() || star.hovered() {
            let color = if record.favorite {
                appearance::warning(&visuals)
            } else {
                visuals.weak_text_color()
            };
            icons::paint(ui.painter(), star_rect.shrink(3.0), Icon::Star, color);
        }
        if star.clicked() {
            commands.push(AppCommand::SetHistoryFavorite {
                id: record.id,
                favorite: !record.favorite,
            });
        }
        self.use_hit(ui, &response, record, index, records, commands);
    }

    fn list_row(
        &mut self,
        ui: &mut Ui,
        rect: egui::Rect,
        record: &HistoryRecord,
        place: Place,
        records: &[HistoryRecord],
        commands: &mut Vec<AppCommand>,
    ) {
        let id = ui.make_persistent_id(("history-row", record.id));
        let response = ui.interact(rect, id, egui::Sense::click());
        let active = self.active == Some(record.id);
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::Button, true, active, &record.name)
        });
        let visuals = ui.visuals().clone();
        if active {
            ui.painter().rect_filled(rect, 0.0, active_fill(ui));
        }
        let thumb = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 10.0, rect.center().y - 17.0),
            egui::vec2(34.0, 34.0),
        );
        self.thumbnail(ui, thumb, record, egui::CornerRadius::same(4), 16.0, false);
        let size_left = rect.right() - 10.0 - 64.0;
        let status_left = size_left - 10.0 - 110.0;
        let text_left = thumb.right() + 10.0;
        let name = elided(
            ui,
            &record.name,
            tokens::medium(tokens::SMALL_SIZE),
            (status_left - 10.0 - text_left).max(40.0),
        );
        let painter = ui.painter();
        let name_height = name.size().y;
        let sub = format!(
            "{}, {}",
            created_label(record.created_ms),
            format_tag(record)
        );
        let top = rect.center().y - (name_height + 14.0) / 2.0;
        painter.galley(egui::pos2(text_left, top), name, visuals.text_color());
        painter.text(
            egui::pos2(text_left, top + name_height + 1.0),
            egui::Align2::LEFT_TOP,
            sub,
            egui::FontId::proportional(tokens::META_SIZE),
            visuals.weak_text_color(),
        );
        let (status, color) = self.status(ui, record);
        painter.circle_filled(egui::pos2(status_left + 3.0, rect.center().y), 3.0, color);
        painter.text(
            egui::pos2(status_left + 11.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            status,
            egui::FontId::proportional(tokens::META_SIZE),
            color,
        );
        painter.text(
            egui::pos2(rect.right() - 10.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            format_size(record.size),
            egui::FontId::proportional(tokens::META_SIZE),
            visuals.weak_text_color(),
        );
        if !place.last {
            painter.hline(
                rect.x_range(),
                rect.bottom() - 0.5,
                visuals.widgets.noninteractive.bg_stroke,
            );
        }
        self.use_hit(ui, &response, record, place.index, records, commands);
    }

    /// Card/list status text and dot color, e.g. ("Uploading, 62%", lavender).
    fn status(&self, ui: &Ui, record: &HistoryRecord) -> (String, egui::Color32) {
        let label = self.status_line(record);
        let text = match label {
            "Failed" => "Upload failed".to_owned(),
            "Uploading" => record
                .job
                .as_ref()
                .and_then(|job| self.progress.get(&job.id))
                .map_or_else(
                    || "Uploading".to_owned(),
                    |p| format!("Uploading, {:.0}%", p * 100.0),
                ),
            other => other.to_owned(),
        };
        (text, status_color(ui, label))
    }

    fn inspector(
        &mut self,
        ui: &mut Ui,
        record: Option<&HistoryRecord>,
        view: &DesktopView,
        commands: &mut Vec<AppCommand>,
    ) {
        let visuals = ui.visuals().clone();
        let muted = visuals.weak_text_color();
        let Some(record) = record else {
            if let Some(focus) = self.pinned {
                self.pinned_inspector(ui, focus, view, commands);
            } else {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new(
                            "Select a capture or file to see its preview and actions.",
                        )
                        .color(muted),
                    );
                });
            }
            return;
        };
        let width = ui.available_width();
        let (preview, _) = ui.allocate_exact_size(
            egui::vec2(width, tokens::PREVIEW_HEIGHT),
            egui::Sense::hover(),
        );
        let radius = egui::CornerRadius::same(tokens::RADIUS_SURFACE);
        self.thumbnail(ui, preview, record, radius, 40.0, false);
        ui.painter().rect_stroke(
            preview,
            radius,
            visuals.widgets.noninteractive.bg_stroke,
            egui::StrokeKind::Inside,
        );
        ui.add_space(12.0);
        ui.add(
            egui::Label::new(
                egui::RichText::new(&record.name)
                    .font(tokens::semibold(15.0))
                    .color(visuals.text_color()),
            )
            .wrap(),
        );
        ui.add_space(2.0);
        let mut sub = format!(
            "{}, {}, {}",
            created_label(record.created_ms),
            format_tag(record),
            format_size(record.size)
        );
        if let Some(meta) = &record.recording {
            sub.push_str(&format!(", {} × {}", meta.width, meta.height));
        }
        ui.label(
            egui::RichText::new(sub)
                .size(tokens::SMALL_SIZE)
                .color(muted),
        );
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            let (status, color) = self.status(ui, record);
            status_dot(ui, &status, color);
            ui.label(
                egui::RichText::new(if record.local_available {
                    "Local file available"
                } else {
                    "Local file missing"
                })
                .size(tokens::SMALL_SIZE)
                .color(muted),
            );
        });
        ui.add_space(12.0);
        divider(ui);
        ui.add_space(12.0);
        let url = share_url(record);
        let placeholder = match record.job.as_ref().map(|j| &j.status) {
            None => "Not uploaded",
            Some(JobStatus::Failed { .. }) => "No link yet",
            _ => "The link appears when the upload finishes",
        };
        egui::Frame::NONE
            .fill(visuals.extreme_bg_color)
            .stroke(visuals.widgets.noninteractive.bg_stroke)
            .corner_radius(egui::CornerRadius::same(tokens::RADIUS_CONTROL))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                match &url {
                    Some(url) => ui.add(
                        egui::Label::new(egui::RichText::new(url).size(tokens::SMALL_SIZE))
                            .selectable(true)
                            .truncate(),
                    ),
                    None => ui.label(
                        egui::RichText::new(placeholder)
                            .size(tokens::SMALL_SIZE)
                            .color(muted),
                    ),
                };
            });
        ui.add_space(8.0);
        let half = (width - 6.0) / 2.0;
        let third = (width - 12.0) / 3.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if url.is_some() {
                if widgets::wide_button(ui, true, Some(Icon::Copy), "Copy link", half).clicked() {
                    commands.push(action(vec![record.id], HistoryAction::CopyLinks));
                }
                if widgets::wide_button(ui, false, Some(Icon::External), "Open link", half)
                    .clicked()
                {
                    commands.push(action(vec![record.id], HistoryAction::OpenLink));
                }
            } else if record.retryable
                && widgets::wide_button(ui, true, Some(Icon::Retry), "Retry upload", half).clicked()
            {
                commands.push(action(vec![record.id], HistoryAction::RetryFailed));
            }
        });
        ui.add_space(6.0);
        let open =
            record.local_available && AppController::viewer_extension(&record.artifact).is_some();
        let pin = record.local_available && crate::pin::source::is_raster_name(&record.name);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for (icon, label, enabled, kind) in [
                (Icon::File, "Open", open, HistoryAction::OpenLocal),
                (
                    Icon::Folder,
                    "Folder",
                    record.local_available,
                    HistoryAction::ShowFolder,
                ),
                (Icon::Pin, "Pin", pin, HistoryAction::Pin),
            ] {
                let clicked = ui
                    .add_enabled_ui(enabled, |ui| {
                        widgets::wide_button(ui, false, Some(icon), label, third).clicked()
                    })
                    .inner;
                if clicked {
                    commands.push(action(vec![record.id], kind));
                }
            }
        });
        let runner = view.task_workflows.iter().find(|r| {
            r.context()
                .artifact
                .as_ref()
                .is_some_and(|a| a.id == record.artifact.id)
        });
        if let Some(runner) = runner {
            what_ran(ui, runner, commands);
        }
        let member = record
            .job
            .as_ref()
            .is_some_and(|job| view.batch_summary.iter().any(|row| row.id == job.id));
        if member {
            batch_section(ui, view, commands);
        }
    }

    /// The inspector for a focus target that isn't on this page of History.
    fn pinned_inspector(
        &mut self,
        ui: &mut Ui,
        focus: HistoryFocus,
        view: &DesktopView,
        commands: &mut Vec<AppCommand>,
    ) {
        let visuals = ui.visuals().clone();
        let title = |ui: &mut Ui, name: &str| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(name)
                        .font(tokens::semibold(15.0))
                        .color(visuals.text_color()),
                )
                .wrap(),
            );
            ui.label(
                egui::RichText::new("Not on this page of History")
                    .size(tokens::SMALL_SIZE)
                    .color(visuals.weak_text_color()),
            );
        };
        let artifact_name = |runner: &crate::workflow::runner::TaskRunner| {
            runner
                .context()
                .artifact
                .as_ref()
                .map_or("Capture".to_owned(), |a| a.display_name.clone())
        };
        match focus {
            HistoryFocus::Workflow(id) => {
                if let Some(runner) = workflow(view, id) {
                    title(ui, &artifact_name(runner));
                    what_ran(ui, runner, commands);
                }
            }
            HistoryFocus::Batch(_) => {
                title(ui, "Upload batch");
                batch_section(ui, view, commands);
            }
            HistoryFocus::Artifact(artifact) => {
                if let Some(job) = view.jobs.iter().find(|j| j.artifact.id == artifact) {
                    title(ui, &job.artifact.display_name);
                    let label = job_status(Some(&job.status), job.paused, view.queue_paused);
                    let color = status_color(ui, label);
                    status_dot(ui, label, color);
                } else if let Some(runner) = view.task_workflows.iter().find(|r| {
                    r.context()
                        .artifact
                        .as_ref()
                        .is_some_and(|a| a.id == artifact)
                }) {
                    title(ui, &artifact_name(runner));
                    what_ran(ui, runner, commands);
                }
            }
            HistoryFocus::InProgress => {}
        }
    }

    fn footer(
        &mut self,
        ui: &mut Ui,
        records: &[HistoryRecord],
        has_more: bool,
        commands: &mut Vec<AppCommand>,
    ) {
        if self.selection.len() > 1 {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(
                    egui::RichText::new(format!("{} selected", self.selection.len()))
                        .size(tokens::SMALL_SIZE),
                );
                if widgets::toggle_button(ui, false, Some(Icon::Copy), "Copy links").clicked() {
                    commands.push(action(self.selection.clone(), HistoryAction::CopyLinks));
                }
                if widgets::toggle_button(ui, false, Some(Icon::Retry), "Retry failed").clicked() {
                    commands.push(action(self.selection.clone(), HistoryAction::RetryFailed));
                }
                if widgets::toggle_button(ui, false, None, "Remove from history").clicked() {
                    self.remove = Some(self.selection.clone());
                }
                if widgets::ghost_button(ui, None, "Clear selection").clicked() {
                    self.selection.clear();
                    self.active = None;
                }
            });
        }
        ui.add_space(8.0);
        divider(ui);
        ui.add_space(4.0);
        let muted = ui.visuals().weak_text_color();
        let narrow = ui.available_width() < 520.0;
        ui.horizontal_wrapped(|ui| {
            let mut summary = format!("{} items", records.len());
            let busy = page_in_progress(records, self.queue_paused);
            if busy > 0 {
                summary.push_str(&format!(", {busy} uploading"));
            }
            let failed = page_failed(records);
            if failed > 0 {
                summary.push_str(&format!(", {failed} failed"));
            }
            ui.label(
                egui::RichText::new(summary)
                    .size(tokens::SMALL_SIZE)
                    .color(muted),
            );
            // An explicit, focusable paste target: Ctrl+V uploads only from here.
            let zone = ui
                .add(
                    egui::Label::new(
                        egui::RichText::new("· Click here, then Ctrl+V to upload the clipboard")
                            .size(tokens::SMALL_SIZE)
                            .color(muted),
                    )
                    .sense(egui::Sense::click()),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if zone.clicked() {
                zone.request_focus();
            }
            if zone.has_focus()
                && ui.input(|i| {
                    i.events.iter().any(|e| matches!(e, egui::Event::Paste(_)))
                        || i.modifiers.ctrl && i.key_pressed(egui::Key::V)
                })
            {
                commands.push(AppCommand::UploadClipboard);
            }
            if !narrow {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.paging(ui, records, has_more, commands)
                });
            }
        });
        if narrow {
            ui.horizontal(|ui| self.paging(ui, records, has_more, commands));
        }
    }
    /// Previous / "Page N" / Next, laid out in whatever direction `ui` runs.
    fn paging(
        &mut self,
        ui: &mut Ui,
        records: &[HistoryRecord],
        has_more: bool,
        commands: &mut Vec<AppCommand>,
    ) {
        let rtl = ui.layout().main_dir() == egui::Direction::RightToLeft;
        let muted = ui.visuals().weak_text_color();
        let page = format!("Page {}", self.previous.len() + 1);
        let mut steps: [(&str, bool); 3] = [
            ("Previous", !self.previous.is_empty()),
            ("", true),
            ("Next", has_more),
        ];
        if rtl {
            steps.reverse();
        }
        for (label, enabled) in steps {
            if label.is_empty() {
                ui.label(
                    egui::RichText::new(&page)
                        .size(tokens::SMALL_SIZE)
                        .color(muted),
                );
                continue;
            }
            let clicked = ui
                .add_enabled_ui(enabled, |ui| {
                    widgets::ghost_button(ui, None, label).clicked()
                })
                .inner;
            if clicked && label == "Next" {
                self.show_next(records, commands);
            } else if clicked {
                self.show_previous(commands);
            }
        }
    }
    fn show_previous(&mut self, commands: &mut Vec<AppCommand>) {
        self.query.after = self.previous.pop().unwrap();
        commands.push(AppCommand::QueryHistory(self.query.clone()));
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
        self.anchor = 0;
        self.last_layout = None;
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
        if self.reveal && self.active == Some(record.id) {
            // None: scroll only if the card is not already visible.
            response.scroll_to_me(None);
            self.reveal = false;
        }
        if response.clicked() {
            if self.pinned.take().is_some() {
                commands.push(AppCommand::ClearHistoryFocus);
            }
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
        response.context_menu(|ui| {
            item_actions(
                ui,
                record,
                commands,
                ItemState {
                    remove: &mut self.remove,
                    delete_local: &mut self.delete_local,
                    selection: &mut self.selection,
                },
            )
        });
    }
    fn status_line(&self, record: &HistoryRecord) -> &'static str {
        job_status(
            record.job.as_ref().map(|job| &job.status),
            record.job.as_ref().is_some_and(|job| job.paused),
            self.queue_paused,
        )
    }
}

use super::{
    icons::{self, Icon},
    tokens, widgets,
};
use crate::platform::windows::appearance;

/// Card: 104 thumbnail + 8 + name + 4 + meta + 8.
const CARD_HEIGHT: f32 = tokens::THUMB_HEIGHT + 8.0 + 16.0 + 4.0 + 14.0 + 8.0;
const LIST_ROW: f32 = 46.0;

/// A list row's index on the page and whether it is the last one.
#[derive(Clone, Copy)]
struct Place {
    index: usize,
    last: bool,
}

/// What the context menu's Remove / Delete local copy actions change.
struct ItemState<'a> {
    remove: &'a mut Option<Vec<Uuid>>,
    delete_local: &'a mut Option<Vec<Uuid>>,
    selection: &'a mut Vec<Uuid>,
}

/// Where a focus target sits on the current page of records, if it does.
fn locate(focus: HistoryFocus, view: &DesktopView) -> Option<usize> {
    let records = &view.history_page.records;
    match focus {
        HistoryFocus::InProgress => None,
        HistoryFocus::Artifact(artifact) => records.iter().position(|r| r.artifact.id == artifact),
        HistoryFocus::Workflow(id) => workflow(view, id)
            .and_then(|r| r.context().artifact.as_ref())
            .and_then(|a| records.iter().position(|r| r.artifact.id == a.id)),
        HistoryFocus::Batch(job) => records
            .iter()
            .position(|r| r.job.as_ref().is_some_and(|j| j.id == job)),
    }
}

fn workflow(view: &DesktopView, id: Uuid) -> Option<&crate::workflow::runner::TaskRunner> {
    view.task_workflows.iter().find(|r| r.context().id == id)
}

/// Whether the data a focus points at is still in the view.
fn focus_exists(focus: HistoryFocus, view: &DesktopView) -> bool {
    match focus {
        HistoryFocus::InProgress => true,
        HistoryFocus::Workflow(id) => workflow(view, id).is_some(),
        HistoryFocus::Batch(job) => view.batch_summary.iter().any(|row| row.id == job),
        HistoryFocus::Artifact(artifact) => {
            view.history_page
                .records
                .iter()
                .any(|r| r.artifact.id == artifact)
                || view.jobs.iter().any(|j| j.artifact.id == artifact)
                || view.task_workflows.iter().any(|r| {
                    r.context()
                        .artifact
                        .as_ref()
                        .is_some_and(|a| a.id == artifact)
                })
        }
    }
}

fn divider(ui: &mut Ui) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        ui.visuals().widgets.noninteractive.bg_stroke,
    );
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(14.0);
    divider(ui);
    ui.add_space(12.0);
    ui.label(
        egui::RichText::new(title)
            .font(tokens::semibold(tokens::SMALL_SIZE))
            .color(ui.visuals().text_color()),
    );
    ui.add_space(8.0);
}

fn status_dot(ui: &mut Ui, label: &str, color: egui::Color32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        let (dot, _) = ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
        ui.painter().circle_filled(dot.center(), 3.0, color);
        ui.label(
            egui::RichText::new(label)
                .size(tokens::SMALL_SIZE)
                .color(color),
        );
    });
}

/// "What ran after capture": each step with its outcome; failed steps keep
/// the Retry/Skip commands the old Task results page sent.
fn what_ran(
    ui: &mut Ui,
    runner: &crate::workflow::runner::TaskRunner,
    commands: &mut Vec<AppCommand>,
) {
    use crate::workflow::{runner::TaskCommand, steps::StepOutcome};
    section(ui, "What ran after capture");
    let visuals = ui.visuals().clone();
    let id = runner.context().id;
    for state in runner.states() {
        let label = state.step.label();
        let (icon, color, text) = match &state.outcome {
            StepOutcome::Succeeded => {
                (Icon::Check, appearance::success(&visuals), label.to_owned())
            }
            StepOutcome::Failed(reason) | StepOutcome::NeedsUserAction(reason) => (
                Icon::Alert,
                visuals.error_fg_color,
                format!("{label}: {reason}"),
            ),
            StepOutcome::Skipped(reason) => (
                Icon::X,
                visuals.weak_text_color(),
                format!("{label}: {reason}"),
            ),
            StepOutcome::Running => (
                Icon::Play,
                visuals.weak_text_color(),
                format!("{label} (running)"),
            ),
            StepOutcome::Pending => (Icon::X, visuals.weak_text_color(), label.to_owned()),
        };
        let stuck = matches!(
            state.outcome,
            StepOutcome::Failed(_) | StepOutcome::NeedsUserAction(_)
        );
        let text_color = if matches!(state.outcome, StepOutcome::Succeeded) {
            visuals.text_color()
        } else {
            color
        };
        ui.push_id(label, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let (glyph, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                icons::paint(ui.painter(), glyph, icon, color);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if stuck {
                        if state.step.can_skip()
                            && widgets::ghost_button(ui, None, "Skip").clicked()
                        {
                            commands.push(AppCommand::Task {
                                id,
                                command: TaskCommand::Skip(state.step),
                            });
                        }
                        if widgets::toggle_button(ui, false, None, "Retry").clicked() {
                            commands.push(AppCommand::Task {
                                id,
                                command: TaskCommand::Retry(state.step),
                            });
                        }
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(text)
                                    .size(tokens::SMALL_SIZE)
                                    .color(text_color),
                            )
                            .wrap(),
                        );
                    });
                });
            });
        });
        ui.add_space(6.0);
    }
}

/// "Batch": the uploads accepted together with this one, with their links.
fn batch_section(ui: &mut Ui, view: &DesktopView, commands: &mut Vec<AppCommand>) {
    section(ui, "Batch");
    let visuals = ui.visuals().clone();
    let jobs: Vec<_> = view
        .batch_summary
        .iter()
        .filter_map(|row| row.job.as_ref())
        .collect();
    let uploaded = jobs
        .iter()
        .filter(|j| matches!(j.status, JobStatus::Completed(_)))
        .count();
    let failed = jobs
        .iter()
        .filter(|j| matches!(j.status, JobStatus::Failed { .. }))
        .count();
    let waiting = jobs
        .iter()
        .filter(|j| !j.status.is_terminal() && !matches!(j.status, JobStatus::Failed { .. }))
        .count();
    ui.label(
        egui::RichText::new(format!(
            "{} files uploaded together",
            view.batch_summary.len()
        ))
        .font(tokens::medium(tokens::SMALL_SIZE))
        .color(visuals.text_color()),
    );
    let mut counts = format!("{uploaded} uploaded");
    if failed > 0 {
        counts.push_str(&format!(", {failed} failed"));
    }
    if waiting > 0 {
        counts.push_str(&format!(", {waiting} waiting"));
    }
    // Jobs whose records are gone (removed from History) still count in the total.
    let missing = view.batch_summary.len() - jobs.len();
    if missing > 0 {
        counts.push_str(&format!(", {missing} unavailable"));
    }
    ui.label(
        egui::RichText::new(counts)
            .size(tokens::SMALL_SIZE)
            .color(visuals.weak_text_color()),
    );
    ui.add_space(8.0);
    let clicked = ui
        .add_enabled_ui(uploaded > 0, |ui| {
            widgets::toggle_button(ui, false, Some(Icon::Copy), "Copy uploaded links").clicked()
        })
        .inner;
    if clicked {
        commands.push(AppCommand::CopyBatchLinks);
    }
}

/// One line of text cut to `width` with an ellipsis.
fn elided(ui: &Ui, text: &str, font: egui::FontId, width: f32) -> std::sync::Arc<egui::Galley> {
    let mut job =
        egui::text::LayoutJob::simple_singleline(text.to_owned(), font, ui.visuals().text_color());
    job.wrap = egui::text::TextWrapping {
        max_width: width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

/// "PNG", "MP4", "ZIP": the file extension, upper-cased.
fn format_tag(record: &HistoryRecord) -> String {
    std::path::Path::new(&record.name)
        .extension()
        .and_then(|e| e.to_str())
        .map_or_else(|| record.kind.to_uppercase(), str::to_uppercase)
}

/// Heading over the results: "Today", "Today and yesterday", "Nov 3 to Nov 9".
fn page_label(records: &[HistoryRecord], sort: HistorySort) -> String {
    if !matches!(sort, HistorySort::Newest | HistorySort::Oldest) {
        return "Results".into();
    }
    let day = |ms: i64| {
        chrono::DateTime::from_timestamp_millis(ms)
            .map(|d| d.with_timezone(&chrono::Local).date_naive())
    };
    let days: Vec<_> = records.iter().filter_map(|r| day(r.created_ms)).collect();
    let (Some(first), Some(last)) = (days.iter().min(), days.iter().max()) else {
        return "Results".into();
    };
    let today = chrono::Local::now().date_naive();
    let name = |d: &chrono::NaiveDate| {
        if *d == today {
            "Today".to_owned()
        } else if Some(*d) == today.pred_opt() {
            "Yesterday".to_owned()
        } else {
            d.format("%b %-d").to_string()
        }
    };
    if first == last {
        name(first)
    } else if *last == today && Some(*first) == today.pred_opt() {
        "Today and yesterday".into()
    } else {
        format!("{} to {}", name(first), name(last))
    }
}

/// "Today, 23:04", "Yesterday, 20:32", else "2026-11-03, 09:12".
fn created_label(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|date| {
            let local = date.with_timezone(&chrono::Local);
            let today = chrono::Local::now().date_naive();
            let day = if local.date_naive() == today {
                "Today".to_owned()
            } else if Some(local.date_naive()) == today.pred_opt() {
                "Yesterday".to_owned()
            } else {
                local.format("%Y-%m-%d").to_string()
            };
            format!("{day}, {}", local.format("%H:%M"))
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
fn status_color(ui: &Ui, label: &str) -> egui::Color32 {
    let visuals = ui.visuals();
    if visuals.override_text_color.is_some() {
        return visuals.text_color();
    }
    match label {
        "Uploaded" => appearance::success(visuals),
        "Failed" | "Cancelled" => visuals.error_fg_color,
        "Uploading" | "Pausing" | "Waiting" => visuals.selection.bg_fill,
        "Uncertain" => appearance::warning(visuals),
        _ if label.starts_with("Paused") => appearance::warning(visuals),
        _ => visuals.weak_text_color(),
    }
}
fn active_fill(ui: &Ui) -> egui::Color32 {
    let color = ui.visuals().selection.bg_fill;
    if ui.visuals().override_text_color.is_some() {
        color.linear_multiply(0.55)
    } else {
        color.linear_multiply(0.12)
    }
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
fn item_actions(ui: &mut Ui, r: &HistoryRecord, commands: &mut Vec<AppCommand>, state: ItemState) {
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
    let favorite = if r.favorite {
        "Remove from favorites"
    } else {
        "Add to favorites"
    };
    if ui.button(favorite).clicked() {
        commands.push(AppCommand::SetHistoryFavorite {
            id: r.id,
            favorite: !r.favorite,
        });
    }
    ui.separator();
    if ui.button("Remove from history...").clicked() {
        *state.remove = Some(vec![r.id]);
    }
    if r.local_available && ui.button("Delete local copy...").clicked() {
        // The confirmation stays only while this record is the selection.
        *state.selection = vec![r.id];
        *state.delete_local = Some(vec![r.id]);
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
        HistoryStatus::Any => "All statuses",
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

    use crate::{
        app::events::HistoryFocus,
        ui::fixtures::{MockupVariant, mockup_view},
    };

    /// Renders the History header and page twice (fonts settle on the second)
    /// at 1180×900 and returns the last output and the page's commands.
    fn render(pane: &mut HistoryPane, view: &DesktopView) -> (egui::FullOutput, Vec<AppCommand>) {
        let ctx = egui::Context::default();
        crate::platform::windows::appearance::install_fonts(&ctx);
        ctx.set_visuals(crate::platform::windows::appearance::visuals());
        let mut last = None;
        let mut commands = vec![];
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1180.0, 900.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                commands.extend(pane.draw_header(ui, view));
                commands.extend(pane.draw(ui, view));
            });
            output.textures_delta.clear();
            last = Some(output);
        }
        (last.expect("two frames ran"), commands)
    }
    fn texts(output: &egui::FullOutput) -> Vec<String> {
        output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect()
    }
    fn busy() -> DesktopView {
        let mut view = mockup_view(MockupVariant::HistoryBusy);
        view.history_focus = None;
        view
    }
    fn select(view: &DesktopView, index: usize) -> HistoryPane {
        let id = view.history_page.records[index].id;
        HistoryPane {
            active: Some(id),
            selection: vec![id],
            ..Default::default()
        }
    }

    #[test]
    fn inspector_shows_what_ran_after_capture_for_the_selected_item() {
        let view = busy();
        let (output, _) = render(&mut select(&view, 0), &view);
        let text = texts(&output);
        assert!(
            text.contains(&"What ran after capture".to_owned()),
            "{text:?}"
        );
        assert!(text.iter().any(|t| t.contains("Upload")), "{text:?}");
        let (output, _) = render(&mut select(&view, 1), &view);
        assert!(!texts(&output).contains(&"What ran after capture".to_owned()));
    }

    #[test]
    fn inspector_shows_batch_only_for_batch_members() {
        let view = busy();
        let batch: Vec<_> = view.batch_summary.iter().map(|row| row.id).collect();
        let records = &view.history_page.records;
        let member = records
            .iter()
            .position(|r| r.job.as_ref().is_some_and(|j| batch.contains(&j.id)))
            .expect("a batch member on the page");
        let outsider = records
            .iter()
            .position(|r| !r.job.as_ref().is_some_and(|j| batch.contains(&j.id)))
            .expect("a record outside the batch");
        let (output, _) = render(&mut select(&view, member), &view);
        assert!(texts(&output).contains(&"Batch".to_owned()));
        let (output, _) = render(&mut select(&view, outsider), &view);
        assert!(!texts(&output).contains(&"Batch".to_owned()));
    }

    #[test]
    fn workflow_focus_pins_an_item_not_on_this_page() {
        let mut view = busy();
        // The attention workflow's capture is not a History record yet.
        let runner = &view.task_workflows[0];
        let name = runner
            .context()
            .artifact
            .as_ref()
            .unwrap()
            .display_name
            .clone();
        assert!(view.history_page.records.iter().all(|r| r.name != name));
        view.history_focus = Some(HistoryFocus::Workflow(runner.context().id));
        let mut pane = HistoryPane::default();
        let (output, commands) = render(&mut pane, &view);
        let text = texts(&output);
        assert!(text.contains(&name), "{text:?}");
        assert!(text.contains(&"What ran after capture".to_owned()));
        assert!(
            !commands
                .iter()
                .any(|c| matches!(c, AppCommand::ClearHistoryFocus))
        );
    }

    #[test]
    fn pinned_focus_selects_the_record_once_it_reaches_the_page() {
        let mut view = busy();
        let runner = &view.task_workflows[0];
        let id = runner.context().id;
        let artifact = (**runner.context().artifact.as_ref().unwrap()).clone();
        view.history_focus = Some(HistoryFocus::Workflow(id));
        let mut pane = HistoryPane::default();
        render(&mut pane, &view);
        assert_eq!(pane.pinned, Some(HistoryFocus::Workflow(id)));
        // The history refresh catches up: the capture is now a record on page 1.
        let mut record = view.history_page.records[0].clone();
        record.id = Uuid::new_v4();
        record.artifact = artifact;
        let record_id = record.id;
        view.history_page.records.insert(0, record);
        let (_, commands) = render(&mut pane, &view);
        assert_eq!(pane.pinned, None);
        assert_eq!(pane.active, Some(record_id));
        assert!(
            commands
                .iter()
                .any(|c| matches!(c, AppCommand::ClearHistoryFocus))
        );
    }

    #[test]
    fn vanished_focus_target_clears_focus_without_panic() {
        let mut view = busy();
        view.history_focus = Some(HistoryFocus::Workflow(Uuid::new_v4()));
        let (_, commands) = render(&mut HistoryPane::default(), &view);
        assert!(
            commands
                .iter()
                .any(|c| matches!(c, AppCommand::ClearHistoryFocus))
        );
    }

    #[test]
    fn only_copy_link_and_upload_file_use_the_primary_fill() {
        let view = busy();
        let (output, _) = render(&mut select(&view, 0), &view);
        let primary = crate::platform::windows::appearance::visuals()
            .selection
            .bg_fill;
        let filled = output
            .shapes
            .iter()
            .filter(|clipped| matches!(&clipped.shape, egui::Shape::Rect(r) if r.fill == primary))
            .count();
        assert_eq!(filled, 2);
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
