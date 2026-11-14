use super::destinations::ConnectionDraft;
use crate::{
    app::{
        desktop::DesktopView, destination_manager::DestinationEdit, destinations::DestinationId,
        error::AppError, events::AppCommand,
    },
    persistence::settings::Settings,
    vectr::{
        client::UploadToken,
        uploader_info::{DomainChoice, UploaderInfoSupport},
    },
};
use eframe::egui::{self, Ui};
use uuid::Uuid;
#[derive(Default)]
pub struct SettingsPane {
    draft: Option<ConnectionDraft>,
    saving: Option<Uuid>,
    error: Option<AppError>,
    remove: Option<DestinationId>,
    just_opened: bool,
    shortcuts: Option<Vec<crate::app::shortcuts::ShortcutBinding>>,
}
impl SettingsPane {
    pub fn submission_failed(&mut self, error: AppError) {
        self.saving = None;
        self.error = Some(error.clone());
        if let Some(draft) = &mut self.draft {
            draft.pending_test = None;
            draft.metadata = Some(Err(error));
        }
    }
    pub fn opened(&mut self, settings: &Settings) {
        self.just_opened = true;
        if settings.destinations.is_empty() {
            self.draft = Some(ConnectionDraft::new(settings, None));
        }
    }
    /// Does not touch `saving`: `reconcile` owns that field's lifecycle and
    /// runs every frame regardless of which window is active, so switching
    /// tabs mid-save must not cause the eventual save_result to be dropped.
    pub fn clear(&mut self, ctx: &egui::Context) {
        self.draft = None;
        self.remove = None;
        self.shortcuts = None;
        self.error = None;
        egui::text_edit::TextEditState::default().store(ctx, egui::Id::unique("connection-token"));
    }
    pub fn reconcile(&mut self, view: &DesktopView, ctx: &egui::Context) {
        if let Some((request, result)) = &view.connection
            && let Some(draft) = &mut self.draft
            && let Some(settings) = &view.settings
        {
            draft.accept_metadata(*request, result.clone(), settings.revision);
        }
        if let Some((request, result)) = &view.save_result
            && Some(*request) == self.saving
        {
            self.saving = None;
            match result {
                Ok(()) => self.clear(ctx),
                Err(error) => self.error = Some(error.clone()),
            }
        }
    }
    pub fn draw(
        &mut self,
        ui: &mut Ui,
        settings: &Settings,
        notifications_enabled: bool,
        statuses: &[(
            crate::app::shortcuts::ShortcutAction,
            crate::app::shortcuts::ShortcutStatus,
        )],
    ) -> Vec<AppCommand> {
        let mut commands = vec![];
        let mut enabled = notifications_enabled;
        if ui
            .checkbox(&mut enabled, "Show completion notifications")
            .changed()
        {
            commands.push(AppCommand::SetNotificationsEnabled(enabled));
        }
        ui.collapsing("Appearance and startup", |ui| {
            let mut appearance = settings.appearance;
            egui::ComboBox::from_id_salt("ui-scale").selected_text(format!("{}%", appearance.ui_scale_percent)).show_ui(ui, |ui| {
                for scale in crate::persistence::settings::AppearancePreferences::VALID_SCALES {
                    ui.selectable_value(&mut appearance.ui_scale_percent, scale, format!("{scale}%"));
                }
            });
            if appearance != settings.appearance {
                commands.push(AppCommand::SetAppearance { appearance, revision: settings.revision });
            }
            ui.label("Scale changes interface text and controls relative to Windows DPI. Media pixels are unchanged.");
            let mut startup = settings.startup;
            if ui.checkbox(&mut startup.launch_at_sign_in, "Launch VectrCap when I sign in").changed() {
                commands.push(AppCommand::SetStartup { startup, revision: settings.revision });
            }
        });
        ui.collapsing("Archive and cleanup", |ui| {
            use crate::persistence::settings::StorageMode;
            let mut policy = settings.archive.clone();
            ui.label(policy.archive_root.as_ref().map_or_else(|| "Archive: Pictures / VectrCap".to_owned(), |p| format!("Archive: {}", p.display())));
            if ui.button("Choose archive folder...").clicked() { commands.push(AppCommand::ChooseArchiveFolder); }
            if ui.button("Use default archive folder").clicked() { policy.archive_root = None; }
            egui::ComboBox::from_id_salt("archive-mode").selected_text(format!("{:?}", policy.storage_mode)).show_ui(ui, |ui| {
                for mode in [StorageMode::AlwaysSave, StorageMode::PendingOrFailed, StorageMode::NeverAutomatic] {
                    ui.selectable_value(&mut policy.storage_mode, mode, format!("{mode:?}"));
                }
            });
            let mut cleanup = policy.cleanup_age_days.is_some();
            if ui.checkbox(&mut cleanup, "Enable age-based cleanup of app-owned archive copies").changed() {
                policy.cleanup_age_days = cleanup.then_some(30);
            }
            if let Some(days) = &mut policy.cleanup_age_days {
                ui.add(egui::DragValue::new(days).range(1..=3650).suffix(" days"));
            }
            ui.label("Cleanup runs at launch. Active upload sources are retained. Favorites do not change retention. Working spool and recording files remain necessary with any archive mode.");
            if policy != settings.archive {
                commands.push(AppCommand::SetArchivePolicy { policy, revision: settings.revision });
            }
        });
        let shortcut_panel = ui.collapsing("Keyboard shortcuts", |ui| {
            use crate::app::shortcuts::{KeyChord, KeyCode};
            ui.label("Capture and editor assignments are saved for later activation; their keys are not registered yet. Apply validates all assignments together. Conflicts keep the previous settings.");
            let bindings = self.shortcuts.get_or_insert_with(|| settings.shortcuts.effective());
            for binding in bindings.iter_mut() {
                if let Some((_, status)) = statuses.iter().find(|(action, _)| *action == binding.action) { ui.label(format!("Current registration: {status:?}")); }
                ui.push_id(binding.action, |ui| {
                    ui.label(format!("{} ({:?}){}", binding.action.label(), binding.scope, if binding.action.is_available_now() { "" } else { " - unavailable until its feature is implemented" }));
                    ui.horizontal_wrapped(|ui| {
                        ui.checkbox(&mut binding.enabled, "Enabled");
                        let mut chord = binding.chord.unwrap_or(KeyChord { ctrl: true, alt: false, shift: true, win: false, key: KeyCode::from_vk(0x41).expect("fixed key") });
                        let mut changed = false;
                        changed |= ui.checkbox(&mut chord.ctrl, "Ctrl").changed();
                        changed |= ui.checkbox(&mut chord.alt, "Alt").changed();
                        changed |= ui.checkbox(&mut chord.shift, "Shift").changed();
                        changed |= ui.checkbox(&mut chord.win, "Win").changed();
                        egui::ComboBox::from_id_salt("key").selected_text(binding.chord.map_or_else(|| "Unassigned".into(), |c| format!("VK {:02X}", c.key.vk()))).show_ui(ui, |ui| {
                            for (vk, label) in (0x41..=0x5A).map(|vk| (vk, char::from_u32(vk).unwrap().to_string())).chain((0x70..=0x87).map(|vk| (vk, format!("F{}", vk - 0x6F)))) {
                                changed |= ui.selectable_value(&mut chord.key, KeyCode::from_vk(vk).expect("fixed key"), label).changed();
                            }
                        });
                        if changed { binding.chord = Some(chord); }
                        if ui.button("Reset").clicked() {
                            commands.push(AppCommand::ResetShortcuts { actions: Some(vec![binding.action]), revision: settings.revision });
                            *binding = crate::app::shortcuts::ShortcutBinding::unassigned(binding.action);
                        }
                        if ui.button("Clear").clicked() { binding.chord = None; binding.enabled = false; }
                    });
                });
            }
            if ui.button("Apply shortcuts").clicked() {
                commands.push(AppCommand::ApplyShortcuts { bindings: bindings.clone(), revision: settings.revision });
            }
            if ui.button("Reset all shortcuts").clicked() {
                commands.push(AppCommand::ResetShortcuts { actions: None, revision: settings.revision });
                self.shortcuts = None;
            }
        });
        commands.push(AppCommand::SetShortcutEntryActive(
            shortcut_panel.body_response.is_some_and(|response| {
                response.contains_pointer() || ui.ctx().egui_wants_keyboard_input()
            }),
        ));
        ui.heading(if settings.destinations.is_empty() {
            "Connect to Vectr"
        } else {
            "Destinations"
        });
        ui.label("Use an uploader token created in Vectr. Your password is never needed.");
        ui.add_space(14.0);
        if let Some(error) = &self.error {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Could not apply: {error}"),
            );
        }
        if let Some(draft) = &mut self.draft {
            let mut cancel = false;
            ui.add_enabled_ui(self.saving.is_none(), |ui| {
                match connection(ui, draft, settings, self.just_opened) {
                    Ok(Some(command)) => {
                        if let AppCommand::SaveConnection { request, .. } = &command {
                            self.saving = Some(*request);
                        }
                        commands.push(command);
                    }
                    Ok(None) => {}
                    Err(error) => self.error = Some(error),
                }
                cancel = ui.button("Cancel editing").clicked();
            });
            self.just_opened = false;
            if cancel {
                self.clear(ui.ctx());
                commands.push(AppCommand::CancelConnectionTest);
            }
            if self.saving.is_some() {
                ui.label("Saving connection…");
            }
            return commands;
        }
        let mut edit = None;
        for d in settings.destinations.iter() {
            ui.push_id(d.id.0, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.strong(&d.name);
                    let default = Some(d.id) == settings.destinations.default_id();
                    if ui.radio(default, "Default").clicked() && !default {
                        commands.push(AppCommand::SetDefault {
                            id: d.id,
                            revision: settings.revision,
                        });
                    }
                    if ui.button(format!("Edit {}", d.name)).clicked() {
                        edit = Some(d.id);
                    }
                    if ui.button(format!("Remove {}", d.name)).clicked() {
                        self.remove = Some(d.id);
                    }
                });
                ui.label(d.origin.as_str());
                ui.label(
                    d.link_domain_override
                        .as_ref()
                        .map(|d| format!("Link domain: {}", d.as_str()))
                        .unwrap_or_else(|| "Link domain: uploader default".into()),
                );
                if d.credential_ref.is_none() {
                    ui.label("Setup needed: no token saved");
                }
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);
            });
        }
        if let Some(id) = edit {
            self.draft = Some(ConnectionDraft::new(settings, Some(id)));
            self.just_opened = true;
            self.error = None;
        }
        if let Some(id) = self.remove {
            ui.label("Remove this connection? Unfinished uploads or workflows can prevent removal. Choose another default first when needed.");
            ui.horizontal_wrapped(|ui| {
                if ui.button("Confirm removal").clicked() {
                    let mut destinations = settings.destinations.clone();
                    match destinations.remove(id, None, &Default::default()) {
                        Ok(_) => {
                            commands.push(AppCommand::ApplyDestinations {
                                edit: DestinationEdit {
                                    destinations,
                                    tokens: vec![],
                                },
                                revision: settings.revision,
                            });
                            self.remove = None;
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                if ui.button("Keep connection").clicked() {
                    self.remove = None;
                }
            });
        }
        if ui
            .add_enabled(
                settings.destinations.len() < 32,
                egui::Button::new("Add connection"),
            )
            .clicked()
        {
            self.draft = Some(ConnectionDraft::new(settings, None));
            self.just_opened = true;
            self.error = None;
        }
        commands
    }
}
fn field(ui: &mut Ui, title: &str, value: &mut String, limit: usize, focus: bool) -> bool {
    let label = ui.label(title);
    let response = ui
        .add(
            egui::TextEdit::singleline(value)
                .desired_width(f32::INFINITY)
                .char_limit(limit),
        )
        .labelled_by(label.id);
    if focus {
        response.request_focus();
    }
    response.changed()
}
fn connection(
    ui: &mut Ui,
    draft: &mut ConnectionDraft,
    settings: &Settings,
    focus: bool,
) -> Result<Option<AppCommand>, AppError> {
    field(ui, "Connection name", &mut draft.name, 100, focus);
    let changed = field(ui, "HTTPS server origin", &mut draft.origin, 2048, false);
    if changed {
        draft.domain = None;
        draft.invalidate_test();
    }
    ui.add_space(8.0);
    let label = ui.label(if draft.id.is_some() {
        "Upload token (leave empty to keep the saved token)"
    } else {
        "Upload token"
    });
    let mut output = egui::TextEdit::singleline(&mut *draft.token)
        .id(egui::Id::unique("connection-token"))
        .password(true)
        .char_limit(2560)
        .desired_width(f32::INFINITY)
        .show(ui);
    output.response.response = output.response.response.labelled_by(label.id);
    // Password mode masks accessibility output and suppresses Copy. Discard the
    // widget's undo snapshots every frame; eframe persistence is disabled.
    output.state.clear_undoer();
    output.state.store(ui.ctx(), output.response.id);
    if output.response.changed() {
        draft.invalidate_test();
    }
    let mut command = None;
    if ui
        .add_enabled(
            draft.pending_test.is_none(),
            egui::Button::new("Test connection / Refresh domains"),
        )
        .clicked()
    {
        let request = Uuid::new_v4();
        let test = if draft.token.is_empty() {
            let id = draft
                .id
                .ok_or(AppError::NeedsSetup("enter an upload token before testing"))?;
            if settings
                .destinations
                .get(id)
                .is_none_or(|d| d.origin.as_str() != draft.origin)
            {
                return Err(AppError::NeedsSetup("enter a token for the changed origin"));
            }
            AppCommand::TestSavedConnection {
                request,
                id,
                revision: draft.revision,
            }
        } else {
            AppCommand::TestConnection {
                request,
                origin: draft.origin.clone(),
                token: UploadToken::new(draft.token.to_string())?,
            }
        };
        draft.metadata = None;
        draft.pending_test = Some(request);
        command = Some(test);
    }
    if draft.pending_test.is_some() {
        ui.label("Testing HTTPS connection…");
    }
    match &draft.metadata {
        Some(Ok(UploaderInfoSupport::Available(info))) => {
            ui.label(format!("Verified uploader: {}", info.name));
        }
        Some(Ok(UploaderInfoSupport::Unsupported404)) => {
            ui.label("Metadata unavailable (HTTP 404). Connection remains unverified; manual HTTPS domain entry is available.");
        }
        Some(Err(error)) => {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Connection test failed: {error}"),
            );
        }
        None if draft.pending_test.is_none() => {
            ui.label("Unverified. You can save now and test later.");
        }
        None => {}
    }
    ui.add_space(10.0);
    let domain_label = ui.label("Link domain");
    egui::ComboBox::from_id_salt("link-domain")
        .selected_text(draft.domain.as_deref().unwrap_or("Use uploader default"))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut draft.domain, None, "Use uploader default");
            if let Some(Ok(UploaderInfoSupport::Available(info))) = &draft.metadata {
                for domain in &info.domains {
                    match domain {
                        DomainChoice::Https(origin) => {
                            ui.selectable_value(
                                &mut draft.domain,
                                Some(origin.as_str().to_owned()),
                                origin.as_str(),
                            );
                        }
                        DomainChoice::UnsupportedHttp(origin) => {
                            ui.add_enabled(
                                false,
                                egui::Label::new(format!("{origin} (HTTP unsupported)")),
                            );
                        }
                    }
                }
            }
        })
        .response
        .labelled_by(domain_label.id);
    if matches!(
        draft.metadata,
        Some(Ok(UploaderInfoSupport::Unsupported404))
    ) {
        let mut manual = draft.domain.clone().unwrap_or_default();
        if field(
            ui,
            "Manual HTTPS domain (empty uses uploader default)",
            &mut manual,
            2048,
            false,
        ) {
            draft.domain = (!manual.is_empty()).then_some(manual);
        }
    }
    let already_default = draft.id.is_some() && draft.id == settings.destinations.default_id();
    ui.add_enabled_ui(!already_default, |ui| {
        ui.checkbox(&mut draft.make_default, "Use as default for future uploads");
    });
    ui.add_space(14.0);
    if ui.button("Save connection").clicked() {
        let revision = draft.revision;
        command = Some(AppCommand::SaveConnection {
            request: Uuid::new_v4(),
            edit: draft.prepare_save(settings)?,
            revision,
        });
    }
    Ok(command)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::settings::Settings;
    #[test]
    fn clear_does_not_drop_a_save_result_pending_across_a_window_switch() {
        let mut pane = SettingsPane::default();
        let settings = Settings::default();
        pane.opened(&settings);
        let request = Uuid::new_v4();
        pane.saving = Some(request);
        let ctx = egui::Context::default();
        // Simulate switching away from the Settings window while the save
        // is still in flight: this must not make the eventual result unreachable.
        pane.clear(&ctx);
        assert_eq!(pane.saving, Some(request));
        let view = DesktopView {
            save_result: Some((request, Err(AppError::Credential("invalid upload token")))),
            ..Default::default()
        };
        pane.reconcile(&view, &ctx);
        assert_eq!(pane.saving, None);
        assert!(pane.error.is_some());
    }
    #[test]
    fn appearance_scales_validate_and_preserve_physical_geometry() {
        let baseline = Settings::default();
        for percent in [100, 125, 150, 175, 200] {
            let mut settings = baseline.clone();
            settings.appearance.ui_scale_percent = percent;
            settings.validate().unwrap();
            let encoded = serde_json::to_vec(&settings).unwrap();
            let decoded: Settings = serde_json::from_slice(&encoded).unwrap();
            assert_eq!(decoded, settings);
            assert_eq!(settings.archive, baseline.archive);
            assert_eq!(settings.filename, baseline.filename);
        }
        let mut invalid = baseline;
        invalid.appearance.ui_scale_percent = 201;
        assert!(invalid.validate().is_err());
    }
    #[test]
    fn settings_draw_at_double_scale_and_narrow_width() {
        let context = egui::Context::default();
        context.set_zoom_factor(2.0);
        let settings = Settings::default();
        let mut pane = SettingsPane::default();
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(360.0, 640.0),
                )),
                ..Default::default()
            },
            |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    pane.draw(ui, &settings, true, &[]);
                });
            },
        );
        assert!(!output.shapes.is_empty());
        output.textures_delta.clear();
    }
}
