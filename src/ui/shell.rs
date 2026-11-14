use super::settings::SettingsPane;
use crate::{
    app::{
        desktop::{DesktopClient, DesktopView},
        error::AppError,
        events::{AppCommand, Lifecycle, Window},
    },
    platform::windows::{appearance, file_dialog::PickerGate, tray::Tray},
};
use eframe::egui;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
pub struct Shell {
    shown_since: Arc<
        std::sync::Mutex<
            std::collections::HashMap<crate::app::notifications::NoticeId, std::time::Instant>,
        >,
    >,
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
}
impl Shell {
    pub fn new(
        client: DesktopClient,
        focus_requested: Arc<AtomicBool>,
        context: egui::Context,
    ) -> Result<Self, AppError> {
        let picker_gate = PickerGate::default();
        let tray = Tray::new(client.clone(), context, picker_gate.clone())?;
        Ok(Self {
            view: client.snapshot(),
            client,
            tray,
            pane: SettingsPane::default(),
            history: Default::default(),
            shown_since: Default::default(),
            focus_revision: 0,
            current_window: None,
            focus_requested,
            picker_gate,
        })
    }
    fn submit(&mut self, command: AppCommand) {
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
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.focus_requested.swap(false, Ordering::AcqRel) {
            self.submit(AppCommand::Show(Window::Settings));
        }
        self.view = self.client.snapshot();
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
            if let Err(error) = self.tray.update(&settings.destinations, settings.revision) {
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
            ctx.show_viewport_deferred(
                egui::ViewportId::from_hash_of("completion-notices"),
                super::completion_notification::viewport_builder(),
                move |ui, _class| {
                    // Read the live snapshot: a disabled preference or dismissed
                    // notice must not survive in a deferred callback's clone.
                    let view = client.snapshot();
                    if !super::completion_notification::surface_visible(&view) {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        return;
                    }
                    let mut shown = shown.lock().unwrap_or_else(|e| e.into_inner());
                    let commands = if ui.ctx().input(|i| i.viewport().close_requested()) {
                        view.notices
                            .iter()
                            .map(|n| AppCommand::DismissNotice(n.view.id()))
                            .collect()
                    } else {
                        egui::ScrollArea::vertical()
                            .show(ui, |ui| {
                                super::completion_notification::draw(ui, &view, &mut shown)
                            })
                            .inner
                    };
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
                    ui.strong("VectrCap");
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
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| match (self.view.window, &self.view.settings) {
                            (Some(Window::Settings), Some(settings)) => commands.extend(
                                self.pane
                                    .draw(ui, settings, self.view.notifications_enabled),
                            ),
                            (Some(Window::History), Some(_)) => {
                                commands.extend(self.history.draw(ui, &self.view))
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
