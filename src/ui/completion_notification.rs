//! Passive notices rendered inside the existing shell window.
use crate::app::{
    desktop::DesktopView,
    events::AppCommand,
    notifications::{CompletionStatus, NoticeId, RequestedCompletionAction},
};
use eframe::egui::Ui;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

pub fn expired_notice_ids(
    shown: &HashMap<NoticeId, Instant>,
    now: Instant,
    ttl: Duration,
) -> Vec<NoticeId> {
    shown
        .iter()
        .filter_map(|(id, start)| (now.saturating_duration_since(*start) >= ttl).then_some(*id))
        .collect()
}
fn label(action: &RequestedCompletionAction) -> &'static str {
    use RequestedCompletionAction::*;
    match action {
        OpenLocal(_) => "Open file",
        ShowFolder(_) => "Show in folder",
        OpenLink(_) => "Open link",
        CopyLink(_) => "Copy link",
        CopyAgain(_) => "Copy link (retry)",
        OpenBatchSummary { .. } => "View all",
        CopySuccessfulLinks { .. } => "Copy successful links",
    }
}
pub fn draw(
    ui: &mut Ui,
    view: &DesktopView,
    shown: &mut HashMap<NoticeId, Instant>,
) -> Vec<AppCommand> {
    let now = Instant::now();
    shown.retain(|id, _| {
        view.notifications_enabled && view.notices.iter().any(|n| n.view.id() == *id)
    });
    if !view.notifications_enabled {
        return vec![];
    }
    for notice in &view.notices {
        shown.entry(notice.view.id()).or_insert(now);
    }
    let expired = expired_notice_ids(shown, now, Duration::from_secs(6));
    let mut commands: Vec<_> = expired
        .iter()
        .copied()
        .map(AppCommand::DismissNotice)
        .collect();
    for notice in &view.notices {
        let id = notice.view.id();
        if expired.contains(&id) {
            continue;
        }
        ui.group(|ui| {
            for item in notice.view.items() {
                ui.label(item.display_name());
                ui.label(match item.status() {
                    CompletionStatus::Uploaded => "Uploaded",
                    CompletionStatus::Failed(_) => "Upload failed",
                    CompletionStatus::LocalOnly => "Local only",
                });
            }
            ui.horizontal_wrapped(|ui| {
                for action in &notice.actions {
                    if ui.button(label(action)).clicked() {
                        commands.push(AppCommand::ExecuteCompletionAction {
                            notice: id,
                            action: action.clone(),
                        });
                    }
                }
                if ui.button("Dismiss").clicked() {
                    commands.push(AppCommand::DismissNotice(id));
                }
            });
        });
    }
    commands
}
