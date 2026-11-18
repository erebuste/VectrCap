use super::destinations::ConnectionDraft;
use super::{
    icons::Icon,
    tokens,
    widgets::{self, PillTone},
};
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsCategory {
    #[default]
    Connections,
    Capture,
    Recording,
    Shortcuts,
    Tasks,
    Storage,
    General,
    Integrations,
    Backup,
    Troubleshooting,
}

impl SettingsCategory {
    pub const ALL: [Self; 10] = [
        Self::Connections,
        Self::Capture,
        Self::Recording,
        Self::Shortcuts,
        Self::Tasks,
        Self::Storage,
        Self::General,
        Self::Integrations,
        Self::Backup,
        Self::Troubleshooting,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Connections => "Connections",
            Self::Capture => "Capture",
            Self::Recording => "Recording",
            Self::Shortcuts => "Shortcuts",
            Self::Tasks => "Tasks",
            Self::Storage => "Storage",
            Self::General => "General",
            Self::Integrations => "Integrations",
            Self::Backup => "Backup",
            Self::Troubleshooting => "Troubleshooting",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ShortcutCapture {
    Binding(crate::app::shortcuts::ShortcutAction),
    New,
}

struct NewShortcut {
    action: crate::app::shortcuts::ShortcutAction,
    chord: Option<crate::app::shortcuts::KeyChord>,
}

#[derive(Default)]
pub struct SettingsPane {
    integrations: super::integrations::IntegrationsPane,
    recorder: Option<super::recording_settings::RecordingSettingsPane>,
    audio_endpoints: Vec<super::audio_sources::AudioEndpoint>,
    audio_meter: Option<Result<f32, AppError>>,
    region: Option<super::region_settings::RegionSettingsPane>,
    draft: Option<ConnectionDraft>,
    saving: Option<Uuid>,
    error: Option<AppError>,
    remove: Option<DestinationId>,
    just_opened: bool,
    shortcuts: Option<Vec<crate::app::shortcuts::ShortcutBinding>>,
    recording: Option<(
        ShortcutCapture,
        Uuid,
        crate::platform::windows::hotkeys::ShortcutRecorder,
    )>,
    adding: Option<NewShortcut>,
    category: SettingsCategory,
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
            self.category = SettingsCategory::Connections;
            self.draft = Some(ConnectionDraft::new(settings, None));
        }
    }
    pub fn category(&self) -> SettingsCategory {
        self.category
    }
    pub fn select(&mut self, category: SettingsCategory) {
        self.category = category;
    }
    /// The section list: a 168-wide column, or a wrapping strip when `strip`.
    pub fn show_categories(&mut self, ui: &mut Ui, strip: bool) {
        let mut entries = |ui: &mut Ui, width: f32| {
            for category in SettingsCategory::ALL {
                let current = self.category == category;
                if widgets::section_button(ui, current, category.label(), width).clicked() {
                    self.category = category;
                }
            }
        };
        if strip {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
                entries(ui, 0.0);
            });
        } else {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                entries(ui, tokens::SECTION_LIST_WIDTH);
            });
        }
    }
    /// Does not touch `saving`: `reconcile` owns that field's lifecycle and
    /// runs every frame regardless of which window is active, so switching
    /// tabs mid-save must not cause the eventual save_result to be dropped.
    pub fn clear(&mut self, ctx: &egui::Context) {
        self.recorder = None;
        self.region = None;
        self.draft = None;
        self.remove = None;
        self.shortcuts = None;
        self.recording = None;
        self.adding = None;
        self.error = None;
        egui::text_edit::TextEditState::default().store(ctx, egui::Id::unique("connection-token"));
    }
    pub fn reconcile(&mut self, view: &DesktopView, ctx: &egui::Context) {
        self.audio_endpoints = view.audio_endpoints.clone();
        self.audio_meter = view.audio_meter.clone();
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
        recorded_shortcut: Option<(Uuid, crate::app::shortcuts::KeyChord)>,
    ) -> Vec<AppCommand> {
        let mut commands = Vec::new();
        // Leaving a category settles the controls that only exist on that page.
        // The draft and in-flight save stay, so returning restores them.
        if self.category != SettingsCategory::Recording
            && let Some(pane) = &mut self.recorder
            && let Some(command) = pane.stop_test()
        {
            commands.push(command);
        }
        if self.category != SettingsCategory::Integrations {
            // Re-read the registration next time the section opens: it can
            // change outside the app (Windows Settings, another install).
            self.integrations = Default::default();
        }
        if self.category != SettingsCategory::Shortcuts {
            self.recording = None;
            self.adding = None;
        }
        match self.category {
            SettingsCategory::Integrations => self.integrations.draw(ui),
            SettingsCategory::Capture => {
                drawing(ui, settings, &mut commands);
                ui.add_space(tokens::GROUP_GAP);
                let pane = self.region.get_or_insert_with(|| {
                    super::region_settings::RegionSettingsPane::new(settings)
                });
                if let Some(command) = pane.draw(ui, settings) {
                    commands.push(command);
                }
            }
            SettingsCategory::Recording => {
                let pane = self.recorder.get_or_insert_with(|| {
                    super::recording_settings::RecordingSettingsPane::new(settings)
                });
                commands.extend(pane.draw(
                    ui,
                    settings,
                    &self.audio_endpoints,
                    self.audio_meter.as_ref(),
                ));
            }
            SettingsCategory::Tasks => commands.extend(super::task_settings::draw(ui, settings)),
            SettingsCategory::General => {
                general(ui, settings, notifications_enabled, &mut commands);
                ui.add_space(tokens::GROUP_GAP);
                updates(ui);
            }
            SettingsCategory::Storage => storage(ui, settings, &mut commands),
            SettingsCategory::Shortcuts => {
                self.shortcuts_section(ui, settings, statuses, recorded_shortcut, &mut commands)
            }
            SettingsCategory::Connections => self.connections(ui, settings, &mut commands),
            SettingsCategory::Backup | SettingsCategory::Troubleshooting => {}
        }
        commands.push(AppCommand::SetShortcutEntryActive(
            self.recording.as_ref().map(|(_, request, _)| *request),
        ));
        if self.category != SettingsCategory::Connections
            && let Some(error) = &self.error
        {
            ui.add_space(10.0);
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Could not apply: {error}"),
            );
        }
        commands
    }

    fn shortcuts_section(
        &mut self,
        ui: &mut Ui,
        settings: &Settings,
        statuses: &[(
            crate::app::shortcuts::ShortcutAction,
            crate::app::shortcuts::ShortcutStatus,
        )],
        recorded_shortcut: Option<(Uuid, crate::app::shortcuts::KeyChord)>,
        commands: &mut Vec<AppCommand>,
    ) {
        use crate::app::{destinations::DestinationSelection, shortcuts::ShortcutStatus};
        let mut recording = self.recording.take();
        let mut adding = self.adding.take();
        let mut shortcut_error = None;
        let mut clear_draft = false;
        let bindings = self
            .shortcuts
            .get_or_insert_with(|| settings.shortcuts.effective());
        widgets::setting_group(
            ui,
            "Shortcuts",
            Some(
                "Press a shortcut anywhere in Windows to run its action. Editor, ruler and sampler keys work inside those tools.",
            ),
            |ui| {
                if let Some(mut draft) = adding.take() {
                    let choices: Vec<_> = bindings
                        .iter()
                        .filter(|binding| binding.chord.is_none())
                        .map(|binding| binding.action)
                        .collect();
                    if !choices.contains(&draft.action)
                        && let Some(action) = choices.first().copied()
                    {
                        draft.action = action;
                    }
                    if choices.is_empty() {
                        recording = None;
                    } else {
                        let mut dismiss = false;
                        widgets::card_block(ui, |ui| {
                            ui.label(
                                egui::RichText::new("New shortcut")
                                    .font(tokens::medium(tokens::BODY_SIZE))
                                    .color(ui.visuals().text_color()),
                            );
                            ui.horizontal_wrapped(|ui| {
                                egui::ComboBox::from_id_salt("new-shortcut-action")
                                    .width(220.0)
                                    .selected_text(draft.action.label())
                                    .show_ui(ui, |ui| {
                                        for action in &choices {
                                            ui.selectable_value(
                                                &mut draft.action,
                                                *action,
                                                action.label(),
                                            );
                                        }
                                    });
                                widgets::hint(ui, shortcut_scope_label(draft.action.scope()));
                            });
                            ui.horizontal_wrapped(|ui| {
                                match shortcut_key_button(
                                    ui,
                                    "new-shortcut-key",
                                    &mut recording,
                                    ShortcutCapture::New,
                                    draft.chord,
                                    recorded_shortcut,
                                    "Click, then press the keys",
                                ) {
                                    Ok(Some(chord)) => draft.chord = Some(chord),
                                    Ok(None) => {}
                                    Err(error) => shortcut_error = Some(error),
                                }
                                let add = ui
                                    .add_enabled_ui(draft.chord.is_some(), |ui| {
                                        widgets::toggle_button(ui, false, None, "Add").clicked()
                                    })
                                    .inner;
                                if add && let Some(chord) = draft.chord {
                                    match store_shortcut_change(
                                        bindings,
                                        draft.action,
                                        Some(chord),
                                        true,
                                    ) {
                                        Ok(()) => dismiss = true,
                                        Err(error) => shortcut_error = Some(error),
                                    }
                                }
                                if widgets::ghost_button(ui, None, "Cancel").clicked() {
                                    dismiss = true;
                                }
                            });
                        });
                        if dismiss {
                            if recording
                                .as_ref()
                                .is_some_and(|(target, _, _)| *target == ShortcutCapture::New)
                            {
                                recording = None;
                            }
                        } else {
                            adding = Some(draft);
                        }
                    }
                }
                let shown: Vec<usize> = bindings
                    .iter()
                    .enumerate()
                    .filter(|(_, binding)| shortcut_is_shown(binding))
                    .map(|(index, _)| index)
                    .collect();
                if shown.is_empty() {
                    widgets::card_block(ui, |ui| {
                        widgets::hint(ui, "No shortcuts yet. Add one below.");
                    });
                }
                for index in shown {
                    let action = bindings[index].action;
                    let status = statuses
                        .iter()
                        .find(|(listed, _)| *listed == action)
                        .map(|(_, status)| *status);
                    let mut description = shortcut_scope_label(action.scope()).to_owned();
                    if !action.is_available_now() {
                        description.push_str(", not available yet");
                    } else if status == Some(ShortcutStatus::Unavailable) {
                        description.push_str(", not registered: another app may be using it");
                    }
                    ui.push_id(action, |ui| {
                        let mut reset = false;
                        let mut remove = false;
                        let destinations = action.supports_task_profile();
                        let mut destination = bindings[index].destination;
                        widgets::setting_row_with(
                            ui,
                            |ui| {
                                ui.label(
                                    egui::RichText::new(action.label())
                                        .font(tokens::medium(tokens::BODY_SIZE))
                                        .color(ui.visuals().text_color()),
                                );
                                widgets::hint(ui, &description);
                                ui.horizontal_wrapped(|ui| {
                                    if destinations {
                                        let label = match destination {
                                            DestinationSelection::Inherit => {
                                                "Default destination".to_owned()
                                            }
                                            DestinationSelection::Specific(id) => settings
                                                .destinations
                                                .get(id)
                                                .map(|d| d.name.clone())
                                                .unwrap_or_else(|| "Missing destination".into()),
                                        };
                                        egui::ComboBox::from_id_salt("shortcut-destination")
                                            .width(150.0)
                                            .selected_text(label)
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut destination,
                                                    DestinationSelection::Inherit,
                                                    "Default destination",
                                                );
                                                for d in settings.destinations.iter() {
                                                    ui.selectable_value(
                                                        &mut destination,
                                                        DestinationSelection::Specific(d.id),
                                                        &d.name,
                                                    );
                                                }
                                            });
                                    }
                                    reset = widgets::ghost_button(ui, None, "Default")
                                        .on_hover_text("Restore this action's default key")
                                        .clicked();
                                });
                            },
                            |ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                remove = widgets::ghost_button(ui, None, "Remove").clicked();
                                let enabled = bindings[index].enabled;
                                match shortcut_key_button(
                                    ui,
                                    "shortcut-key",
                                    &mut recording,
                                    ShortcutCapture::Binding(action),
                                    bindings[index].chord,
                                    recorded_shortcut,
                                    "Change key",
                                ) {
                                    Ok(Some(chord)) => {
                                        if let Err(error) = store_shortcut_change(
                                            bindings,
                                            action,
                                            Some(chord),
                                            enabled,
                                        ) {
                                            shortcut_error = Some(error);
                                        }
                                    }
                                    Ok(None) => {}
                                    Err(error) => shortcut_error = Some(error),
                                }
                                widgets::toggle_switch(
                                    ui,
                                    &mut bindings[index].enabled,
                                    &format!("{} enabled", action.label()),
                                );
                            },
                        );
                        bindings[index].destination = destination;
                        let mine = |recording: &Option<(ShortcutCapture, Uuid, _)>| {
                            recording.as_ref().is_some_and(|(target, _, _)| {
                                *target == ShortcutCapture::Binding(action)
                            })
                        };
                        if reset {
                            commands.push(AppCommand::ResetShortcuts {
                                actions: Some(vec![action]),
                                revision: settings.revision,
                            });
                            bindings[index] =
                                crate::app::shortcuts::ShortcutBinding::default_for(action);
                            if mine(&recording) {
                                recording = None;
                            }
                        }
                        if remove {
                            bindings[index].chord = None;
                            bindings[index].enabled = false;
                            if mine(&recording) {
                                recording = None;
                            }
                        }
                    });
                }
                widgets::card_footer(ui, |ui| {
                    let narrow = ui.available_width() < tokens::STACK_BELOW;
                    let can_add = bindings.iter().any(|binding| binding.chord.is_none());
                    let add = ui
                        .add_enabled_ui(can_add && adding.is_none(), |ui| {
                            widgets::toggle_button(ui, false, Some(Icon::Plus), "Add shortcut")
                                .clicked()
                        })
                        .inner;
                    if add
                        && let Some(action) = bindings
                            .iter()
                            .find(|binding| binding.chord.is_none())
                            .map(|binding| binding.action)
                    {
                        adding = Some(NewShortcut {
                            action,
                            chord: None,
                        });
                    }
                    if widgets::ghost_button(ui, None, "Reset all").clicked() {
                        commands.push(AppCommand::ResetShortcuts {
                            actions: None,
                            revision: settings.revision,
                        });
                        clear_draft = true;
                        adding = None;
                        recording = None;
                    }
                    let mut save = |ui: &mut Ui| {
                        if widgets::primary_button(ui, None, "Save shortcuts").clicked() {
                            commands.push(AppCommand::ApplyShortcuts {
                                bindings: bindings.clone(),
                                revision: settings.revision,
                            });
                        }
                    };
                    // Narrow: Save wraps inline; wide: hint and Save sit at the right end.
                    if narrow {
                        save(ui);
                    } else {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            save(ui);
                            widgets::hint(ui, "Changes apply when you save");
                        });
                    }
                });
            },
        );
        if clear_draft {
            self.shortcuts = None;
        }
        self.recording = recording;
        self.adding = adding;
        if let Some(error) = shortcut_error {
            self.error = Some(error);
        }
    }

    fn connections(&mut self, ui: &mut Ui, settings: &Settings, commands: &mut Vec<AppCommand>) {
        let title = if settings.destinations.is_empty() {
            "Connect to Vectr"
        } else {
            "Connections"
        };
        let description = "Where your uploads go. Use an uploader token created in Vectr; your password is never needed.";
        if let Some(draft) = &mut self.draft {
            let mut cancel = false;
            let mut remove = None;
            let editing = draft.id;
            widgets::setting_group(
                ui,
                if editing.is_some() {
                    "Edit connection"
                } else {
                    title
                },
                Some(description),
                |ui| {
                    widgets::card_block(ui, |ui| {
                        if let Some(error) = &self.error {
                            ui.colored_label(
                                ui.visuals().error_fg_color,
                                format!("Could not apply: {error}"),
                            );
                        }
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
                        });
                        if self.saving.is_some() {
                            widgets::hint(ui, "Saving connection…");
                        }
                    });
                    widgets::card_footer(ui, |ui| {
                        cancel = widgets::ghost_button(ui, None, "Cancel").clicked();
                        if let Some(id) = editing
                            && widgets::ghost_button(ui, None, "Remove connection").clicked()
                        {
                            remove = Some(id);
                        }
                    });
                },
            );
            self.just_opened = false;
            if cancel || remove.is_some() {
                self.clear(ui.ctx());
                commands.push(AppCommand::CancelConnectionTest);
                self.remove = remove;
            }
            return;
        }
        let mut edit = None;
        widgets::setting_group(ui, title, Some(description), |ui| {
            if let Some(error) = &self.error {
                widgets::card_block(ui, |ui| {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        format!("Could not apply: {error}"),
                    );
                });
            }
            for d in settings.destinations.iter() {
                ui.push_id(d.id.0, |ui| {
                    let default = Some(d.id) == settings.destinations.default_id();
                    let token = d.credential_ref.is_some();
                    let host = d.origin.as_str();
                    let host = host.strip_prefix("https://").unwrap_or(host).to_owned();
                    let mut detail = host;
                    if let Some(domain) = &d.link_domain_override {
                        detail.push_str(&format!(", links use {}", domain.as_str()));
                    }
                    widgets::setting_row_with(
                        ui,
                        |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                ui.label(
                                    egui::RichText::new(&d.name)
                                        .font(tokens::medium(tokens::BODY_SIZE))
                                        .color(ui.visuals().text_color()),
                                );
                                if default {
                                    widgets::pill(ui, "Default", PillTone::Default);
                                }
                                if token {
                                    widgets::pill(ui, "Token saved", PillTone::Success);
                                } else {
                                    widgets::pill(ui, "Needs a token", PillTone::Warning);
                                }
                            });
                            widgets::hint(ui, &detail);
                        },
                        |ui| {
                            let clicked = if token {
                                widgets::toggle_button(ui, false, None, "Edit")
                                    .on_hover_text(format!("Edit {}", d.name))
                                    .clicked()
                            } else {
                                widgets::primary_button(ui, None, "Add token").clicked()
                            };
                            if clicked {
                                edit = Some(d.id);
                            }
                            if !default
                                && widgets::ghost_button(ui, None, "Make default")
                                    .on_hover_text(format!("Use {} for future uploads", d.name))
                                    .clicked()
                            {
                                commands.push(AppCommand::SetDefault {
                                    id: d.id,
                                    revision: settings.revision,
                                });
                            }
                        },
                    );
                });
            }
            if let Some(id) = self.remove {
                widgets::card_block(ui, |ui| {
                    ui.label("Remove this connection? Unfinished uploads or workflows can prevent removal. Choose another default first when needed.");
                    ui.horizontal_wrapped(|ui| {
                        if widgets::toggle_button(ui, false, None, "Remove").clicked() {
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
                        if widgets::ghost_button(ui, None, "Keep it").clicked() {
                            self.remove = None;
                        }
                    });
                });
            }
            widgets::card_footer(ui, |ui| {
                let add = ui
                    .add_enabled_ui(settings.destinations.len() < 32, |ui| {
                        widgets::toggle_button(ui, false, Some(Icon::Plus), "Add connection")
                            .clicked()
                    })
                    .inner;
                if add {
                    self.draft = Some(ConnectionDraft::new(settings, None));
                    self.just_opened = true;
                    self.error = None;
                }
                if widgets::toggle_button(ui, false, None, "Import VectrCap config…").clicked() {
                    commands.push(AppCommand::ChooseImportConfig);
                }
                // Wide: the drop hint sits at the right end; narrow: it wraps inline.
                if ui.available_width() > 220.0 {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        widgets::hint(ui, "Or drop a .vectrcap file here");
                    });
                } else {
                    widgets::hint(ui, "Or drop a .vectrcap file here");
                }
            });
        });
        if let Some(id) = edit {
            self.draft = Some(ConnectionDraft::new(settings, Some(id)));
            self.just_opened = true;
            self.error = None;
        }
    }
}

