use super::{
    icons::{self, Icon},
    settings::{SettingsCategory, SettingsPane},
    tokens, widgets,
};
use crate::{
    app::{
        desktop::{DesktopClient, DesktopView},
        error::AppError,
        events::{AppCommand, Lifecycle, QueueAction, Window},
    },
    platform::windows::{appearance, file_dialog::PickerGate, tray::Tray},
};
use eframe::egui;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
pub struct Shell {
    troubleshooting: super::troubleshooting::TroubleshootingPane,
    preferences_transfer: super::settings_transfer::SettingsTransferPane,
    install_review: super::destination_install::InstallReview,
    scrolling_host: super::scrolling_controls::ScrollingHost,
    recording_host: super::recording_overlay::RecordingHost,
    pin_sources: super::pin_sources::PinSources,
    capture_host: super::capture_host::CaptureHost,
    shown_since: Arc<
        std::sync::Mutex<
            std::collections::HashMap<crate::app::notifications::NoticeId, std::time::Instant>,
        >,
    >,
    thumbs: Arc<Mutex<super::completion_notification::NoticeThumbs>>,
    client: DesktopClient,
    view: DesktopView,
    /// `None` only for fixture screenshots, which must not add a tray icon.
    tray: Option<Tray>,
    pane: SettingsPane,
    history: super::history::HistoryPane,
    in_progress_expanded: bool,
    focus_revision: u64,
    current_window: Option<Window>,
    focus_requested: Arc<AtomicBool>,
    /// Shared with the tray's "Upload file" menu item: the two entry points
    /// are single-flight together, not just independently.
    picker_gate: PickerGate,
    picker_requests: u64,
}
impl Shell {
    pub fn new(
        client: DesktopClient,
        focus_requested: Arc<AtomicBool>,
        context: egui::Context,
    ) -> Result<Self, AppError> {
        appearance::install_fonts(&context);
        let picker_gate = PickerGate::default();
        let tray = Tray::new(client.clone(), context, picker_gate.clone())?;
        Ok(Self::assemble(
            client,
            Some(tray),
            focus_requested,
            picker_gate,
        ))
    }
    /// Renders `view` with no controller, runtime or tray icon. Commands are dropped.
    #[doc(hidden)]
    pub fn from_fixture(view: DesktopView, context: egui::Context) -> Self {
        appearance::install_fonts(&context);
        Self::assemble(
            DesktopClient::detached(view),
            None,
            Arc::new(AtomicBool::new(false)),
            PickerGate::default(),
        )
    }
    #[doc(hidden)]
    pub fn select_settings_category(&mut self, category: SettingsCategory) {
        self.pane.select(category);
    }
    fn assemble(
        client: DesktopClient,
        tray: Option<Tray>,
        focus_requested: Arc<AtomicBool>,
        picker_gate: PickerGate,
    ) -> Self {
        Self {
            troubleshooting: Default::default(),
            preferences_transfer: Default::default(),
            install_review: Default::default(),
            scrolling_host: Default::default(),
            recording_host: Default::default(),
            pin_sources: Default::default(),
            capture_host: Default::default(),
            view: client.snapshot(),
            client,
            tray,
            pane: SettingsPane::default(),
            history: Default::default(),
            in_progress_expanded: false,
            shown_since: Default::default(),
            thumbs: Arc::new(Mutex::new(
                super::completion_notification::NoticeThumbs::new(),
            )),
            focus_revision: 0,
            current_window: None,
            focus_requested,
            picker_gate,
            picker_requests: 0,
        }
    }
    fn submit(&mut self, command: AppCommand) {
        // A fixture shell (no tray) has no controller: drop, don't report a failure.
        if self.tray.is_none() {
            return;
        }
        match command {
            AppCommand::ChooseImportConfig => {
                crate::platform::windows::file_dialog::spawn_import_destination(
                    self.client.clone(),
                    self.picker_gate.clone(),
                );
                return;
            }
            AppCommand::ChooseUploadFile => {
                crate::platform::windows::file_dialog::spawn_pick_and_upload(
                    self.client.clone(),
                    self.picker_gate.clone(),
                );
                return;
            }
            AppCommand::ChooseTaskSave { id, name } => {
                crate::platform::windows::file_dialog::spawn_task_save(
                    self.client.clone(),
                    self.picker_gate.clone(),
                    id,
                    name,
                );
                return;
            }
            _ => {}
        }
        if matches!(command, AppCommand::ChooseArchiveFolder) {
            if let Some(settings) = &self.view.settings {
                crate::platform::windows::file_dialog::spawn_pick_archive_folder(
                    self.client.clone(),
                    settings.archive.clone(),
                    settings.revision,
                );
            }
            return;
        }
        let editing = matches!(
            command,
            AppCommand::SaveConnection { .. }
                | AppCommand::TestConnection { .. }
                | AppCommand::TestSavedConnection { .. }
        );
        if let Err(error) = self.client.submit(command) {
            if editing {
                self.pane.submission_failed(error.clone());
            }
            self.client.report_error(error);
        }
    }
}
impl eframe::App for Shell {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.focus_requested.swap(false, Ordering::AcqRel) {
            self.submit(AppCommand::Show(Window::Settings));
        }
        self.view = self.client.snapshot();
        if self.view.window != Some(Window::Settings) {
            self.troubleshooting.cancel_connection();
        }
        self.preferences_transfer.reconcile();
        if let Some(settings) = &self.view.settings {
            self.troubleshooting.poll(settings);
        }
        let install_commands =
            self.install_review
                .show(ctx, self.view.install.as_ref(), self.view.settings.as_ref());
        for command in install_commands {
            self.submit(command);
        }
        self.capture_host.show(ctx, &self.client, &self.view);
        super::screen_sampler::show_host(ctx, &self.client, &self.view);
        self.scrolling_host.show(ctx, &self.client, &self.view);
        self.recording_host.show(ctx, &self.client, &self.view);
        self.pin_sources
            .show(ctx, &self.client, &self.view, &self.picker_gate);
        if self.picker_requests != self.view.picker_requests {
            self.picker_requests = self.view.picker_requests;
            crate::platform::windows::file_dialog::spawn_pick_and_upload_for(
                self.client.clone(),
                self.picker_gate.clone(),
                self.view.picker_selection,
            );
        }
        if let Some(settings) = &self.view.settings {
            ctx.set_zoom_factor(f32::from(settings.appearance.ui_scale_percent) / 100.0);
        }
        // eframe shows its first rendered window; explicitly preserve tray mode
        // even before the initial worker snapshot arrives.
        if self.view.window.is_none() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        if self.view.lifecycle == Lifecycle::Draining {
            self.pane.clear(ctx);
        }
        self.pane.reconcile(&self.view, ctx);
        if let Some(settings) = &self.view.settings {
            if let Some(Err(error)) = self
                .tray
                .as_mut()
                .map(|tray| tray.update_settings(settings))
            {
                self.client.report_error(error);
            }
            if self.current_window != self.view.window {
                self.pane.clear(ctx);
                if self.view.window != Some(Window::History) {
                    self.history.suspend();
                }
                if self.view.window == Some(Window::Settings) {
                    self.pane.opened(settings);
                }
                self.current_window = self.view.window;
            }
        }
        ctx.set_visuals(appearance::visuals());
        if ctx.input(|input| input.viewport().close_requested())
            && self.view.lifecycle != Lifecycle::Exited
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.view.lifecycle == Lifecycle::Running {
                self.pane.clear(ctx);
                self.submit(AppCommand::CloseWindow);
            }
        }
        if self.view.focus_revision != self.focus_revision {
            self.focus_revision = self.view.focus_revision;
            let visible = self.view.window.is_some();
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(visible));
            if visible {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }
        if super::completion_notification::surface_visible(&self.view) {
            let client = self.client.clone();
            let shown = self.shown_since.clone();
            let thumbs = self.thumbs.clone();
            ctx.show_viewport_deferred(
                egui::ViewportId::from_hash_of("completion-notices"),
                super::completion_notification::viewport_builder_for(
                    self.view.notices.len(),
                    ctx.pixels_per_point(),
                ),
                move |ui, _class| {
                    // Read the live snapshot: a disabled preference or dismissed
                    // notice must not survive in a deferred callback's clone.
                    let view = client.snapshot();
                    if !super::completion_notification::surface_visible(&view) {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        return;
                    }
                    let mut shown = shown.lock().unwrap_or_else(|e| e.into_inner());
                    let mut thumbs = thumbs.lock().unwrap_or_else(|e| e.into_inner());
                    let commands = if ui.ctx().input(|i| i.viewport().close_requested()) {
                        view.notices
                            .iter()
                            .map(|n| AppCommand::DismissNotice(n.view.id()))
                            .collect()
                    } else {
                        super::completion_notification::draw_with_thumbs(
                            ui,
                            &view,
                            &mut shown,
                            &mut thumbs,
                        )
                    };
                    drop(thumbs);
                    drop(shown);
                    for command in commands {
                        if let Err(error) = client.submit(command) {
                            client.report_error(error);
                        }
                    }
                    // Only the notice surface has a timer; the hidden shell
                    // remains driven by worker events. Stop when it disappears.
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(100));
                },
            );
        } else {
            self.shown_since
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            self.thumbs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
        }
        // Exit follows driver settlement, never a window close or an upload hint.
        if self.view.lifecycle == Lifecycle::Exited && self.view.exit_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
    fn persist_egui_memory(&self) -> bool {
        false
    }
}

