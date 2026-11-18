//! History's "In progress" section: uploads still moving and work waiting on the user.
use super::{
    icons::{self, Icon},
    tokens::*,
    widgets,
};
use crate::{
    app::{
        controller::ingress::PendingSelectionView,
        desktop::DesktopView,
        error::AppError,
        events::{AppCommand, QueueAction},
        types::ArtifactKind,
    },
    persistence::history::{JobStatus, UploadJob},
    platform::windows::appearance,
    workflow::{
        profile::CaptureTask,
        runner::{TaskCommand, TaskRunner},
        steps::{StepOutcome, StepState, TaskStep},
    },
};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Rect, Ui, UiBuilder, pos2, vec2,
};
use uuid::Uuid;

/// One In progress row, in display order (see `rank`).
enum Row<'a> {
    Attention(&'a TaskRunner, &'a StepState),
    Job(&'a UploadJob, u8),
    Rejected(&'a str, &'a AppError),
    Pending(&'a PendingSelectionView),
}
impl Row<'_> {
    /// 0 attention, 1 uploading, 2 waiting, 3 paused, 4 failed/uncertain,
    /// 5 rejected files, 6 Explorer selections waiting for setup.
    fn rank(&self) -> u8 {
        match self {
            Row::Attention(..) => 0,
            Row::Job(_, rank) => *rank,
            Row::Rejected(..) => 5,
            Row::Pending(_) => 6,
        }
    }
}

/// The step a workflow waits on the user for, if any.
fn attention_step(runner: &TaskRunner) -> Option<&StepState> {
    let states = runner.states();
    states
        .iter()
        .find(|s| {
            matches!(
                s.outcome,
                StepOutcome::Failed(_) | StepOutcome::NeedsUserAction(_)
            )
        })
        .or_else(|| {
            states.iter().find(|s| {
                s.outcome == StepOutcome::Running
                    && matches!(
                        s.step,
                        TaskStep::Capture(
                            CaptureTask::QuickMenu
                                | CaptureTask::Review
                                | CaptureTask::BeforeUploadReview
                                | CaptureTask::SaveAs
                        )
                    )
            })
        })
}

fn job_rank(job: &UploadJob, queue_paused: bool) -> Option<u8> {
    match job.status {
        _ if job.status.is_terminal() => None,
        JobStatus::Failed { .. } | JobStatus::Uncertain => Some(4),
        JobStatus::Running => Some(1),
        _ if job.paused || queue_paused => Some(3),
        _ => Some(2),
    }
}

fn rows(view: &DesktopView) -> Vec<Row<'_>> {
    let mut rows: Vec<Row> = view
        .task_workflows
        .iter()
        .filter_map(|r| attention_step(r).map(|s| Row::Attention(r, s)))
        .collect();
    let mut jobs: Vec<Row> = view
        .jobs
        .iter()
        .filter_map(|j| job_rank(j, view.queue_paused).map(|rank| Row::Job(j, rank)))
        .collect();
    jobs.sort_by_key(Row::rank);
    rows.extend(jobs);
    rows.extend(
        view.failed_uploads
            .iter()
            .map(|(name, error)| Row::Rejected(name, error)),
    );
    rows.extend(view.pending_ingress.iter().map(Row::Pending));
    rows
}

pub fn in_progress_count(view: &DesktopView) -> usize {
    rows(view).len()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    Primary,
    Normal,
    Ghost,
    GhostIcon,
}
struct Action {
    look: Look,
    icon: Option<Icon>,
    label: &'static str,
    command: AppCommand,
}
fn action(look: Look, icon: Option<Icon>, label: &'static str, command: AppCommand) -> Action {
    Action {
        look,
        icon,
        label,
        command,
    }
}
impl Action {
    fn width(&self, ui: &Ui) -> f32 {
        let label = (self.look != Look::GhostIcon).then_some(self.label);
        widgets::button_width(ui, self.icon, label)
    }
    fn show(&self, ui: &mut Ui) -> egui::Response {
        match self.look {
            Look::Primary => widgets::primary_button(ui, self.icon, self.label),
            Look::Normal => widgets::toggle_button(ui, false, self.icon, self.label),
            Look::Ghost => widgets::ghost_button(ui, self.icon, self.label),
            Look::GhostIcon => {
                widgets::ghost_icon_button(ui, self.icon.unwrap_or(Icon::ChevronRight), self.label)
            }
        }
    }
}

