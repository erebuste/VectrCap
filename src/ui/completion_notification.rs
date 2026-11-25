//! Passive completion toast. It never activates a window or opens a link itself.
use crate::app::{
    controller::NoticeDisplay,
    desktop::DesktopView,
    events::AppCommand,
    notifications::{CompletionStatus, LocalPresence, NoticeId, RequestedCompletionAction},
    types::{Artifact, ArtifactKind, JobId},
};
use eframe::egui::{self, Color32, TextureHandle, Ui};
use std::{
    collections::{HashMap, HashSet},
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant},
};

const PICTURE_W: f32 = 196.0;
const PICTURE_H: f32 = 112.0;
const CAPTION_H: f32 = 18.0;
const PAD: f32 = 8.0;
const GAP: f32 = 8.0;
/// ShareX: 3 s on screen plus a 1 s fade.
const TOAST_TTL: Duration = Duration::from_secs(4);
/// Same inset Windows uses between a native toast and the work-area corner.
const CORNER_MARGIN: f32 = 16.0;

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

/// Click contract for one notice. A batch opens its summary. A single uploaded
/// item opens that item's confirmed link. Anything else does not.
pub fn primary_action(notice: &NoticeDisplay) -> Option<RequestedCompletionAction> {
    if notice.view.is_batch() {
        return notice
            .actions
            .iter()
            .find(|action| matches!(action, RequestedCompletionAction::OpenBatchSummary { .. }))
            .cloned();
    }
    let item = notice.view.items().first()?;
    if !item.has_confirmed_receipt() {
        return None;
    }
    notice
        .actions
        .iter()
        .find(|action| matches!(action, RequestedCompletionAction::OpenLink(id) if *id == item.job_id()))
        .cloned()
}

pub fn draw(
    ui: &mut Ui,
    view: &DesktopView,
    shown: &mut HashMap<NoticeId, Instant>,
) -> Vec<AppCommand> {
    paint(ui, view, shown, None)
}

pub fn draw_with_thumbs(
    ui: &mut Ui,
    view: &DesktopView,
    shown: &mut HashMap<NoticeId, Instant>,
    thumbs: &mut NoticeThumbs,
) -> Vec<AppCommand> {
    thumbs.sync(ui.ctx(), view);
    paint(ui, view, shown, Some(thumbs))
}

fn paint(
    ui: &mut Ui,
    view: &DesktopView,
    shown: &mut HashMap<NoticeId, Instant>,
    thumbs: Option<&NoticeThumbs>,
) -> Vec<AppCommand> {
    let now = Instant::now();
    shown.retain(|id, _| {
        view.notifications_enabled && view.notices.iter().any(|notice| notice.view.id() == *id)
    });
    if !view.notifications_enabled {
        return vec![];
    }
    for notice in &view.notices {
        shown.entry(notice.view.id()).or_insert(now);
    }
    let expired = expired_notice_ids(shown, now, TOAST_TTL);
    let mut commands: Vec<_> = expired
        .iter()
        .copied()
        .map(AppCommand::DismissNotice)
        .collect();
    let fill = toast_fill(ui);
    ui.painter()
        .rect_filled(ui.max_rect(), egui::CornerRadius::ZERO, fill);
    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::NONE)
        .inner_margin(egui::Margin::same(PAD as i8))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = GAP;
            for notice in &view.notices {
                let id = notice.view.id();
                if expired.contains(&id) {
                    continue;
                }
                commands.extend(card(ui, notice, thumbs));
            }
        });
    commands
}

