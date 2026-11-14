//! Read-only summary of the exact jobs selected by a completion notice.
use crate::{app::desktop::DesktopView, persistence::history::JobStatus};
use eframe::egui::{Label, Ui};

pub fn draw(ui: &mut Ui, view: &DesktopView) {
    ui.heading("Batch summary");
    ui.label(format!("{} selected uploads", view.batch_summary.len()));
    if view.batch_summary.is_empty() {
        ui.label("No batch selected.");
    }
    for row in &view.batch_summary {
        ui.push_id(row.id.0, |ui| {
            let Some(job) = &row.job else {
                ui.label("Upload record no longer available.");
                ui.monospace(row.id.0.to_string());
                ui.separator();
                return;
            };
            ui.strong(&job.artifact.display_name);
            ui.label(&job.destination.label);
            ui.label(match &job.status {
                JobStatus::Completed(_) => "Uploaded",
                JobStatus::Failed { .. } => "Failed",
                JobStatus::Cancelled => "Cancelled",
                JobStatus::Uncertain => "Outcome uncertain",
                JobStatus::Running => "Uploading",
                _ => "Waiting",
            });
            if let JobStatus::Failed { code } = &job.status {
                ui.label(format!("Failure: {code}"));
            }
            if let JobStatus::Completed(receipt) = &job.status {
                ui.add(Label::new(receipt.url()).selectable(true).wrap());
            }
            if !row.local_available {
                ui.label("Local file unavailable.");
            }
            ui.separator();
        });
    }
}