/// Row actions for an unfinished upload; the same commands the old Uploads page sent.
fn job_actions(job: &UploadJob, failed_jobs: usize) -> Vec<Action> {
    let queue = |a| AppCommand::Queue(a);
    let mut actions = vec![];
    let failed = matches!(job.status, JobStatus::Failed { .. });
    if failed && failed_jobs == 1 {
        actions.push(action(
            Look::Normal,
            Some(Icon::Retry),
            "Retry",
            queue(QueueAction::RetryEligible),
        ));
    }
    if job.paused {
        actions.push(action(
            Look::GhostIcon,
            Some(Icon::Play),
            "Resume this upload",
            queue(QueueAction::Resume(job.id)),
        ));
    } else if !failed && job.status != JobStatus::Uncertain {
        actions.push(action(
            Look::GhostIcon,
            Some(Icon::Pause),
            "Pause this upload",
            queue(QueueAction::Pause(job.id)),
        ));
    }
    actions.push(if failed {
        action(
            Look::Ghost,
            None,
            "Remove",
            queue(QueueAction::Cancel(job.id)),
        )
    } else {
        action(
            Look::GhostIcon,
            Some(Icon::X),
            "Cancel this upload",
            queue(QueueAction::Cancel(job.id)),
        )
    });
    actions
}

fn pending_actions(pending: &PendingSelectionView) -> Vec<Action> {
    vec![
        action(
            Look::Normal,
            Some(Icon::Retry),
            "Retry",
            AppCommand::ResumeIngress,
        ),
        action(
            Look::Ghost,
            None,
            "Cancel",
            AppCommand::CancelPendingIngress(pending.request),
        ),
    ]
}

/// Actions for a workflow waiting on the user, plus whether it shows the quick menu.
fn workflow_actions(runner: &TaskRunner, state: &StepState) -> (Vec<Action>, bool) {
    let context = runner.context();
    let id = context.id;
    let task = |command| AppCommand::Task { id, command };
    let save = || {
        context
            .artifact
            .as_ref()
            .map(|a| AppCommand::ChooseTaskSave {
                id,
                name: a.display_name.clone(),
            })
    };
    let mut actions = vec![];
    let mut quick_menu = false;
    match (&state.outcome, state.step) {
        (StepOutcome::Failed(_) | StepOutcome::NeedsUserAction(_), step) => {
            let durable = matches!(step, TaskStep::Capture(t) if t.needs_durable());
            match save() {
                Some(command) if durable => {
                    actions.push(action(Look::Primary, None, "Choose folder…", command))
                }
                _ => actions.push(action(
                    Look::Normal,
                    Some(Icon::Retry),
                    "Retry",
                    task(TaskCommand::Retry(step)),
                )),
            }
            if step.can_skip() {
                actions.push(action(
                    Look::Ghost,
                    None,
                    "Skip",
                    task(TaskCommand::Skip(step)),
                ));
            }
        }
        (_, TaskStep::Capture(CaptureTask::QuickMenu)) => quick_menu = true,
        (_, TaskStep::Capture(CaptureTask::SaveAs)) => {
            if let Some(command) = save() {
                actions.push(action(Look::Primary, None, "Save as…", command));
            }
        }
        _ => {
            actions.push(action(
                Look::Primary,
                None,
                "Confirm",
                task(TaskCommand::Approve),
            ));
            actions.push(action(
                Look::Ghost,
                None,
                "Inspect",
                task(TaskCommand::Inspect),
            ));
        }
    }
    if !runner.complete() {
        actions.push(action(
            Look::GhostIcon,
            Some(Icon::X),
            "Cancel remaining tasks",
            task(TaskCommand::Cancel),
        ));
    }
    (actions, quick_menu)
}