fn general(
    ui: &mut Ui,
    settings: &Settings,
    notifications_enabled: bool,
    commands: &mut Vec<AppCommand>,
) {
    widgets::setting_group(ui, "General", None, |ui| {
        let label = "Show a notification when an upload finishes";
        widgets::setting_row(ui, label, None, |ui| {
            let mut enabled = notifications_enabled;
            if widgets::toggle_switch(ui, &mut enabled, label).changed() {
                commands.push(AppCommand::SetNotificationsEnabled(enabled));
            }
        });
        let label = "Start VectrCap when Windows starts";
        widgets::setting_row(ui, label, None, |ui| {
            let mut startup = settings.startup;
            if widgets::toggle_switch(ui, &mut startup.launch_at_sign_in, label).changed() {
                commands.push(AppCommand::SetStartup {
                    startup,
                    revision: settings.revision,
                });
            }
        });
        widgets::setting_row(
            ui,
            "Interface size",
            Some(
                "Scales text and controls on top of Windows display scaling. Captures keep their pixels.",
            ),
            |ui| {
                let mut appearance = settings.appearance;
                egui::ComboBox::from_id_salt("ui-scale")
                    .width(tokens::CONTROL_COLUMN - 16.0)
                    .selected_text(format!("{}%", appearance.ui_scale_percent))
                    .show_ui(ui, |ui| {
                        for scale in
                            crate::persistence::settings::AppearancePreferences::VALID_SCALES
                        {
                            ui.selectable_value(
                                &mut appearance.ui_scale_percent,
                                scale,
                                format!("{scale}%"),
                            );
                        }
                    });
                if appearance != settings.appearance {
                    commands.push(AppCommand::SetAppearance {
                        appearance,
                        revision: settings.revision,
                    });
                }
            },
        );
    });
}