impl Shell {
    /// Sidebar plus the current page; submits whatever the user clicked.
    fn draw(&mut self, ui: &mut egui::Ui) {
        let mut commands = vec![];
        let window_width = ui.max_rect().width();
        let collapsed = window_width < tokens::COLLAPSE_BELOW;
        let sidebar_width = if collapsed {
            tokens::SIDEBAR_COLLAPSED
        } else {
            tokens::SIDEBAR_WIDTH
        };
        let visuals = ui.visuals().clone();
        let sidebar = egui::Panel::left("sidebar")
            .exact_size(sidebar_width)
            .resizable(false)
            // egui's separator ignores the border token; draw our own edge below.
            .show_separator_line(false)
            .frame(
                egui::Frame::NONE
                    .fill(visuals.extreme_bg_color)
                    .inner_margin(egui::Margin::symmetric(
                        tokens::SIDEBAR_PAD_X as i8,
                        tokens::SIDEBAR_PAD_Y as i8,
                    )),
            )
            .show(ui, |ui| {
                commands.extend(draw_sidebar(ui, &self.view, collapsed));
            })
            .response
            .rect;
        ui.painter().vline(
            sidebar.right() - 0.5,
            sidebar.y_range(),
            visuals.widgets.noninteractive.bg_stroke,
        );
        let stacked = window_width - sidebar_width < tokens::STACK_BELOW;
        let margin = if stacked {
            egui::Margin::symmetric(14, 16)
        } else {
            egui::Margin {
                left: tokens::PAGE_PAD_X as i8,
                right: tokens::PAGE_PAD_X as i8,
                top: tokens::PAGE_PAD_TOP as i8,
                bottom: tokens::PAGE_PAD_BOTTOM as i8,
            }
        };
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(visuals.panel_fill)
                    .inner_margin(margin),
            )
            .show(ui, |ui| {
                let running = self.view.lifecycle == Lifecycle::Running;
                if self.view.window == Some(Window::History) && self.view.settings.is_some() {
                    ui.add_enabled_ui(running, |ui| {
                        commands.extend(self.history.draw_header(ui, &self.view));
                    });
                }
                if self.view.window == Some(Window::Settings) {
                    page_header(
                        ui,
                        "Settings",
                        "Connections, shortcuts, and what happens after you capture",
                    );
                }
                // The banner stays enabled: Resume uploads must work while draining.
                commands.extend(draw_status_banner(ui, &self.view));
                ui.add_enabled_ui(running, |ui| {
                    self.draw_page(ui, &mut commands);
                });
            });
        for command in commands {
            if matches!(command, AppCommand::Quit) && self.view.lifecycle == Lifecycle::Exited {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                self.submit(command);
            }
        }
    }

    /// Files dropped onto the History page (while running and with the pointer
    /// over the page) upload. Clipboard paste needs History's focused paste
    /// target, never a bare Ctrl+V.
    fn accept_drops(&self, ui: &egui::Ui) -> Vec<AppCommand> {
        let mut commands = vec![];
        let over_page = ui
            .ctx()
            .pointer_hover_pos()
            .is_some_and(|pos| ui.max_rect().contains(pos));
        if self.view.lifecycle != Lifecycle::Running || !over_page {
            return commands;
        }
        let files = ui.ctx().input(|i| i.raw.dropped_files.clone());
        if !files.is_empty() {
            let paths = files
                .into_iter()
                .map(|f| f.path().to_path_buf().into_os_string())
                .collect();
            match crate::app::upload_sources::UploadSource::files(paths) {
                Ok(source) => commands.push(AppCommand::UploadSource(source)),
                Err(error) => self.client.report_error(error),
            }
        }
        commands
    }

    fn draw_page(&mut self, ui: &mut egui::Ui, commands: &mut Vec<AppCommand>) {
        match self.view.window {
            Some(Window::Settings) => self.draw_settings(ui, commands),
            Some(Window::History) if self.view.settings.is_some() => {
                commands.extend(self.accept_drops(ui));
                // In progress, filters, results, inspector and footer scroll as one page.
                egui::ScrollArea::vertical()
                    .id_salt("history-page")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        commands.extend(super::in_progress::draw(
                            ui,
                            &self.view,
                            &mut self.in_progress_expanded,
                        ));
                        commands.extend(self.history.draw(ui, &self.view));
                    });
            }
            Some(Window::History) => {
                ui.label("Opening local settings…");
            }
            None => {}
        }
    }

    /// Settings: the section list (a column, or a strip on narrow windows)
    /// beside one section's groups, which scroll.
    fn draw_settings(&mut self, ui: &mut egui::Ui, commands: &mut Vec<AppCommand>) {
        let Some(settings) = self.view.settings.clone() else {
            ui.label("Opening local settings…");
            return;
        };
        if self.pane.category() == SettingsCategory::Connections {
            commands.extend(self.accept_config_drop(ui));
        }
        let strip = ui.ctx().content_rect().width() < tokens::COLLAPSE_BELOW;
        if strip {
            self.pane.show_categories(ui, true);
            ui.add_space(14.0);
            self.settings_body(ui, &settings, commands);
        } else {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                self.pane.show_categories(ui, false);
                ui.add_space(tokens::SECTION_GAP);
                ui.vertical(|ui| self.settings_body(ui, &settings, commands));
            });
        }
    }

    /// The current section's groups, scrolling, at most 760 wide.
    fn settings_body(
        &mut self,
        ui: &mut egui::Ui,
        settings: &crate::persistence::settings::Settings,
        commands: &mut Vec<AppCommand>,
    ) {
        egui::ScrollArea::vertical()
            .id_salt("settings-page")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Cap, never widen: set_max_width alone would stretch a narrow page.
                ui.set_max_width(ui.available_width().min(tokens::GROUPS_MAX_WIDTH));
                match self.pane.category() {
                    SettingsCategory::Troubleshooting => {
                        self.troubleshooting
                            .draw(ui, settings, &self.client, &self.picker_gate)
                    }
                    SettingsCategory::Backup => self.preferences_transfer.draw(
                        ui,
                        settings,
                        &self.client,
                        &self.picker_gate,
                    ),
                    _ => {}
                }
                commands.extend(self.pane.draw(
                    ui,
                    settings,
                    self.view.notifications_enabled,
                    &self.view.shortcut_statuses,
                    self.view.recorded_shortcut,
                ));
            });
    }

    /// One `.vectrcap` file dropped on Connections opens its install review.
    fn accept_config_drop(&self, ui: &egui::Ui) -> Vec<AppCommand> {
        let over_page = ui
            .ctx()
            .pointer_hover_pos()
            .is_some_and(|pos| ui.max_rect().contains(pos));
        let files = ui.ctx().input(|i| i.raw.dropped_files.clone());
        if self.view.lifecycle != Lifecycle::Running || !over_page || files.is_empty() {
            return vec![];
        }
        if files.len() != 1 {
            self.client
                .report_error(AppError::InvalidInput("drop one .vectrcap file at a time"));
            return vec![];
        }
        match crate::app::cli::LaunchRequest::import(files[0].path().to_path_buf().into_os_string())
        {
            Ok(request) => vec![AppCommand::Launch {
                request,
                reply: None,
            }],
            Err(error) => {
                self.client.report_error(error);
                vec![]
            }
        }
    }
}