fn attention_detail(state: &StepState) -> String {
    let durable = matches!(state.step, TaskStep::Capture(t) if t.needs_durable());
    match (&state.outcome, state.step) {
        (StepOutcome::Failed(_) | StepOutcome::NeedsUserAction(_), _) if durable => {
            "Needs your attention: choose where to save it".into()
        }
        (StepOutcome::NeedsUserAction(reason), _) => format!("Needs your attention: {reason}"),
        (StepOutcome::Failed(reason), step) => {
            format!("Needs your attention: {} failed, {reason}", step.label())
        }
        (_, TaskStep::Capture(CaptureTask::QuickMenu)) => {
            "Needs your attention: choose what to do with it".into()
        }
        (_, TaskStep::Capture(CaptureTask::SaveAs)) => {
            "Needs your attention: choose where to save it".into()
        }
        _ => "Needs your attention: review it before it uploads".into(),
    }
}

fn failure_reason(code: &str) -> String {
    match code {
        "transient" => "the connection was interrupted".into(),
        "rate_limited" => "the server asked to slow down".into(),
        "retry_exhausted" => "it kept failing after several tries".into(),
        other => other.replace(['_', '-', '.'], " "),
    }
}

#[derive(Clone, Copy)]
enum Tone {
    Muted,
    Primary,
    Warning,
    Destructive,
}

/// Everything one row paints.
struct RowView {
    icon: Icon,
    tone: Tone,
    name: String,
    detail: String,
    progress: Option<f32>,
    actions: Vec<Action>,
    quick_menu: Option<(Uuid, ArtifactKind)>,
}

fn row_view(row: &Row, view: &DesktopView, failed_jobs: usize) -> RowView {
    match row {
        Row::Attention(runner, state) => {
            let context = runner.context();
            let (actions, quick) = workflow_actions(runner, state);
            RowView {
                icon: Icon::Attention,
                tone: Tone::Warning,
                name: context
                    .artifact
                    .as_ref()
                    .map_or("Capture".into(), |a| a.display_name.clone()),
                detail: attention_detail(state),
                progress: None,
                actions,
                quick_menu: quick.then_some((context.id, context.kind)),
            }
        }
        Row::Job(job, _) => {
            let size = super::history::format_size;
            let (icon, tone, detail, progress) = match &job.status {
                JobStatus::Failed { code } => (
                    Icon::Alert,
                    Tone::Destructive,
                    format!(
                        "Upload failed: {}. The local file is safe.",
                        failure_reason(code)
                    ),
                    None,
                ),
                JobStatus::Uncertain => (
                    Icon::Alert,
                    Tone::Destructive,
                    "Upload outcome unknown: it may have reached the server, so it won't restart on its own."
                        .into(),
                    None,
                ),
                JobStatus::Running if job.paused || view.queue_paused => (
                    Icon::Pause,
                    Tone::Muted,
                    "Pausing, finishing the current request".into(),
                    None,
                ),
                JobStatus::Running => {
                    let target = &job.destination.label;
                    match view.transfers.get(&job.id) {
                        Some((sent, speed)) => (
                            Icon::Upload,
                            Tone::Primary,
                            format!(
                                "Uploading to {target}, {} of {}{}",
                                size(*sent),
                                size(job.artifact.size),
                                speed
                                    .map(|s| format!(", {}/s", size(s as u64)))
                                    .unwrap_or_default()
                            ),
                            Some(*sent as f32 / job.artifact.size.max(1) as f32),
                        ),
                        None => (
                            Icon::Upload,
                            Tone::Primary,
                            format!("Uploading to {target}"),
                            Some(0.0),
                        ),
                    }
                }
                _ if job.paused => (
                    Icon::Pause,
                    Tone::Muted,
                    "Paused. Resume it to continue.".into(),
                    None,
                ),
                _ if view.queue_paused => (
                    Icon::Pause,
                    Tone::Muted,
                    "Paused with all uploads".into(),
                    None,
                ),
                _ => (
                    Icon::File,
                    Tone::Muted,
                    "Waiting, starts after the current upload".into(),
                    None,
                ),
            };
            RowView {
                icon,
                tone,
                name: job.artifact.display_name.clone(),
                detail,
                progress,
                actions: job_actions(job, failed_jobs),
                quick_menu: None,
            }
        }
        Row::Rejected(name, error) => RowView {
            icon: Icon::Alert,
            tone: Tone::Destructive,
            name: (*name).to_owned(),
            detail: format!("Couldn't upload: {error}"),
            progress: None,
            actions: vec![],
            quick_menu: None,
        },
        Row::Pending(pending) => RowView {
            icon: Icon::File,
            tone: Tone::Muted,
            name: if pending.import {
                "Config file waiting for setup".into()
            } else {
                format!("{} file(s) waiting for setup", pending.count)
            },
            detail: pending.names.join(", "),
            progress: None,
            actions: pending_actions(pending),
            quick_menu: None,
        },
    }
}

