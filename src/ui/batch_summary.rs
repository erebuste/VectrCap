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
            ui.label(
                eframe::egui::RichText::new(&job.artifact.display_name)
                    .strong()
                    .color(ui.visuals().text_color()),
            );
            ui.label(&job.destination.label);
            ui.label(outcome(&job.status, job.paused, view.queue_paused));
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

fn outcome(status: &JobStatus, paused: bool, queue_paused: bool) -> &'static str {
    match status {
        JobStatus::Completed(_) => "Uploaded",
        JobStatus::Failed { .. } => "Failed",
        JobStatus::Cancelled => "Cancelled",
        JobStatus::Uncertain => "Outcome uncertain",
        JobStatus::Running if paused || queue_paused => "Pausing",
        JobStatus::Running => "Uploading",
        _ if paused && queue_paused => "Paused by job and queue",
        _ if paused => "Paused by job",
        _ if queue_paused => "Paused by queue",
        _ => "Waiting",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paused_batch_rows_are_not_labeled_waiting() {
        assert_eq!(outcome(&JobStatus::Pending, false, true), "Paused by queue");
        assert_eq!(
            outcome(&JobStatus::Interrupted, true, false),
            "Paused by job"
        );
        assert_eq!(
            outcome(&JobStatus::Pending, true, true),
            "Paused by job and queue"
        );
        assert_eq!(outcome(&JobStatus::Running, true, false), "Pausing");
        assert_eq!(outcome(&JobStatus::Interrupted, false, false), "Waiting");
        assert_eq!(
            outcome(&JobStatus::Uncertain, false, true),
            "Outcome uncertain"
        );
        assert_eq!(outcome(&JobStatus::Cancelled, true, true), "Cancelled");
    }
}