/// A page title (SemiBold 20) over a muted subtitle, 16 above the content.
fn page_header(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(
            egui::RichText::new(title)
                .font(tokens::semibold(tokens::TITLE_SIZE))
                .color(ui.visuals().text_color()),
        );
        ui.label(
            egui::RichText::new(subtitle)
                .size(tokens::BODY_SIZE)
                .color(ui.visuals().weak_text_color()),
        );
    });
    ui.add_space(16.0);
}

/// The left navigation: brand, History, Settings and, at the bottom, Quit.
/// `collapsed` keeps only the icons; names stay on hover and for screen readers.
pub fn draw_sidebar(ui: &mut egui::Ui, view: &DesktopView, collapsed: bool) -> Vec<AppCommand> {
    let mut commands = vec![];
    ui.spacing_mut().item_spacing.y = 2.0;
    brand(ui, collapsed);
    ui.add_space(tokens::NAV_GAP - 2.0);
    let in_progress = super::in_progress::in_progress_count(view);
    for (icon, label, window, badge) in [
        (Icon::History, "History", Window::History, in_progress),
        (Icon::Settings, "Settings", Window::Settings, 0),
    ] {
        let current = view.window == Some(window);
        if nav_item(ui, icon, label, current, badge, collapsed).clicked() {
            commands.push(AppCommand::Show(window));
        }
    }
    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
        if nav_item(ui, Icon::Power, "Quit", false, 0, collapsed)
            .on_hover_text("Quit VectrCap")
            .clicked()
        {
            commands.push(AppCommand::Quit);
        }
    });
    commands
}