fn tone_color(ui: &Ui, tone: Tone) -> Color32 {
    let visuals = ui.visuals();
    match tone {
        Tone::Muted => visuals.weak_text_color(),
        Tone::Primary => visuals.selection.bg_fill,
        Tone::Warning => appearance::warning(visuals),
        Tone::Destructive => visuals.error_fg_color,
    }
}

const QUICK_MENU_LABEL: &str = "Choose task…";

/// Lays `actions` out in `area` (right-aligned unless `stacked`) and records clicks.
fn place_actions(
    ui: &mut Ui,
    area: Rect,
    stacked: bool,
    row: RowView,
    commands: &mut Vec<AppCommand>,
) {
    let layout = if stacked {
        Layout::left_to_right(Align::Center)
    } else {
        Layout::right_to_left(Align::Center)
    };
    ui.scope_builder(UiBuilder::new().max_rect(area).layout(layout), |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let mut ordered: Vec<Action> = row.actions;
        if !stacked {
            ordered.reverse();
        }
        let quick = |ui: &mut Ui, commands: &mut Vec<AppCommand>| {
            if let Some((id, kind)) = row.quick_menu {
                ui.menu_button(QUICK_MENU_LABEL, |ui| {
                    if let Some(command) = super::quick_tasks::draw(ui, kind) {
                        commands.push(AppCommand::Task { id, command });
                        ui.close();
                    }
                });
            }
        };
        if stacked {
            quick(ui, commands);
        }
        for action in ordered {
            if action.show(ui).clicked() {
                commands.push(action.command);
            }
        }
        if !stacked {
            quick(ui, commands);
        }
    });
}