/// The running version and the GitHub Releases update flow (`crate::update`).
fn updates(ui: &mut Ui) {
    use crate::update::{self, Status};
    let status = update::status();
    let (text, action, busy) = match &status {
        Status::Idle => (
            "Checks GitHub for new versions daily.".into(),
            Some("Check now"),
            false,
        ),
        Status::Checking => ("Checking for updates…".into(), None, true),
        Status::NoReleases => (
            "No releases are published yet.".into(),
            Some("Check again"),
            false,
        ),
        Status::UpToDate => (
            "You have the latest version.".into(),
            Some("Check again"),
            false,
        ),
        Status::Available(release) => (
            format!(
                "Version {} is available. VectrCap restarts to install it.",
                release.version
            ),
            Some("Update and restart"),
            false,
        ),
        Status::Downloading(release) => (
            format!("Downloading version {}…", release.version),
            None,
            true,
        ),
        Status::Restarting(release) => (
            format!("Restarting into version {}…", release.version),
            None,
            true,
        ),
        Status::Failed(message) => (message.clone(), Some("Try again"), false),
    };
    widgets::setting_group(ui, "Updates", None, |ui| {
        let label = format!("VectrCap {}", update::CURRENT);
        widgets::setting_row(ui, &label, Some(&text), |ui| {
            if busy {
                ui.spinner();
            }
            if let Some(action) = action {
                let clicked = if matches!(status, Status::Available(_)) {
                    widgets::primary_button(ui, None, action).clicked()
                } else {
                    widgets::ghost_button(ui, None, action).clicked()
                };
                if clicked {
                    match status {
                        Status::Available(_) => update::install(),
                        _ => update::check(),
                    }
                }
            }
        });
    });
}