fn brand(ui: &mut egui::Ui, collapsed: bool) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), tokens::BRAND_HEIGHT),
        egui::Sense::hover(),
    );
    let visuals = ui.visuals();
    let size = egui::Vec2::splat(20.0);
    let mark = if collapsed {
        egui::Rect::from_center_size(rect.center(), size)
    } else {
        egui::Rect::from_min_size(egui::pos2(rect.left() + 8.0, rect.center().y - 10.0), size)
    };
    icons::paint(ui.painter(), mark, Icon::Brand, visuals.selection.bg_fill);
    if !collapsed {
        ui.painter().text(
            egui::pos2(mark.right() + 9.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            "VectrCap",
            tokens::semibold(14.0),
            visuals.text_color(),
        );
    }
}

fn nav_item(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    current: bool,
    badge: usize,
    collapsed: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), tokens::NAV_ITEM_HEIGHT),
        egui::Sense::click(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::Button, ui.is_enabled(), current, label)
    });
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let painter = ui.painter();
        let lit = current || response.hovered();
        if lit {
            painter.rect_filled(
                rect,
                egui::CornerRadius::same(tokens::RADIUS_CONTROL),
                visuals.widgets.inactive.bg_fill,
            );
        }
        let color = if lit {
            visuals.text_color()
        } else {
            visuals.weak_text_color()
        };
        let size = egui::Vec2::splat(tokens::ICON_SIZE);
        let glyph = if collapsed {
            egui::Rect::from_center_size(rect.center(), size)
        } else {
            egui::Rect::from_min_size(
                egui::pos2(
                    rect.left() + 10.0,
                    rect.center().y - tokens::ICON_SIZE / 2.0,
                ),
                size,
            )
        };
        icons::paint(painter, glyph, icon, color);
        if !collapsed {
            painter.text(
                egui::pos2(glyph.right() + 10.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                tokens::medium(tokens::BODY_SIZE),
                color,
            );
            if badge > 0 {
                let galley = painter.layout_no_wrap(
                    badge.to_string(),
                    tokens::semibold(tokens::META_SIZE),
                    visuals.selection.stroke.color,
                );
                let width = (galley.size().x + 10.0).max(18.0);
                let pill = egui::Rect::from_min_size(
                    egui::pos2(rect.right() - 10.0 - width, rect.center().y - 9.0),
                    egui::vec2(width, 18.0),
                );
                painter.rect_filled(pill, egui::CornerRadius::same(9), visuals.selection.bg_fill);
                painter.galley(
                    pill.center() - galley.size() / 2.0,
                    galley,
                    visuals.selection.stroke.color,
                );
            }
        }
    }
    if collapsed {
        response.on_hover_text(label)
    } else {
        response
    }
}