fn card(ui: &mut Ui, notice: &NoticeDisplay, thumbs: Option<&NoticeThumbs>) -> Vec<AppCommand> {
    let action = primary_action(notice);
    // Every card takes clicks: right click closes it, as in ShareX.
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(PICTURE_W, PICTURE_H + CAPTION_H),
        egui::Sense::click(),
    );
    if response.secondary_clicked() {
        return vec![AppCommand::DismissNotice(notice.view.id())];
    }
    let picture = egui::Rect::from_min_size(rect.min, egui::vec2(PICTURE_W, PICTURE_H));
    let caption = egui::Rect::from_min_size(
        egui::pos2(rect.min.x, picture.max.y),
        egui::vec2(PICTURE_W, CAPTION_H),
    );
    let placeholder = if ui.visuals().override_text_color.is_some() {
        ui.visuals().faint_bg_color
    } else {
        Color32::from_rgb(214, 214, 214)
    };
    ui.painter()
        .rect_filled(picture, egui::CornerRadius::same(4), placeholder);
    if let Some(job) = preview_job(notice)
        && let Some(texture) = thumbs.and_then(|thumbs| thumbs.texture(job))
    {
        paint_fit(ui, picture, texture);
    }
    let status = (!notice.view.is_batch())
        .then(|| notice.view.items().first().map(|item| item.status()))
        .flatten();
    ui.painter().with_clip_rect(caption).text(
        caption.left_center(),
        egui::Align2::LEFT_CENTER,
        caption_text(notice),
        egui::FontId::proportional(12.0),
        caption_color(ui, status),
    );
    if response.hovered() && action.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let response = response.on_hover_text(hover_text(action.as_ref()));
    if response.clicked()
        && let Some(action) = action
    {
        return vec![
            AppCommand::ExecuteCompletionAction {
                notice: notice.view.id(),
                action,
            },
            AppCommand::DismissNotice(notice.view.id()),
        ];
    }
    vec![]
}

fn paint_fit(ui: &Ui, bounds: egui::Rect, texture: &TextureHandle) {
    let original = texture.size_vec2();
    if original.x <= 0.0 || original.y <= 0.0 {
        return;
    }
    let size = original * ((bounds.width() / original.x).min(bounds.height() / original.y));
    ui.painter().image(
        texture.id(),
        egui::Rect::from_center_size(bounds.center(), size),
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        Color32::WHITE,
    );
}

fn caption_text(notice: &NoticeDisplay) -> String {
    if notice.view.is_batch() {
        let count = notice.view.items().len();
        return if count == 1 {
            "1 file".to_owned()
        } else {
            format!("{count} files")
        };
    }
    match notice.view.items().first().map(|item| item.status()) {
        Some(CompletionStatus::Uploaded) => notice
            .view
            .items()
            .first()
            .map(|item| item.display_name().to_owned())
            .unwrap_or_default(),
        Some(CompletionStatus::Failed(_)) => "Upload failed".to_owned(),
        Some(CompletionStatus::LocalOnly) => "Local only".to_owned(),
        Some(CompletionStatus::Uncertain) => "Outcome uncertain".to_owned(),
        Some(CompletionStatus::Cancelled) => "Cancelled".to_owned(),
        None => String::new(),
    }
}

fn hover_text(action: Option<&RequestedCompletionAction>) -> &'static str {
    match action {
        Some(RequestedCompletionAction::OpenLink(_)) => "Open in browser",
        Some(RequestedCompletionAction::OpenBatchSummary { .. }) => "View all",
        _ => "Closes on its own",
    }
}

fn toast_fill(ui: &Ui) -> Color32 {
    if ui.visuals().override_text_color.is_some() {
        ui.visuals().panel_fill
    } else {
        Color32::from_rgb(244, 244, 244)
    }
}

fn caption_color(ui: &Ui, status: Option<&CompletionStatus>) -> Color32 {
    if ui.visuals().override_text_color.is_some() {
        return ui.visuals().text_color();
    }
    match status {
        Some(CompletionStatus::Failed(_)) => Color32::from_rgb(168, 32, 40),
        Some(CompletionStatus::Uploaded) | None => Color32::from_rgb(28, 28, 28),
        _ => Color32::from_rgb(90, 90, 90),
    }
}