/// The colors and line width the capture tools start with. A color picked in
/// the capture toolbar is saved here too when the capture ends.
fn drawing(ui: &mut Ui, settings: &Settings, commands: &mut Vec<AppCommand>) {
    let mut tools = settings.tools.clone();
    widgets::setting_group(
        ui,
        "Drawing",
        Some(
            "What the capture tools start with. A color you pick while drawing is remembered here too.",
        ),
        |ui| {
            widgets::setting_row(
                ui,
                "Drawing color",
                Some("Arrows, shapes, lines, text and steps."),
                |ui| {
                    ui.push_id("drawing-color", |ui| {
                        super::annotation_toolbar::color_palette(ui, &mut tools.color);
                    });
                },
            );
            widgets::setting_row(ui, "Highlight color", None, |ui| {
                ui.push_id("highlight-color", |ui| {
                    super::annotation_toolbar::color_palette(ui, &mut tools.highlight_color);
                });
            });
            widgets::setting_row(ui, "Line width", None, |ui| {
                let mut width = f32::from(tools.width_tenths) / 10.0;
                if ui
                    .add(egui::Slider::new(&mut width, 0.5..=64.0).suffix(" px"))
                    .changed()
                {
                    tools.width_tenths = (width * 10.0).round() as u16;
                }
            });
        },
    );
    if tools != settings.tools {
        commands.push(AppCommand::SetToolPreferences {
            tools,
            revision: settings.revision,
        });
    }
}