/// Window-wide notices under the page header: quitting, the last error, and a
/// paused upload queue with its Resume action.
pub fn draw_status_banner(ui: &mut egui::Ui, view: &DesktopView) -> Vec<AppCommand> {
    let mut commands = vec![];
    let visuals = ui.visuals().clone();
    if view.lifecycle == Lifecycle::Draining {
        banner(
            ui,
            Icon::Pause,
            visuals.weak_text_color(),
            "Quitting.",
            "Settling the upload worker safely…",
            None,
        );
    }
    if let Some(error) = &view.error {
        banner(
            ui,
            Icon::Alert,
            visuals.error_fg_color,
            "Something went wrong.",
            &error.to_string(),
            None,
        );
    }
    if view.queue_paused
        && banner(
            ui,
            Icon::Pause,
            appearance::warning(&visuals),
            "Uploads are paused.",
            "Waiting uploads stay queued until you resume.",
            Some((Icon::Play, "Resume uploads")),
        )
    {
        commands.push(AppCommand::Queue(QueueAction::ResumeAll));
    }
    commands
}

/// Returns whether the action button was clicked.
fn banner(
    ui: &mut egui::Ui,
    icon: Icon,
    icon_color: egui::Color32,
    bold: &str,
    muted: &str,
    action: Option<(Icon, &str)>,
) -> bool {
    let visuals = ui.visuals().clone();
    let mut clicked = false;
    egui::Frame::NONE
        .fill(visuals.extreme_bg_color)
        .stroke(visuals.widgets.noninteractive.bg_stroke)
        .corner_radius(egui::CornerRadius::same(tokens::RADIUS_SURFACE))
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let (slot, _) = ui.allocate_exact_size(
                    egui::Vec2::splat(tokens::ICON_SIZE),
                    egui::Sense::hover(),
                );
                icons::paint(ui.painter(), slot, icon, icon_color);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some((icon, label)) = action {
                        clicked = widgets::icon_button(ui, icon, label).clicked();
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        let mut job = egui::text::LayoutJob::default();
                        job.append(
                            bold,
                            0.0,
                            egui::TextFormat::simple(
                                tokens::semibold(tokens::BODY_SIZE),
                                visuals.text_color(),
                            ),
                        );
                        job.append(
                            muted,
                            6.0,
                            egui::TextFormat::simple(
                                egui::FontId::proportional(tokens::BODY_SIZE),
                                visuals.weak_text_color(),
                            ),
                        );
                        ui.add(egui::Label::new(job).wrap());
                    });
                });
            });
        });
    ui.add_space(14.0);
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tokens;

    #[test]
    fn fixture_shell_shows_the_fixture_view_without_a_runtime_or_tray() {
        use crate::ui::fixtures::{MockupVariant, mockup_view};
        let view = mockup_view(MockupVariant::HistoryBusy);
        let shell = Shell::from_fixture(view.clone(), egui::Context::default());
        assert!(shell.tray.is_none());
        assert_eq!(shell.view.window, view.window);
        assert_eq!(shell.client.snapshot().jobs.len(), view.jobs.len());
    }

    fn screen(width: f32) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 720.0),
            )),
            ..Default::default()
        }
    }

    /// Two frames (fonts apply from the second); returns the last output and the
    /// root ui's used rect.
    fn run(
        ctx: &egui::Context,
        width: f32,
        mut draw: impl FnMut(&mut egui::Ui),
    ) -> (egui::FullOutput, egui::Rect) {
        ctx.enable_accesskit();
        ctx.set_visuals(appearance::visuals());
        let mut used = egui::Rect::NOTHING;
        let mut last = None;
        for _ in 0..2 {
            let mut output = ctx.run_ui(screen(width), |ui| {
                draw(ui);
                used = ui.min_rect();
            });
            output.textures_delta.clear();
            last = Some(output);
        }
        (last.expect("two frames ran"), used)
    }

    fn button_labels(output: &egui::FullOutput) -> Vec<String> {
        output
            .platform_output
            .accesskit_update
            .iter()
            .flat_map(|update| &update.nodes)
            .filter(|(_, node)| node.role() == egui::accesskit::Role::Button)
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect()
    }

    fn texts(output: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
        output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => {
                    Some((text.galley.text().to_owned(), text.visual_bounding_rect()))
                }
                _ => None,
            })
            .collect()
    }

    fn fixture_shell(ctx: &egui::Context) -> Shell {
        use crate::ui::fixtures::{MockupVariant, mockup_view};
        Shell::from_fixture(mockup_view(MockupVariant::HistoryBusy), ctx.clone())
    }

    #[test]
    fn sidebar_offers_exactly_history_settings_and_quit() {
        let ctx = egui::Context::default();
        appearance::install_fonts(&ctx);
        let view =
            crate::ui::fixtures::mockup_view(crate::ui::fixtures::MockupVariant::HistoryBusy);
        let (output, _) = run(&ctx, 1180.0, |ui| {
            draw_sidebar(ui, &view, false);
        });
        let mut labels = button_labels(&output);
        labels.sort();
        assert_eq!(labels, ["History", "Quit", "Settings"]);
    }

    #[test]
    fn sidebar_collapses_below_960_and_keeps_accessible_names() {
        let ctx = egui::Context::default();
        let mut shell = fixture_shell(&ctx);
        let (output, _) = run(&ctx, 900.0, |ui| {
            shell.draw(ui);
        });
        let in_sidebar: Vec<_> = texts(&output)
            .into_iter()
            .filter(|(text, rect)| !text.is_empty() && rect.left() < tokens::SIDEBAR_COLLAPSED)
            .collect();
        assert!(
            in_sidebar.is_empty(),
            "collapsed sidebar draws {in_sidebar:?}"
        );
        let labels = button_labels(&output);
        for name in ["History", "Settings", "Quit"] {
            assert!(labels.iter().any(|l| l == name), "missing {name}");
        }
    }

    #[test]
    fn main_window_has_no_capture_or_record_buttons() {
        let ctx = egui::Context::default();
        let mut shell = fixture_shell(&ctx);
        let (output, _) = run(&ctx, 1180.0, |ui| {
            shell.draw(ui);
        });
        let labels = button_labels(&output);
        assert!(!labels.is_empty());
        for label in &labels {
            assert!(
                // "Recordings" is a History filter, not a launcher.
                !label.starts_with("Capture ")
                    && label != "Record"
                    && !label.starts_with("Record ")
                    && !label.starts_with("Start / stop")
                    && label != "Return to tray",
                "unexpected button {label}"
            );
        }
    }

    fn settings_shell(ctx: &egui::Context, category: SettingsCategory) -> Shell {
        use crate::ui::fixtures::{MockupVariant, mockup_view};
        let mut shell =
            Shell::from_fixture(mockup_view(MockupVariant::Settings(category)), ctx.clone());
        shell.select_settings_category(category);
        shell
    }

    #[test]
    fn import_config_lives_in_connections_not_backup() {
        let import = "Import VectrCap config…".to_owned();
        let ctx = egui::Context::default();
        let mut shell = settings_shell(&ctx, SettingsCategory::Connections);
        let (output, _) = run(&ctx, 1180.0, |ui| shell.draw(ui));
        assert!(button_labels(&output).contains(&import));
        let ctx = egui::Context::default();
        let mut shell = settings_shell(&ctx, SettingsCategory::Backup);
        let (output, _) = run(&ctx, 1180.0, |ui| shell.draw(ui));
        assert!(!button_labels(&output).contains(&import));
    }

    #[test]
    fn each_section_draws_at_380_double_scale_without_overflow() {
        for category in SettingsCategory::ALL {
            let ctx = egui::Context::default();
            ctx.set_zoom_factor(2.0);
            let mut shell = settings_shell(&ctx, category);
            let (_, used) = run(&ctx, 380.0, |ui| shell.draw(ui));
            assert!(
                used.right() <= 380.5,
                "{} reaches {}",
                category.label(),
                used.right()
            );
        }
    }

    #[test]
    fn shell_draws_at_double_scale_and_380_wide_without_overflow() {
        let ctx = egui::Context::default();
        ctx.set_zoom_factor(2.0);
        let mut shell = fixture_shell(&ctx);
        // RawInput sizes are points, so this is a 760 px window at 200%.
        let (_, used) = run(&ctx, 380.0, |ui| {
            shell.draw(ui);
        });
        assert!(used.right() <= 380.5, "content reaches {}", used.right());
    }
}