fn draw_row(ui: &mut Ui, row: RowView, divider: bool, commands: &mut Vec<AppCommand>) {
    let width = ui.available_width();
    let stacked = width < STACK_BELOW;
    let mut actions_width: f32 = row.actions.iter().map(|a| a.width(ui) + 4.0).sum();
    if row.quick_menu.is_some() {
        actions_width += widgets::button_width(ui, None, Some(QUICK_MENU_LABEL)) + 4.0;
    }
    let text_left = 14.0 + GLYPH_BOX + 12.0;
    let mut text_width = width - text_left - 12.0;
    if !stacked && actions_width > 0.0 {
        text_width -= actions_width + 8.0;
    }
    let text_width = text_width.max(40.0);
    let visuals = ui.visuals().clone();
    let color = tone_color(ui, row.tone);
    let mut name = egui::text::LayoutJob::simple_singleline(
        row.name.clone(),
        medium(BODY_SIZE),
        visuals.text_color(),
    );
    name.wrap = egui::text::TextWrapping {
        max_width: text_width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let name = ui.painter().layout_job(name);
    let detail_color = match row.tone {
        Tone::Warning | Tone::Destructive => color,
        _ => visuals.weak_text_color(),
    };
    let detail = ui.painter().layout(
        row.detail.clone(),
        FontId::proportional(SMALL_SIZE),
        detail_color,
        text_width,
    );
    let bar = if row.progress.is_some() {
        6.0 + PROGRESS_HEIGHT
    } else {
        0.0
    };
    let text_height = name.size().y + 2.0 + detail.size().y + bar;
    let has_actions = actions_width > 0.0;
    let content = if stacked {
        text_height.max(GLYPH_BOX)
            + if has_actions {
                8.0 + BUTTON_HEIGHT_SMALL
            } else {
                0.0
            }
    } else {
        text_height.max(GLYPH_BOX).max(BUTTON_HEIGHT_SMALL)
    };
    let (rect, _) = ui.allocate_exact_size(vec2(width, content + 20.0), egui::Sense::hover());
    let inner = Rect::from_min_max(
        pos2(rect.left() + 14.0, rect.top() + 10.0),
        pos2(rect.right() - 12.0, rect.bottom() - 10.0),
    );
    let text_top = if stacked {
        inner.top()
    } else {
        inner.center().y - text_height / 2.0
    };
    let glyph_top = if stacked {
        inner.top()
    } else {
        inner.center().y - GLYPH_BOX / 2.0
    };
    let painter = ui.painter().clone();
    let glyph = Rect::from_min_size(pos2(inner.left(), glyph_top), vec2(GLYPH_BOX, GLYPH_BOX));
    painter.rect_filled(
        glyph,
        CornerRadius::same(RADIUS_CONTROL),
        visuals.widgets.inactive.bg_fill,
    );
    icons::paint(&painter, icons::slot(glyph, ICON_SIZE), row.icon, color);
    let x = rect.left() + text_left;
    let name_height = name.size().y;
    painter.galley(pos2(x, text_top), name, visuals.text_color());
    let detail_top = text_top + name_height + 2.0;
    let detail_height = detail.size().y;
    painter.galley(pos2(x, detail_top), detail, detail_color);
    if let Some(progress) = row.progress {
        let track = Rect::from_min_size(
            pos2(x, detail_top + detail_height + 6.0),
            vec2(text_width, PROGRESS_HEIGHT),
        );
        let radius = CornerRadius::same((PROGRESS_HEIGHT / 2.0) as u8);
        painter.rect_filled(track, radius, visuals.widgets.inactive.bg_fill);
        let mut done = track;
        done.set_width(track.width() * progress.clamp(0.0, 1.0));
        painter.rect_filled(done, radius, visuals.selection.bg_fill);
    }
    if has_actions {
        let area = if stacked {
            Rect::from_min_size(
                pos2(x, text_top + text_height.max(GLYPH_BOX) + 8.0),
                vec2(inner.right() - x, BUTTON_HEIGHT_SMALL),
            )
        } else {
            Rect::from_min_max(
                pos2(inner.right() - actions_width, inner.top()),
                inner.right_bottom(),
            )
        };
        place_actions(ui, area, stacked, row, commands);
    }
    if divider {
        painter.hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            visuals.widgets.noninteractive.bg_stroke,
        );
    }
}

fn draw_head(ui: &mut Ui, view: &DesktopView, count: usize, commands: &mut Vec<AppCommand>) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(
        vec2(width, 18.0 + BUTTON_HEIGHT_SMALL),
        egui::Sense::hover(),
    );
    let visuals = ui.visuals().clone();
    let painter = ui.painter().clone();
    let title = painter.text(
        pos2(rect.left() + 14.0, rect.center().y),
        Align2::LEFT_CENTER,
        "In progress",
        semibold(BODY_SIZE),
        visuals.text_color(),
    );
    painter.text(
        pos2(title.right() + 8.0, rect.center().y),
        Align2::LEFT_CENTER,
        count.to_string(),
        FontId::proportional(BODY_SIZE),
        visuals.weak_text_color(),
    );
    let mut actions = vec![];
    if view
        .jobs
        .iter()
        .any(|j| matches!(j.status, JobStatus::Failed { .. }))
    {
        actions.push(action(
            Look::Ghost,
            Some(Icon::Retry),
            "Retry failed",
            AppCommand::Queue(QueueAction::RetryEligible),
        ));
    }
    if view
        .jobs
        .iter()
        .any(|j| job_rank(j, view.queue_paused).is_some())
    {
        actions.push(if view.queue_paused {
            action(
                Look::Ghost,
                Some(Icon::Play),
                "Resume all",
                AppCommand::Queue(QueueAction::ResumeAll),
            )
        } else {
            action(
                Look::Ghost,
                Some(Icon::Pause),
                "Pause all",
                AppCommand::Queue(QueueAction::PauseAll),
            )
        });
    }
    let area = Rect::from_min_max(
        pos2(rect.left() + 120.0, rect.top()),
        pos2(rect.right() - 12.0, rect.bottom()),
    );
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(area)
            .layout(Layout::right_to_left(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for action in actions.into_iter().rev() {
                if action.show(ui).clicked() {
                    commands.push(action.command);
                }
            }
        },
    );
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        visuals.widgets.noninteractive.bg_stroke,
    );
}