fn storage(ui: &mut Ui, settings: &Settings, commands: &mut Vec<AppCommand>) {
    use crate::persistence::settings::StorageMode;
    let mut policy = settings.archive.clone();
    widgets::setting_group(
        ui,
        "Local copies",
        Some(
            "Files VectrCap saves on this computer. Working files for uploads and recordings are kept whatever you choose.",
        ),
        |ui| {
            let mode_label = |mode: StorageMode| match mode {
                StorageMode::AlwaysSave => "Always",
                StorageMode::PendingOrFailed => "Until the upload succeeds",
                StorageMode::NeverAutomatic => "Only when a task asks",
            };
            widgets::setting_row(ui, "Keep a local copy", None, |ui| {
                egui::ComboBox::from_id_salt("archive-mode")
                    .width(tokens::CONTROL_COLUMN - 16.0)
                    .selected_text(mode_label(policy.storage_mode))
                    .show_ui(ui, |ui| {
                        for mode in [
                            StorageMode::AlwaysSave,
                            StorageMode::PendingOrFailed,
                            StorageMode::NeverAutomatic,
                        ] {
                            ui.selectable_value(&mut policy.storage_mode, mode, mode_label(mode));
                        }
                    });
            });
            let folder = policy.archive_root.as_ref().map_or_else(
                || "Pictures\\VectrCap".to_owned(),
                |p| p.display().to_string(),
            );
            widgets::setting_row(ui, "Capture folder", Some(&folder), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if widgets::toggle_button(ui, false, Some(Icon::Folder), "Browse…").clicked() {
                    commands.push(AppCommand::ChooseArchiveFolder);
                }
                if policy.archive_root.is_some()
                    && widgets::ghost_button(ui, None, "Use default").clicked()
                {
                    policy.archive_root = None;
                }
            });
            let label = "Delete old copies";
            widgets::setting_row(
                ui,
                label,
                Some(
                    "Only copies VectrCap saved. Runs at launch; files still waiting to upload are kept. Favorites don't change this.",
                ),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let mut cleanup = policy.cleanup_age_days.is_some();
                    if widgets::toggle_switch(ui, &mut cleanup, label).changed() {
                        policy.cleanup_age_days = cleanup.then_some(30);
                    }
                    if let Some(days) = &mut policy.cleanup_age_days {
                        ui.add(
                            egui::DragValue::new(days)
                                .range(1..=3650)
                                .prefix("after ")
                                .suffix(" days"),
                        );
                    }
                },
            );
        },
    );
    if policy != settings.archive {
        commands.push(AppCommand::SetArchivePolicy {
            policy,
            revision: settings.revision,
        });
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
fn shortcut_scope_label(scope: crate::app::shortcuts::ShortcutScope) -> &'static str {
    use crate::app::shortcuts::ShortcutScope;
    match scope {
        ShortcutScope::Global => "Global",
        ShortcutScope::Editor => "Editor",
        ShortcutScope::Ruler => "Ruler",
        ShortcutScope::ScreenSampler => "Screen sampler",
    }
}

