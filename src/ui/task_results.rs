use crate::{
    app::{desktop::DesktopClient, events::AppCommand},
    platform::windows::file_dialog::{self, PickerGate},
    workflow::{profile::CaptureTask, runner::*, steps::*},
};
use eframe::egui::Ui;
use uuid::Uuid;
fn submit(commands: &mut Vec<AppCommand>, id: Uuid, command: TaskCommand) {
    commands.push(AppCommand::Task { id, command });
}
fn choose_save(
    ui: &mut Ui,
    r: &TaskRunner,
    client: &DesktopClient,
    gate: &PickerGate,
    label: &str,
) {
    if ui.button(label).clicked()
        && let Some(a) = &r.context().artifact
    {
        file_dialog::spawn_task_save(
            client.clone(),
            gate.clone(),
            r.context().id,
            a.display_name.clone(),
        );
    }
}
fn row(
    ui: &mut Ui,
    r: &TaskRunner,
    client: &DesktopClient,
    gate: &PickerGate,
    commands: &mut Vec<AppCommand>,
) {
    let c = r.context();
    ui.label(
        c.artifact
            .as_ref()
            .map_or("Capture workflow", |a| a.display_name.as_str()),
    );
    let target = c.destination.as_ref().map_or_else(
        || {
            c.destination_issue
                .clone()
                .unwrap_or_else(|| "Upload destination needs setup".into())
        },
        |d| format!("Frozen target: {} ({})", d.label, d.origin.as_str()),
    );
    ui.label(target);
    if c.artifact.is_some() && ui.button("Inspect finalized output").clicked() {
        submit(commands, c.id, TaskCommand::Inspect);
    }
    if let Some(receipt) = &c.receipt {
        ui.label(receipt.url());
    }
    for state in r.states() {
        ui.push_id(state.step.label(), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{}: {}", state.step.label(), state.outcome.label()));
                if let Some(reason) = state.outcome.reason() {
                    ui.label(reason);
                }
                if matches!(
                    state.outcome,
                    StepOutcome::Failed(_) | StepOutcome::NeedsUserAction(_)
                ) {
                    if ui.button("Retry").clicked() {
                        submit(commands, c.id, TaskCommand::Retry(state.step));
                    }
                    if state.step.can_skip() && ui.button("Skip").clicked() {
                        submit(commands, c.id, TaskCommand::Skip(state.step));
                    }
                    if matches!(state.step,TaskStep::Capture(t) if t.needs_durable()) {
                        choose_save(ui, r, client, gate, "Save durable copy...");
                    }
                }
            });
            if state.outcome != StepOutcome::Running {
                return;
            }
            match state.step {
                TaskStep::Capture(CaptureTask::QuickMenu) => {
                    if let Some(command) = super::quick_tasks::draw(ui, c.kind) {
                        submit(commands, c.id, command);
                    }
                }
                TaskStep::Capture(CaptureTask::Review | CaptureTask::BeforeUploadReview) => {
                    ui.label(
                        "Inspect the finalized output and frozen destination before publishing.",
                    );
                    if ui.button("Confirm reviewed output").clicked() {
                        submit(commands, c.id, TaskCommand::Approve);
                    }
                }
                TaskStep::Capture(CaptureTask::SaveAs) => {
                    choose_save(ui, r, client, gate, "Choose Save as destination...")
                }
                TaskStep::Capture(CaptureTask::Upload) => {
                    ui.label("Manage failed or uncertain uploads in Uploads.");
                }
                _ => {}
            }
        });
    }
    if !r.complete() && ui.button("Cancel remaining tasks").clicked() {
        submit(commands, c.id, TaskCommand::Cancel);
    }
}
pub fn draw(
    ui: &mut Ui,
    rows: &[TaskRunner],
    client: &DesktopClient,
    gate: &PickerGate,
) -> Vec<AppCommand> {
    let mut commands = vec![];
    for r in rows {
        ui.push_id(r.context().id, |ui| {
            ui.group(|ui| row(ui, r, client, gate, &mut commands));
        });
    }
    commands
}
