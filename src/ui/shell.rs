use super::settings::{SettingsCategory, SettingsPane};
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
    tray: Tray,
    pane: SettingsPane,
    history: super::history::HistoryPane,
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
        Ok(Self {
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
            shown_since: Default::default(),
            thumbs: Arc::new(Mutex::new(
                super::completion_notification::NoticeThumbs::new(),
            )),
            focus_revision: 0,
            current_window: None,
            focus_requested,
            picker_gate,
            picker_requests: 0,
        })
    }
    fn submit(&mut self, command: AppCommand) {
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
            if let Err(error) = self.tray.update_settings(settings) {
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
        let mut commands = vec![];
        egui::Frame::central_panel(ui.style())
            .inner_margin(18.0)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        eframe::egui::RichText::new("VectrCap")
                            .strong()
                            .color(ui.visuals().text_color()),
                    );
                    if ui
                        .selectable_label(self.view.window == Some(Window::Uploads), "Uploads")
                        .clicked()
                    {
                        commands.push(AppCommand::Show(Window::Uploads));
                    }
                    if ui
                        .selectable_label(self.view.window == Some(Window::Settings), "Settings")
                        .clicked()
                    {
                        commands.push(AppCommand::Show(Window::Settings));
                    }
                    if !self.view.batch_summary.is_empty()
                        && ui
                            .selectable_label(
                                self.view.window == Some(Window::BatchSummary),
                                "Batch summary",
                            )
                            .clicked()
                    {
                        commands.push(AppCommand::Show(Window::BatchSummary));
                    }
                    if ui
                        .selectable_label(self.view.window == Some(Window::Tasks), "Task results")
                        .clicked()
                    {
                        commands.push(AppCommand::Show(Window::Tasks));
                    }
                    if ui
                        .selectable_label(self.view.window == Some(Window::History), "History")
                        .clicked()
                    {
                        commands.push(AppCommand::Show(Window::History));
                    }
                    if ui.button("Return to tray").clicked() {
                        commands.push(AppCommand::CloseWindow);
                    }
                    if ui.button("Quit").clicked() {
                        if self.view.lifecycle == Lifecycle::Exited {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        } else {
                            commands.push(AppCommand::Quit);
                        }
                    }
                });
                ui.separator();
                ui.add_space(10.0);
                if self.view.lifecycle == Lifecycle::Draining {
                    ui.label("Quitting — settling the upload worker safely…");
                }
                if let Some(error) = &self.view.error {
                    ui.colored_label(ui.visuals().error_fg_color, error.to_string());
                }
                ui.add_enabled_ui(self.view.lifecycle == Lifecycle::Running, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            eframe::egui::RichText::new("Capture")
                                .strong()
                                .color(ui.visuals().text_color()),
                        );
                        for (label, mode) in capture_entries() {
                            if ui.button(label).clicked() {
                                commands.push(AppCommand::StartCapture {
                                    mode,
                                    selection: Default::default(),
                                });
                            }
                        }
                        if ui.button("Upload from clipboard").clicked() {
                            commands.push(AppCommand::UploadClipboard);
                        }
                        if ui.button("Upload file…").clicked() {
                            crate::platform::windows::file_dialog::spawn_pick_and_upload(
                                self.client.clone(),
                                self.picker_gate.clone(),
                            );
                        }
                    });
                    if self.view.queue_paused && self.view.window != Some(Window::Uploads) {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                eframe::egui::RichText::new("Uploads are paused.")
                                    .strong()
                                    .color(ui.visuals().text_color()),
                            );
                            ui.label("Waiting jobs stay queued until you resume.");
                            if ui.button("Resume uploads").clicked() {
                                commands.push(AppCommand::Queue(QueueAction::ResumeAll));
                            }
                        });
                    } else if !self.view.queue_paused
                        && self.view.window != Some(Window::Uploads)
                        && self
                            .view
                            .jobs
                            .iter()
                            .any(|job| job.paused && !job.status.is_terminal())
                    {
                        ui.label(
                            "Some uploads are paused individually. Resume each one from Uploads.",
                        );
                    }
                    ui.add_space(8.0);
                    if self.view.window == Some(Window::Settings) && self.view.settings.is_some()
                    {
                        self.pane.show_categories(ui);
                        ui.separator();
                    }
                    if self.view.window == Some(Window::History) {
                        if self.view.settings.is_some() {
                            commands.extend(self.history.draw(ui, &self.view));
                        } else {
                            ui.label("Opening local settings…");
                        }
                        return;
                    }
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| match (self.view.window, &self.view.settings) {
                            (Some(Window::Settings), Some(settings)) => {
                                match self.pane.category() {
                                    SettingsCategory::Troubleshooting => {
                                        self.troubleshooting.draw(
                                            ui,
                                            settings,
                                            &self.client,
                                            &self.picker_gate,
                                        );
                                    }
                                    SettingsCategory::Backup => {
                                        self.preferences_transfer.draw(
                                            ui,
                                            settings,
                                            &self.client,
                                            &self.picker_gate,
                                        );
                                        if ui.button("Import VectrCap config…").clicked() {
                                            crate::platform::windows::file_dialog::spawn_import_destination(
                                                self.client.clone(),
                                                self.picker_gate.clone(),
                                            );
                                        }
                                        let zone = ui.add(
                                            egui::Label::new(
                                                "Drop one .vectrcap config here to review installation",
                                            )
                                            .sense(egui::Sense::click()),
                                        );
                                        if zone.hovered() {
                                            let files =
                                                ui.ctx().input(|i| i.raw.dropped_files.clone());
                                            if files.len() == 1 {
                                                match crate::app::cli::LaunchRequest::import(
                                                    files[0].path().to_path_buf().into_os_string(),
                                                ) {
                                                    Ok(request) => {
                                                        commands.push(AppCommand::Launch {
                                                            request,
                                                            reply: None,
                                                        });
                                                    }
                                                    Err(error) => self.client.report_error(error),
                                                }
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                                commands.extend(self.pane.draw(
                                    ui,
                                    settings,
                                    self.view.notifications_enabled,
                                    &self.view.shortcut_statuses,
                                    self.view.recorded_shortcut,
                                ))
                            }
                            (Some(Window::Tasks), Some(_)) => {
                                commands.extend(super::task_results::draw(
                                    ui,
                                    &self.view.task_workflows,
                                    &self.client,
                                    &self.picker_gate,
                                ))
                            }
                            (Some(Window::BatchSummary), Some(_)) => {
                                super::batch_summary::draw(ui, &self.view);
                            }
                            (Some(Window::Uploads), Some(_)) => {
                                commands.extend(super::uploads::draw(
                                    ui,
                                    &self.view,
                                    &self.client,
                                    &self.picker_gate,
                                ))
                            }
                            (_, None) => {
                                ui.label("Opening local settings…");
                            }
                            _ => {}
                        });
                });
            });
        for command in commands {
            self.submit(command);
        }
    }
    fn persist_egui_memory(&self) -> bool {
        false
    }
}

fn capture_entries() -> [(&'static str, crate::capture::request::CaptureMode); 4] {
    use crate::capture::request::CaptureMode;
    [
        ("Capture region", CaptureMode::Region),
        ("Capture window under pointer", CaptureMode::Window),
        ("Capture fullscreen", CaptureMode::Fullscreen),
        ("Capture last region", CaptureMode::LastRegion),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::request::CaptureMode;

    #[test]
    fn shell_capture_row_offers_the_tray_capture_modes() {
        let entries = capture_entries();
        assert_eq!(
            entries.map(|(_, mode)| mode),
            [
                CaptureMode::Region,
                CaptureMode::Window,
                CaptureMode::Fullscreen,
                CaptureMode::LastRegion,
            ]
        );
    }
}