fn shortcut_is_shown(binding: &crate::app::shortcuts::ShortcutBinding) -> bool {
    binding.chord.is_some()
}

fn store_shortcut_change(
    bindings: &mut [crate::app::shortcuts::ShortcutBinding],
    action: crate::app::shortcuts::ShortcutAction,
    chord: Option<crate::app::shortcuts::KeyChord>,
    enabled: bool,
) -> Result<(), AppError> {
    let mut candidate = bindings.to_vec();
    let Some(binding) = candidate
        .iter_mut()
        .find(|binding| binding.action == action)
    else {
        return Err(AppError::InvalidInput("unknown shortcut"));
    };
    binding.chord = chord;
    binding.enabled = enabled;
    binding.scope = action.scope();
    crate::app::shortcuts::ShortcutSettings {
        bindings: candidate,
    }
    .validate()?;
    let Some(binding) = bindings.iter_mut().find(|binding| binding.action == action) else {
        return Err(AppError::InvalidInput("unknown shortcut"));
    };
    binding.chord = chord;
    binding.enabled = enabled;
    binding.scope = action.scope();
    Ok(())
}

fn shortcut_key_button(
    ui: &mut Ui,
    id: &'static str,
    recording: &mut Option<(
        ShortcutCapture,
        Uuid,
        crate::platform::windows::hotkeys::ShortcutRecorder,
    )>,
    target: ShortcutCapture,
    current: Option<crate::app::shortcuts::KeyChord>,
    recorded_shortcut: Option<(Uuid, crate::app::shortcuts::KeyChord)>,
    idle_label: &str,
) -> Result<Option<crate::app::shortcuts::KeyChord>, AppError> {
    use crate::platform::windows::hotkeys::{ShortcutRecorder, chord_label};
    let active = recording
        .as_ref()
        .is_some_and(|(captured, _, _)| *captured == target);
    let label = if active {
        "Press a combination...".to_owned()
    } else {
        current
            .map(chord_label)
            .unwrap_or_else(|| idle_label.to_owned())
    };
    let field = ui.push_id(id, |ui| ui.button(label)).inner;
    if field.clicked() {
        field.request_focus();
        *recording = Some((target, Uuid::new_v4(), ShortcutRecorder::start()?));
    }
    if !active {
        return Ok(None);
    }
    if !field.has_focus() || !ui.input(|input| input.focused) {
        *recording = None;
        return Ok(None);
    }
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(16));
    let captured = {
        let Some((_, request, recorder)) = recording.as_mut() else {
            return Ok(None);
        };
        let request = *request;
        recorded_shortcut
            .filter(|(recorded, _)| *recorded == request)
            .map(|(_, chord)| chord)
            .or_else(|| recorder.poll())
    };
    if let Some(chord) = captured {
        *recording = None;
        field.surrender_focus();
        return Ok(Some(chord));
    }
    Ok(None)
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
                    for category in SettingsCategory::ALL {
                        pane.category = category;
                        pane.draw(ui, &settings, true, &[], None);
                    }
                });
            },
        );
        assert!(!output.shapes.is_empty());
        output.textures_delta.clear();
    }
    #[test]
    fn sections_are_the_ten_in_spec_order() {
        assert_eq!(
            SettingsCategory::ALL.map(|c| c.label()),
            [
                "Connections",
                "Capture",
                "Recording",
                "Shortcuts",
                "Tasks",
                "Storage",
                "General",
                "Integrations",
                "Backup",
                "Troubleshooting"
            ]
        );
    }

    /// Names of the switches (checkbox role) a category draws.
    fn switches(category: SettingsCategory) -> Vec<String> {
        labels(category, egui::accesskit::Role::CheckBox)
    }

    /// Names of the widgets with `role` a category draws.
    fn labels(category: SettingsCategory, role: egui::accesskit::Role) -> Vec<String> {
        let context = egui::Context::default();
        context.enable_accesskit();
        let settings = Settings::default();
        let mut pane = SettingsPane::default();
        pane.select(category);
        let mut last = None;
        for _ in 0..2 {
            let mut output = context.run_ui(Default::default(), |ui| {
                pane.draw(ui, &settings, true, &[], None);
            });
            output.textures_delta.clear();
            last = Some(output);
        }
        last.expect("two frames")
            .platform_output
            .accesskit_update
            .iter()
            .flat_map(|update| &update.nodes)
            .filter(|(_, node)| node.role() == role)
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect()
    }

    #[test]
    fn tasks_section_owns_after_capture_and_after_upload_toggles() {
        use crate::workflow::profile::{CaptureTask, UploadTask};
        let tasks = switches(SettingsCategory::Tasks);
        for task in CaptureTask::ALL {
            assert!(
                tasks.iter().any(|l| l == task.label()),
                "{:?} missing",
                task.label()
            );
        }
        for task in UploadTask::ALL {
            assert!(
                tasks.iter().any(|l| l == task.label()),
                "{:?} missing",
                task.label()
            );
        }
        let capture = switches(SettingsCategory::Capture);
        assert!(
            UploadTask::ALL
                .iter()
                .all(|task| !capture.iter().any(|l| l == task.label()))
        );
    }

    #[test]
    fn every_listed_shortcut_can_restore_its_default_key() {
        let shown = Settings::default()
            .shortcuts
            .effective()
            .iter()
            .filter(|binding| shortcut_is_shown(binding))
            .count();
        let defaults = labels(SettingsCategory::Shortcuts, egui::accesskit::Role::Button)
            .into_iter()
            .filter(|label| label == "Default")
            .count();
        assert!(shown > 0);
        assert_eq!(defaults, shown);
    }

    #[test]
    fn capture_offers_drawing_and_highlight_colors() {
        let buttons = labels(SettingsCategory::Capture, egui::accesskit::Role::Button);
        // One palette for the drawing color, one for the highlight color.
        assert_eq!(buttons.iter().filter(|label| *label == "Red").count(), 2);
    }

    #[test]
    fn integrations_are_switches() {
        let integrations = switches(SettingsCategory::Integrations);
        for name in [
            "\"Upload with Vectr\" in Explorer",
            "Open .vectrcap files with VectrCap",
        ] {
            assert!(
                integrations.iter().any(|l| l == name),
                "{name} missing: {integrations:?}"
            );
        }
    }

    #[test]
    fn select_sets_the_shown_category() {
        let mut pane = SettingsPane::default();
        pane.select(SettingsCategory::Shortcuts);
        assert_eq!(pane.category(), SettingsCategory::Shortcuts);
    }
    #[test]
    fn settings_open_on_connections() {
        assert_eq!(
            SettingsPane::default().category(),
            SettingsCategory::Connections
        );
    }
    #[test]
    fn empty_destinations_reopen_selects_connections() {
        let mut pane = SettingsPane::default();
        pane.select(SettingsCategory::Troubleshooting);
        pane.opened(&Settings::default());
        assert_eq!(pane.category(), SettingsCategory::Connections);
        assert!(pane.draft.is_some());
        assert!(pane.just_opened);
    }
    #[test]
    fn switching_category_keeps_the_open_connection_draft_and_save() {
        let context = egui::Context::default();
        let settings = Settings::default();
        let mut pane = SettingsPane::default();
        let request = Uuid::new_v4();
        pane.saving = Some(request);
        pane.draft = Some(ConnectionDraft::new(&settings, None));
        pane.category = SettingsCategory::Troubleshooting;
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            pane.show_categories(ui, false);
            let commands = pane.draw(ui, &settings, true, &[], None);
            assert!(
                commands
                    .iter()
                    .any(|command| matches!(command, AppCommand::SetShortcutEntryActive(None)))
            );
        });
        output.textures_delta.clear();
        assert_eq!(pane.saving, Some(request));
        assert!(pane.draft.is_some());
        assert_eq!(pane.category(), SettingsCategory::Troubleshooting);
    }
    #[test]
    fn other_categories_leave_connection_focus_until_connections_is_drawn() {
        let context = egui::Context::default();
        let settings = Settings::default();
        let mut pane = SettingsPane::default();
        pane.opened(&settings);
        pane.category = SettingsCategory::General;
        let token = egui::Id::unique("connection-token");
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            pane.draw(ui, &settings, true, &[], None);
        });
        output.textures_delta.clear();
        assert!(pane.just_opened);
        assert!(pane.draft.is_some());
        assert!(context.memory(|memory| memory.focused()).is_none());
        pane.category = SettingsCategory::Connections;
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            pane.draw(ui, &settings, true, &[], None);
        });
        output.textures_delta.clear();
        assert!(!pane.just_opened);
        let focused = context.memory(|memory| memory.focused());
        assert!(focused.is_some());
        assert_ne!(focused, Some(token));
    }
    #[test]
    fn shortcut_list_hides_actions_that_have_no_key() {
        use crate::app::shortcuts::{KeyChord, KeyCode, ShortcutAction, ShortcutSettings};
        let bindings = ShortcutSettings::default().effective();
        let shown: Vec<_> = bindings
            .iter()
            .filter(|binding| shortcut_is_shown(binding))
            .map(|binding| binding.action)
            .collect();
        assert!(!shown.contains(&ShortcutAction::RegionScreenshot));
        assert!(!shown.contains(&ShortcutAction::ShowHistory));
        assert!(shown.contains(&ShortcutAction::ToolArrow));
        assert!(shown.iter().all(|action| {
            bindings
                .iter()
                .find(|binding| binding.action == *action)
                .unwrap()
                .chord
                .is_some()
        }));
        let mut bindings = bindings;
        let chord = KeyChord {
            ctrl: true,
            alt: false,
            shift: false,
            win: false,
            key: KeyCode::from_vk(0x70).unwrap(),
        };
        store_shortcut_change(
            &mut bindings,
            ShortcutAction::RegionScreenshot,
            Some(chord),
            true,
        )
        .unwrap();
        assert!(shortcut_is_shown(
            bindings
                .iter()
                .find(|binding| binding.action == ShortcutAction::RegionScreenshot)
                .unwrap()
        ));
        assert!(!shortcut_is_shown(
            bindings
                .iter()
                .find(|binding| binding.action == ShortcutAction::ShowHistory)
                .unwrap()
        ));
        let arrow = bindings
            .iter()
            .find(|binding| binding.action == ShortcutAction::ToolArrow)
            .unwrap()
            .chord
            .unwrap();
        assert!(
            store_shortcut_change(
                &mut bindings,
                ShortcutAction::ToolRectangle,
                Some(arrow),
                true,
            )
            .is_err()
        );
        assert!(
            store_shortcut_change(
                &mut bindings,
                ShortcutAction::FullscreenScreenshot,
                Some(arrow),
                true,
            )
            .is_ok()
        );
    }
}
