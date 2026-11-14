use crate::{
    app::{
        desktop::{DesktopClient, DesktopView},
        events::{AppCommand, QueueAction},
    },
    persistence::history::JobStatus,
    platform::windows::file_dialog::PickerGate,
};
use eframe::egui::Ui;
pub fn draw(
    ui: &mut Ui,
    view: &DesktopView,
    client: &DesktopClient,
    picker_gate: &PickerGate,
) -> Vec<AppCommand> {
    let mut commands = vec![];
    ui.heading("Uploads");
    if ui.button("Upload file…").clicked() {
        crate::platform::windows::file_dialog::spawn_pick_and_upload(
            client.clone(),
            picker_gate.clone(),
        );
    }
    if !view.failed_uploads.is_empty() {
        ui.add_space(6.0);
        for (name, error) in &view.failed_uploads {
            ui.colored_label(ui.visuals().error_fg_color, format!("{name}: {error}"));
        }
    }
    ui.label(if view.queue_paused {
        "Queue paused. Resume all keeps individually paused jobs paused."
    } else {
        "Queue active. One upload runs at a time."
    });
    ui.horizontal_wrapped(|ui| {
        for (label, action) in [
            ("Pause all", QueueAction::PauseAll),
            ("Resume all", QueueAction::ResumeAll),
            ("Retry eligible failures", QueueAction::RetryEligible),
        ] {
            if ui.button(label).clicked() {
                commands.push(AppCommand::Queue(action));
            }
        }
    });
    ui.add_space(12.0);
    if view.jobs.is_empty() {
        ui.label("No uploads yet.");
    }
    for job in &view.jobs {
        ui.push_id(job.id.0, |ui| {
            ui.label(
                eframe::egui::RichText::new(&job.artifact.display_name)
                    .strong()
                    .color(ui.visuals().text_color()),
            );
            ui.label(format!(
                "{} · {} · {} bytes",
                job.destination.label,
                job.destination
                    .link_domain
                    .as_ref()
                    .map(|d| d.as_str())
                    .unwrap_or("uploader default"),
                job.artifact.size
            ));
            let status = match &job.status {
                JobStatus::Running if job.paused || view.queue_paused => {
                    "Pausing — waiting for the current request"
                }
                JobStatus::Running => "Uploading — waiting for server confirmation",
                JobStatus::Completed(_) => "Uploaded — confirmed",
                JobStatus::Cancelled => "Cancelled",
                JobStatus::Uncertain => "Outcome uncertain — automatic restart blocked",
                JobStatus::Failed { .. } => "Failed — see retry controls",
                _ if job.paused && view.queue_paused => "Paused by job and queue",
                _ if job.paused => "Paused by job",
                _ if view.queue_paused => "Paused by queue",
                _ => "Waiting",
            };
            ui.label(status);
            if let Some((bytes, speed)) = view.transfers.get(&job.id) {
                ui.label(format!(
                    "Acknowledged: {bytes} / {} bytes",
                    job.artifact.size
                ));
                let transferring =
                    job.status == JobStatus::Running && !job.paused && !view.queue_paused;
                ui.label(if transferring {
                    speed
                        .map(|n| format!("Payload speed: {:.1} KiB/s", n / 1024.0))
                        .unwrap_or_else(|| "Payload speed: Waiting".into())
                } else {
                    "Payload speed: Paused / Waiting".into()
                });
            }
            ui.horizontal_wrapped(|ui| {
                if !job.status.is_terminal() {
                    let action = if job.paused {
                        QueueAction::Resume(job.id)
                    } else {
                        QueueAction::Pause(job.id)
                    };
                    if ui
                        .button(if job.paused {
                            "Resume job"
                        } else {
                            "Pause job"
                        })
                        .clicked()
                    {
                        commands.push(AppCommand::Queue(action));
                    }
                    if ui.button("Cancel upload").clicked() {
                        commands.push(AppCommand::Queue(QueueAction::Cancel(job.id)));
                    }
                }
                // Restart of uncertain submissions requires a separate deliberate
                // confirmation flow, deferred with file ingress/receipt actions.
                if let JobStatus::Failed { code } = &job.status {
                    ui.label(format!("Failure: {code}"));
                }
            });
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
        });
    }
    if view.jobs.len() == 100 {
        ui.label("Showing up to 100 jobs, with active and unfinished work first.");
    }
    commands
}
