use crate::workflow::{
    profile::CaptureTask,
    runner::{TaskCommand, TaskIntent},
};
use eframe::egui::Ui;
pub fn draw(ui: &mut Ui, kind: crate::app::types::ArtifactKind) -> Option<TaskCommand> {
    let mut command = None;
    ui.label("Choose a finish action for the reviewed output");
    for (t, label) in [
        (CaptureTask::Upload, "Upload"),
        (CaptureTask::CopyImage, "Copy image"),
        (CaptureTask::SaveAs, "Save as"),
        (CaptureTask::Pin, "Pin"),
    ] {
        if ui
            .add_enabled(
                t.unavailable_reason(kind).is_none(),
                eframe::egui::Button::new(label),
            )
            .on_hover_text(t.unavailable_reason(kind).unwrap_or(label))
            .clicked()
        {
            command = Some(TaskCommand::Choose(TaskIntent::Direct(t)));
        }
    }
    if ui.button("Run selected tasks").clicked() {
        command = Some(TaskCommand::Choose(TaskIntent::RunSelectedTasks));
    }
    if ui.button("Cancel").clicked() {
        command = Some(TaskCommand::Cancel);
    }
    command
}
