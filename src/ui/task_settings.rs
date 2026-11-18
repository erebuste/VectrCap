use crate::{
    app::{
        destinations::{DestinationSelection, Destinations},
        events::AppCommand,
        shortcuts::ShortcutAction,
    },
    persistence::settings::Settings,
    workflow::profile::*,
};
use eframe::egui::{self, Ui};
pub fn destination(ui: &mut Ui, selection: &mut DestinationSelection, destinations: &Destinations) {
    let text = match selection {
        DestinationSelection::Inherit => "Inherit destination".to_owned(),
        DestinationSelection::Specific(id) => destinations
            .get(*id)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| "Missing destination: choose an existing connection".into()),
    };
    egui::ComboBox::from_id_salt("task-destination")
        .selected_text(text)
        .show_ui(ui, |ui| {
            ui.selectable_value(
                selection,
                DestinationSelection::Inherit,
                "Inherit destination",
            );
            for d in destinations.iter() {
                ui.selectable_value(selection, DestinationSelection::Specific(d.id), &d.name);
            }
        });
}
fn scope(ui: &mut Ui, value: &mut ProfileScope) {
    let label = |scope: ProfileScope| match scope {
        ProfileScope::Captures => "Captures only",
        ProfileScope::Files => "Uploaded files only",
        ProfileScope::All => "Captures and files",
    };
    egui::ComboBox::from_id_salt("task-scope")
        .selected_text(label(*value))
        .show_ui(ui, |ui| {
            for scope in [
                ProfileScope::Captures,
                ProfileScope::Files,
                ProfileScope::All,
            ] {
                ui.selectable_value(value, scope, label(scope));
            }
        });
}
fn profile(ui: &mut Ui, p: &mut TaskProfile, s: &Settings) {
    for t in CaptureTask::ALL {
        if t == CaptureTask::SaveFinal {
            continue;
        }
        let mut checked = if t == CaptureTask::BeforeUploadReview {
            p.before_upload_review
        } else {
            p.capture.contains(&t)
        };
        if ui.checkbox(&mut checked, t.label()).changed() {
            if t == CaptureTask::BeforeUploadReview {
                p.before_upload_review = checked;
                p.capture.remove(&t);
            } else if checked {
                p.capture.insert(t);
            } else {
                p.capture.remove(&t);
            }
        }
    }
    ui.label("After confirmed upload");
    for t in UploadTask::ALL {
        let mut checked = p.after_upload.contains(&t);
        if ui.checkbox(&mut checked, t.label()).changed() {
            if checked {
                p.after_upload.insert(t);
            } else {
                p.after_upload.remove(&t);
            }
        }
    }
    ui.label("Upload destination (choosing a target does not enable upload)");
    destination(ui, &mut p.destination, &s.destinations);
    scope(ui, &mut p.scope);
    ui.label("Quick choice / review -> final output -> save -> local actions -> before-upload review -> upload -> link actions. Video cannot Copy image or Pin. File/path actions require a durable save.");
}
/// The Tasks section: the after-capture and after-upload switches (formerly tray
/// submenus, same `SetTaskProfiles` command), destination, scope and overrides.
pub fn draw(ui: &mut Ui, s: &Settings) -> Vec<AppCommand> {
    use super::{tokens, widgets};
    let mut tasks = s.tasks.clone();
    let mut archive = s.archive.clone();
    widgets::setting_group(
        ui,
        "After capture",
        Some(
            "What happens to a screenshot or recording once you finish it. Video can't Copy image or Pin; file and path actions need a saved copy.",
        ),
        |ui| {
            for task in CaptureTask::ALL {
                let mut on = match task {
                    CaptureTask::SaveFinal => tasks.save_final(&archive),
                    CaptureTask::BeforeUploadReview => {
                        tasks.global.before_upload_review || tasks.global.capture.contains(&task)
                    }
                    _ => tasks.global.capture.contains(&task),
                };
                widgets::setting_row(ui, task.label(), None, |ui| {
                    if widgets::toggle_switch(ui, &mut on, task.label()).changed() {
                        match task {
                            CaptureTask::SaveFinal => tasks.set_save_final(&mut archive, on),
                            CaptureTask::BeforeUploadReview => {
                                tasks.global.before_upload_review = on;
                                tasks.global.capture.remove(&task);
                            }
                            _ if on => {
                                tasks.global.capture.insert(task);
                            }
                            _ => {
                                tasks.global.capture.remove(&task);
                            }
                        }
                    }
                });
            }
            widgets::setting_row(
                ui,
                "Destination",
                Some(
                    "Where Upload sends it. Choosing one doesn't turn Upload on; shortcuts can pick their own.",
                ),
                |ui| destination(ui, &mut tasks.global.destination, &s.destinations),
            );
            widgets::setting_row(
                ui,
                "Applies to",
                Some("Which captures and files use these tasks."),
                |ui| scope(ui, &mut tasks.global.scope),
            );
        },
    );
    ui.add_space(tokens::GROUP_GAP);
    widgets::setting_group(
        ui,
        "After upload",
        Some("Runs once the upload is confirmed."),
        |ui| {
            for task in UploadTask::ALL {
                let mut on = tasks.global.after_upload.contains(&task);
                widgets::setting_row(ui, task.label(), None, |ui| {
                    if widgets::toggle_switch(ui, &mut on, task.label()).changed() {
                        if on {
                            tasks.global.after_upload.insert(task);
                        } else {
                            tasks.global.after_upload.remove(&task);
                        }
                    }
                });
            }
        },
    );
    ui.add_space(tokens::GROUP_GAP);
    widgets::setting_group(
        ui,
        "Per-shortcut tasks",
        Some("A shortcut can run its own tasks instead of the ones above."),
        |ui| {
            widgets::card_block(ui, |ui| {
                for &action in ShortcutAction::all()
                    .iter()
                    .filter(|a| a.supports_task_profile())
                {
                    ui.push_id(action, |ui| {
                        ui.collapsing(action.label(), |ui| {
                            let mut custom = tasks.overrides.contains_key(&action);
                            if ui.checkbox(&mut custom, "Use its own tasks").changed() {
                                if custom {
                                    tasks.overrides.insert(action, tasks.global.clone());
                                } else {
                                    tasks.overrides.remove(&action);
                                }
                            }
                            if let Some(p) = tasks.overrides.get_mut(&action) {
                                profile(ui, p, s);
                            } else {
                                widgets::hint(ui, "Uses the tasks above");
                            }
                        });
                    });
                }
            });
        },
    );
    if tasks != s.tasks || archive != s.archive {
        vec![AppCommand::SetTaskProfiles {
            tasks,
            archive,
            revision: s.revision,
        }]
    } else {
        vec![]
    }
}