fn preview_job(notice: &NoticeDisplay) -> Option<JobId> {
    notice
        .view
        .items()
        .iter()
        .find(|item| item.local() != LocalPresence::Missing)
        .map(|item| item.job_id())
}

fn find_artifact(view: &DesktopView, id: JobId) -> Option<&Artifact> {
    if let Some(job) = view.jobs.iter().find(|job| job.id == id) {
        return Some(&job.artifact);
    }
    view.history_page.records.iter().find_map(|record| {
        record
            .job
            .as_ref()
            .is_some_and(|job| job.id == id)
            .then_some(&record.artifact)
    })
}

struct ThumbRequest {
    id: JobId,
    artifact: Artifact,
}

struct ThumbResult {
    id: JobId,
    image: Option<egui::ColorImage>,
}

/// One decoder thread for the toast. Textures are created on the UI thread.
pub struct NoticeThumbs {
    requests: Option<SyncSender<ThumbRequest>>,
    results: Receiver<ThumbResult>,
    pending: HashSet<JobId>,
    images: HashMap<JobId, Option<TextureHandle>>,
}

impl Default for NoticeThumbs {
    fn default() -> Self {
        Self::new()
    }
}

impl NoticeThumbs {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel::<ThumbRequest>(4);
        let (output, results) = mpsc::sync_channel(8);
        let spawned = std::thread::Builder::new()
            .name("notice-thumb".into())
            .spawn(move || {
                while let Ok(request) = rx.recv() {
                    let image = super::history_previews::decode_artifact(
                        &request.artifact.display_name,
                        kind_name(request.artifact.kind),
                        &request.artifact,
                    );
                    if output
                        .send(ThumbResult {
                            id: request.id,
                            image,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .is_ok();
        Self {
            requests: spawned.then_some(tx),
            results,
            pending: HashSet::new(),
            images: HashMap::new(),
        }
    }

    pub fn clear(&mut self) {
        self.images.clear();
        self.pending.clear();
        while self.results.try_recv().is_ok() {}
    }

    fn texture(&self, id: JobId) -> Option<&TextureHandle> {
        self.images.get(&id).and_then(|image| image.as_ref())
    }

    fn sync(&mut self, ctx: &egui::Context, view: &DesktopView) {
        let wanted: Vec<JobId> = view.notices.iter().filter_map(preview_job).collect();
        self.images.retain(|id, _| wanted.contains(id));
        self.pending.retain(|id| wanted.contains(id));
        while let Ok(result) = self.results.try_recv() {
            self.pending.remove(&result.id);
            if !wanted.contains(&result.id) {
                continue;
            }
            self.images.insert(
                result.id,
                result.image.map(|image| {
                    ctx.load_texture(
                        format!("notice-{}", result.id.0),
                        image,
                        egui::TextureOptions::LINEAR,
                    )
                }),
            );
        }
        for id in wanted {
            if self.images.contains_key(&id) || self.pending.contains(&id) {
                continue;
            }
            let Some(artifact) = find_artifact(view, id).cloned() else {
                continue;
            };
            let Some(tx) = &self.requests else {
                self.images.insert(id, None);
                continue;
            };
            if tx.try_send(ThumbRequest { id, artifact }).is_ok() {
                self.pending.insert(id);
            }
        }
        if !self.pending.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}

impl Drop for NoticeThumbs {
    fn drop(&mut self) {
        self.requests.take();
    }
}

fn kind_name(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Screenshot => "screenshot",
        ArtifactKind::Gif => "gif",
        ArtifactKind::Mp4 => "mp4",
        ArtifactKind::File => "file",
    }
}

/// Completion windows never alter the main shell's visibility or focus.
pub fn surface_visible(view: &DesktopView) -> bool {
    view.notifications_enabled
        && view.capture_acquiring.is_none()
        && view.capture_surface.is_none()
        && !view.notices.is_empty()
        && view.lifecycle == crate::app::events::Lifecycle::Running
}

fn toast_size(count: usize) -> [f32; 2] {
    let count = count.clamp(1, crate::app::notifications::MAX_VISIBLE_NOTICES) as f32;
    let card = PICTURE_H + CAPTION_H;
    let height = PAD * 2.0 + count * card + (count - 1.0) * GAP;
    [PICTURE_W + PAD * 2.0, height]
}

pub fn viewport_builder() -> eframe::egui::ViewportBuilder {
    viewport_builder_for(1, 1.0)
}

pub fn viewport_builder_for(count: usize, pixels_per_point: f32) -> eframe::egui::ViewportBuilder {
    let size = toast_size(count);
    let scale = if pixels_per_point.is_finite() && (0.5..=8.0).contains(&pixels_per_point) {
        pixels_per_point
    } else {
        1.0
    };
    let mut builder = eframe::egui::ViewportBuilder::default()
        .with_title("VectrCap notifications")
        .with_inner_size(size)
        .with_min_inner_size(size)
        .with_max_inner_size(size)
        .with_resizable(false)
        .with_decorations(false)
        .with_active(false)
        .with_taskbar(false)
        .with_always_on_top();
    if let Some(position) = toast_position(size, scale) {
        builder = builder.with_position(position);
    }
    builder
}

/// ShareX shows the toast on the active monitor: the one under the pointer.
fn toast_position(size: [f32; 2], scale: f32) -> Option<[f32; 2]> {
    use crate::platform::windows::{monitors, windows::physical_pointer};
    let area = physical_pointer()
        .ok()
        .and_then(|p| {
            monitors::enumerate_work_areas()
                .ok()?
                .into_iter()
                .find(|a| a.contains(p))
        })
        .or_else(|| monitors::primary_work_area().ok())?;
    Some(toast_corner(area, size, scale))
}

/// Bottom-right of a work area, in logical points. The work area already excludes the taskbar.
fn toast_corner(
    area: crate::capture::geometry::PhysicalRect,
    size: [f32; 2],
    scale: f32,
) -> [f32; 2] {
    let scale = if scale.is_finite() && (0.5..=8.0).contains(&scale) {
        scale
    } else {
        1.0
    };
    let left = area.left() as f32 / scale;
    let top = area.top() as f32 / scale;
    let right = area.right() as f32 / scale;
    let bottom = area.bottom() as f32 / scale;
    [
        (right - size[0] - CORNER_MARGIN).max(left),
        (bottom - size[1] - CORNER_MARGIN).max(top),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::geometry::PhysicalRect;

    #[test]
    fn toast_sits_in_the_bottom_right_of_the_work_area() {
        let area = PhysicalRect::new(0, 0, 1920, 1080).unwrap();
        let size = [212.0, 146.0];
        let [x, y] = toast_corner(area, size, 1.0);
        assert_eq!(x, 1920.0 - size[0] - CORNER_MARGIN);
        assert_eq!(y, 1080.0 - size[1] - CORNER_MARGIN);
    }

    #[test]
    fn toast_corner_follows_a_scaled_work_area_that_excludes_the_taskbar() {
        let area = PhysicalRect::new(-1920, 40, 1920, 1000).unwrap();
        let size = [212.0, 146.0];
        let [x, y] = toast_corner(area, size, 2.0);
        assert_eq!(x, 0.0 - size[0] - CORNER_MARGIN);
        assert_eq!(y, 520.0 - size[1] - CORNER_MARGIN);
    }

    #[test]
    fn toast_corner_stays_inside_a_work_area_smaller_than_the_card() {
        let area = PhysicalRect::new(10, 20, 100, 80).unwrap();
        let [x, y] = toast_corner(area, [212.0, 146.0], 1.0);
        assert_eq!(x, 10.0);
        assert_eq!(y, 20.0);
    }
}