/// The In progress card; draws nothing when nothing is in progress.
/// `expanded` lifts the five-row limit.
pub fn draw(ui: &mut Ui, view: &DesktopView, expanded: &mut bool) -> Vec<AppCommand> {
    let mut commands = vec![];
    let rows = rows(view);
    if rows.is_empty() {
        return commands;
    }
    let failed_jobs = view
        .jobs
        .iter()
        .filter(|j| matches!(j.status, JobStatus::Failed { .. }))
        .count();
    let total = rows.len();
    let shown = if *expanded {
        total
    } else {
        total.min(IN_PROGRESS_MAX_ROWS)
    };
    let visuals = ui.visuals().clone();
    egui::Frame::NONE
        .fill(visuals.extreme_bg_color)
        .stroke(visuals.widgets.noninteractive.bg_stroke)
        .corner_radius(CornerRadius::same(RADIUS_SURFACE))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            draw_head(ui, view, total, &mut commands);
            let more = total > IN_PROGRESS_MAX_ROWS;
            for (i, row) in rows.iter().take(shown).enumerate() {
                let view = row_view(row, view, failed_jobs);
                ui.push_id(i, |ui| {
                    draw_row(ui, view, i + 1 < shown || more, &mut commands)
                });
            }
            if more {
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    let label = if *expanded {
                        "Show fewer".to_owned()
                    } else {
                        format!("Show all ({total})")
                    };
                    if widgets::ghost_button(ui, None, &label).clicked() {
                        *expanded = !*expanded;
                    }
                });
                ui.add_space(5.0);
            }
        });
    ui.add_space(18.0);
    commands
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{controller::ingress::PendingSelectionView, events::QueueAction, types::JobId},
        persistence::history::JobStatus,
        platform::windows::appearance,
        ui::fixtures::{MockupVariant, mockup_view},
    };
    use eframe::egui;

    fn render(view: &DesktopView, expanded: bool) -> egui::FullOutput {
        render_with(|ui| {
            let mut expanded = expanded;
            draw(ui, view, &mut expanded);
        })
    }
    fn render_with(mut draw: impl FnMut(&mut egui::Ui)) -> egui::FullOutput {
        let ctx = egui::Context::default();
        appearance::install_fonts(&ctx);
        ctx.enable_accesskit();
        ctx.set_visuals(appearance::visuals());
        let mut last = None;
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(940.0, 2000.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| draw(ui));
            output.textures_delta.clear();
            last = Some(output);
        }
        last.expect("two frames ran")
    }
    fn texts(output: &egui::FullOutput) -> Vec<String> {
        output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect()
    }
    fn buttons(output: &egui::FullOutput) -> Vec<String> {
        output
            .platform_output
            .accesskit_update
            .iter()
            .flat_map(|update| &update.nodes)
            .filter(|(_, node)| node.role() == egui::accesskit::Role::Button)
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect()
    }
    fn waiting_jobs(n: usize) -> DesktopView {
        let mut view = mockup_view(MockupVariant::HistoryEmpty);
        let template = view.jobs[0].clone();
        view.jobs = (0..n)
            .map(|i| {
                let mut job = template.clone();
                job.id = JobId::new_random();
                job.status = JobStatus::Pending;
                job.artifact.display_name = format!("job-{i}");
                job
            })
            .collect();
        view
    }

    #[test]
    fn busy_view_lists_attention_first_then_uploads_then_failures() {
        let view = mockup_view(MockupVariant::HistoryBusy);
        let ranks: Vec<u8> = rows(&view).iter().map(Row::rank).collect();
        assert_eq!(ranks.first(), Some(&0), "attention first: {ranks:?}");
        assert!(ranks.windows(2).all(|w| w[0] <= w[1]), "{ranks:?}");
        assert!(ranks.contains(&1) && ranks.contains(&2) && ranks.contains(&4));
        assert_eq!(in_progress_count(&view), ranks.len());
        let text = texts(&render(&view, false));
        let attention = text
            .iter()
            .position(|t| t.starts_with("Needs your attention"));
        let uploading = text.iter().position(|t| t.starts_with("Uploading to"));
        let failed = text.iter().position(|t| t.starts_with("Upload failed"));
        assert!(attention < uploading && uploading < failed, "{text:?}");
        assert!(attention.is_some());
    }

    #[test]
    fn empty_view_draws_no_section() {
        let view = mockup_view(MockupVariant::HistoryEmpty);
        assert_eq!(in_progress_count(&view), 0);
        let output = render(&view, false);
        assert!(texts(&output).iter().all(|t| t != "In progress"));
    }

    #[test]
    fn more_than_five_rows_collapse_behind_show_all() {
        let view = waiting_jobs(60);
        let output = render(&view, false);
        let text = texts(&output);
        for i in 0..5 {
            assert!(text.contains(&format!("job-{i}")), "job-{i} missing");
        }
        assert!(!text.contains(&"job-5".to_owned()));
        assert!(buttons(&output).contains(&"Show all (60)".to_owned()));
        let expanded = texts(&render(&view, true));
        assert!(expanded.contains(&"job-59".to_owned()));
    }

    #[test]
    fn retry_failed_only_when_something_failed() {
        let busy = mockup_view(MockupVariant::HistoryBusy);
        assert!(buttons(&render(&busy, false)).contains(&"Retry failed".to_owned()));
        let mut calm = busy.clone();
        calm.jobs
            .retain(|job| !matches!(job.status, JobStatus::Failed { .. }));
        assert!(!buttons(&render(&calm, false)).contains(&"Retry failed".to_owned()));
    }

    #[test]
    fn every_uploads_page_action_is_still_reachable() {
        let view = waiting_jobs(1);
        let mut job = view.jobs[0].clone();
        let id = job.id;
        let commands = |job: &crate::persistence::history::UploadJob| {
            job_actions(job, 1)
                .into_iter()
                .map(|a| a.command)
                .collect::<Vec<_>>()
        };
        let has = |list: &[AppCommand], f: &dyn Fn(&AppCommand) -> bool| list.iter().any(f);
        job.status = JobStatus::Running;
        let running = commands(&job);
        assert!(has(
            &running,
            &|c| matches!(c, AppCommand::Queue(QueueAction::Pause(j)) if *j == id)
        ));
        assert!(has(
            &running,
            &|c| matches!(c, AppCommand::Queue(QueueAction::Cancel(j)) if *j == id)
        ));
        job.paused = true;
        let paused = commands(&job);
        assert!(has(
            &paused,
            &|c| matches!(c, AppCommand::Queue(QueueAction::Resume(j)) if *j == id)
        ));
        job.paused = false;
        job.status = JobStatus::Failed {
            code: "transient".into(),
        };
        let failed = commands(&job);
        assert!(has(&failed, &|c| matches!(
            c,
            AppCommand::Queue(QueueAction::RetryEligible)
        )));
        assert!(has(
            &failed,
            &|c| matches!(c, AppCommand::Queue(QueueAction::Cancel(j)) if *j == id)
        ));
        let request = uuid::Uuid::new_v4();
        let pending = pending_actions(&PendingSelectionView {
            request,
            count: 2,
            names: vec!["a.png".into(), "b.png".into()],
            import: false,
        })
        .into_iter()
        .map(|a| a.command)
        .collect::<Vec<_>>();
        assert!(has(
            &pending,
            &|c| matches!(c, AppCommand::CancelPendingIngress(r) if *r == request)
        ));
        assert!(has(&pending, &|c| matches!(c, AppCommand::ResumeIngress)));
    }

    #[test]
    fn history_header_has_upload_file_but_not_upload_from_clipboard() {
        let view = mockup_view(MockupVariant::HistoryBusy);
        let mut pane = crate::ui::history::HistoryPane::default();
        let labels = buttons(&render_with(|ui| {
            pane.draw_header(ui, &view);
        }));
        assert!(labels.contains(&"Upload file".to_owned()), "{labels:?}");
        assert!(!labels.iter().any(|l| l.contains("clipboard")));
    }
}
