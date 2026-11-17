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
    egui::ComboBox::from_id_salt("task-scope")
        .selected_text(format!("{:?}", p.scope))
        .show_ui(ui, |ui| {
            for scope in [
                ProfileScope::Captures,
                ProfileScope::Files,
                ProfileScope::All,
            ] {
                ui.selectable_value(&mut p.scope, scope, format!("{scope:?}"));
            }
        });
    ui.label("Quick choice / review -> final output -> save -> local actions -> before-upload review -> upload -> link actions. Video cannot Copy image or Pin. File/path actions require a durable save.");
}
pub fn draw(ui: &mut Ui, s: &Settings) -> Vec<AppCommand> {
    let mut tasks = s.tasks.clone();
    let mut archive = s.archive.clone();
    ui.label("Run selected tasks confirms the configured chain. Enter in the editor exposes finish actions. Direct Upload, Copy, Save and Pin run that action and the local-save policy.");
    let mut save = tasks.save_final(&archive);
    if ui
        .checkbox(&mut save, "Save final output (Always save archive policy)")
        .changed()
    {
        tasks.set_save_final(&mut archive, save);
    }
    ui.label("Archive storage mode is the single Save final output setting; Pending or failed follows upload intent.");
    ui.collapsing("Global profile", |ui| profile(ui, &mut tasks.global, s));
    ui.collapsing("Per-action / hotkey overrides", |ui| {
        for &action in ShortcutAction::all()
            .iter()
            .filter(|a| a.supports_task_profile())
        {
            ui.push_id(action, |ui| {
                ui.collapsing(action.label(), |ui| {
                    let mut custom = tasks.overrides.contains_key(&action);
                    if ui
                        .checkbox(&mut custom, "Override global task profile")
                        .changed()
                    {
                        if custom {
                            tasks.overrides.insert(action, tasks.global.clone());
                        } else {
                            tasks.overrides.remove(&action);
                        }
                    }
                    if let Some(p) = tasks.overrides.get_mut(&action) {
                        profile(ui, p, s);
                    } else {
                        ui.label("Inherits the global task profile");
                    }
                });
            });
        }
    });
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
