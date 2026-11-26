use crate::{
    app::{controller::RecordingView, desktop::DesktopClient, error::AppError, events::AppCommand},
    recording::{
        review::RecordingMetadata,
        session::{RecordingInput, RecordingState},
    },
};
use eframe::egui;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
pub fn metadata_label(m: &RecordingMetadata) -> String {
    use crate::recording::options::AudioMode;
    let audio = match m.audio {
        AudioMode::Off => "No audio",
        AudioMode::System => "System audio",
        AudioMode::Microphone => "Microphone",
        AudioMode::Both => "System + mic audio",
    };
    format!(
        "{} × {}  ·  {} fps  ·  {:.1} s  ·  {}  ·  {audio}",
        m.width,
        m.height,
        m.fps,
        m.active_millis as f64 / 1000.0,
        super::history::format_size(m.size),
    )
}
pub struct PlaybackFrame {
    pub image: egui::ColorImage,
    pub delay: Duration,
}
pub struct GifPlayback {
    pub frames: mpsc::Receiver<Result<PlaybackFrame, AppError>>,
    cancel: tokio_util::sync::CancellationToken,
}
impl GifPlayback {
    pub fn open(path: PathBuf) -> Result<Self, AppError> {
        let (tx, frames) = mpsc::sync_channel(2);
        let cancel = tokio_util::sync::CancellationToken::new();
        let token = cancel.clone();
        std::thread::Builder::new()
            .name("vectrcap-gif-review".into())
            .spawn(move || {
                let result = (|| {
                    while !token.is_cancelled() {
                        let file = std::fs::File::open(&path)
                            .map_err(|e| AppError::io("opening finalized GIF", &e))?;
                        let mut options = gif::DecodeOptions::new();
                        options.set_color_output(gif::ColorOutput::RGBA);
                        let mut reader = options
                            .read_info(std::io::BufReader::new(file))
                            .map_err(|_| AppError::Encoder("reading GIF review"))?;
                        crate::capture::geometry::PhysicalRect::new(
                            0,
                            0,
                            u32::from(reader.width()),
                            u32::from(reader.height()),
                        )?
                        .rgba_len()?;
                        while let Some(frame) = reader
                            .read_next_frame()
                            .map_err(|_| AppError::Encoder("decoding GIF review"))?
                        {
                            if token.is_cancelled() {
                                return Ok(());
                            }
                            let image = image::RgbaImage::from_raw(
                                u32::from(frame.width),
                                u32::from(frame.height),
                                frame.buffer.to_vec(),
                            )
                            .ok_or(AppError::Encoder("invalid GIF frame"))?;
                            let preview = image::imageops::thumbnail(
                                &image,
                                image.width().min(640),
                                image.height().min(360),
                            );
                            let mut packet = Ok(PlaybackFrame {
                                image: egui::ColorImage::from_rgba_unmultiplied(
                                    [preview.width() as usize, preview.height() as usize],
                                    preview.as_raw(),
                                ),
                                delay: Duration::from_millis(u64::from(frame.delay.max(1)) * 10),
                            });
                            loop {
                                match tx.try_send(packet) {
                                    Ok(()) => break,
                                    Err(mpsc::TrySendError::Disconnected(_)) => return Ok(()),
                                    Err(mpsc::TrySendError::Full(p)) => {
                                        packet = p;
                                        if token.is_cancelled() {
                                            return Ok(());
                                        }
                                        std::thread::sleep(Duration::from_millis(10));
                                    }
                                }
                            }
                        }
                    }
                    Ok(())
                })();
                if let Err(e) = result {
                    let _ = tx.try_send(Err(e));
                }
            })
            .map_err(|e| AppError::io("starting GIF review", &e))?;
        Ok(Self { frames, cancel })
    }
}
impl Drop for GifPlayback {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
struct PlayerState {
    id: uuid::Uuid,
    decoder: Option<GifPlayback>,
    texture: Option<egui::TextureHandle>,
    next: Instant,
    error: Option<AppError>,
}
#[derive(Default)]
pub struct ReviewPlayer {
    state: Option<Arc<Mutex<PlayerState>>>,
}
impl ReviewPlayer {
    pub fn clear(&mut self) {
        self.state = None;
    }
    pub fn show(&mut self, ctx: &egui::Context, client: &DesktopClient, view: &RecordingView) {
        if self
            .state
            .as_ref()
            .is_none_or(|s| s.lock().is_ok_and(|s| s.id != view.id))
        {
            let decoder = if !view.mp4 {
                view.playback_path
                    .clone()
                    .map(GifPlayback::open)
                    .transpose()
            } else {
                Ok(None)
            };
            let (decoder, error) = match decoder {
                Ok(d) => (d, None),
                Err(e) => (None, Some(e)),
            };
            self.state = Some(Arc::new(Mutex::new(PlayerState {
                id: view.id,
                decoder,
                texture: None,
                next: Instant::now(),
                error,
            })));
        }
        let Some(state) = self.state.clone() else {
            return;
        };
        let client = client.clone();
        let id = view.id;
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of(("recording-review", id)),
            egui::ViewportBuilder::default()
                .with_title("VectrCap recording review")
                // MP4 review is text and buttons; GIF also shows a ≤640×360 preview.
                .with_inner_size(if view.mp4 {
                    [540.0, 250.0]
                } else {
                    [680.0, 600.0]
                }),
            move |ui, _| {
                let snapshot = client.snapshot();
                let Some(recording) = snapshot.recording.filter(|r| {
                    r.id == id && matches!(r.state, RecordingState::Review | RecordingState::Failed)
                }) else {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                };
                let mut commands = Vec::new();
                if ui.ctx().input(|i| i.viewport().close_requested()) {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    commands.push(AppCommand::CloseRecordingReview);
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Frame::new().inner_margin(20).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if let Ok(mut player) = state.try_lock() {
                            player.draw(ui);
                        }
                        commands.extend(draw_review(ui, &recording, snapshot.error.as_ref()));
                    });
                });
                for command in commands {
                    if matches!(command, AppCommand::ChooseArchiveFolder) {
                        if let Some(settings) = &snapshot.settings {
                            crate::platform::windows::file_dialog::spawn_pick_archive_folder(
                                client.clone(),
                                settings.archive.clone(),
                                settings.revision,
                            );
                        }
                        continue;
                    }
                    if let Err(error) = client.submit(command) {
                        client.report_error(error);
                    }
                }
                ui.ctx().request_repaint_after(Duration::from_millis(16));
            },
        );
    }
}
impl PlayerState {
    fn draw(&mut self, ui: &mut egui::Ui) {
        if Instant::now() >= self.next
            && let Some(decoder) = &self.decoder
            && let Ok(frame) = decoder.frames.try_recv()
        {
            match frame {
                Ok(frame) => {
                    self.texture = Some(ui.ctx().load_texture(
                        "recording-gif",
                        frame.image,
                        egui::TextureOptions::LINEAR,
                    ));
                    self.next = Instant::now() + frame.delay;
                }
                Err(error) => self.error = Some(error),
            }
        }
        if let Some(texture) = &self.texture {
            ui.vertical_centered(|ui| {
                ui.add(
                    egui::Image::new(texture)
                        .max_size(egui::vec2(640.0, 360.0))
                        .corner_radius(super::tokens::RADIUS_CONTROL),
                );
            });
            ui.add_space(14.0);
        }
        if let Some(error) = &self.error {
            ui.label(error.to_string());
        }
    }
}
/// Header, stats line, a "Saved to" card, notices, then one action bar.
fn draw_review(
    ui: &mut egui::Ui,
    recording: &RecordingView,
    action_error: Option<&AppError>,
) -> Vec<AppCommand> {
    use super::{
        icons::Icon,
        tokens::*,
        widgets::{self, PillTone},
    };
    use egui::{Align, Layout, RichText};
    let mut commands = Vec::new();
    let mut push = |c| commands.push(c);
    let failed = recording.state == RecordingState::Failed;
    let (strong, weak) = (ui.visuals().text_color(), ui.visuals().weak_text_color());

    ui.horizontal(|ui| {
        let title = if failed {
            "Recording failed"
        } else {
            "Recording ready"
        };
        ui.label(RichText::new(title).font(semibold(TITLE_SIZE - 2.0)).color(strong));
        widgets::pill(ui, if recording.mp4 { "MP4" } else { "GIF" }, PillTone::Neutral);
        if recording.interruption.is_some() {
            widgets::pill(ui, "Interrupted", PillTone::Warning);
        }
        if recording.uploaded {
            widgets::pill(ui, "Upload queued", PillTone::Success);
        }
    });
    if let Some(metadata) = &recording.metadata {
        ui.label(RichText::new(metadata_label(metadata)).size(SMALL_SIZE).color(weak));
    }
    ui.add_space(12.0);

    if let Some(path) = &recording.archive_path {
        widgets::card(ui, |ui| {
            widgets::card_block(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width((ui.available_width() - 150.0).max(0.0));
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        let folder = path.parent().unwrap_or(path).to_string_lossy();
                        ui.add(
                            egui::Label::new(RichText::new(name).font(medium(BODY_SIZE)))
                                .truncate(),
                        )
                        .on_hover_text(path.to_string_lossy());
                        ui.add(
                            egui::Label::new(RichText::new(folder).size(SMALL_SIZE).color(weak))
                                .truncate(),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if recording.mp4
                            && recording.playback_path.is_some()
                            && widgets::icon_button(ui, Icon::Play, "Play").clicked()
                        {
                            push(AppCommand::OpenRecordingPlayer);
                        }
                    });
                });
            });
        });
        ui.add_space(10.0);
    }

    widgets::hint(ui, &recording.task_target);
    if let Some(reason) = recording.interruption {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            format!(
                "{} Only the part before it was kept; check it before uploading.",
                reason.label()
            ),
        );
    }
    for error in [recording.error.as_ref(), action_error]
        .into_iter()
        .flatten()
    {
        ui.colored_label(ui.visuals().error_fg_color, error.to_string());
    }
    if failed {
        widgets::hint(
            ui,
            "Fix the recording or audio settings and select the area again, or start without audio.",
        );
    }
    if recording.archive_failed {
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if widgets::icon_button(ui, Icon::Retry, "Retry save").clicked() {
                push(AppCommand::RetryRecordingArchive {
                    continue_without: false,
                });
            }
            if widgets::icon_button(ui, Icon::Folder, "Choose folder").clicked() {
                push(AppCommand::ChooseArchiveFolder);
            }
            if widgets::ghost_button(ui, None, "Continue without local copy").clicked() {
                push(AppCommand::RetryRecordingArchive {
                    continue_without: true,
                });
            }
        });
    }

    // Action bar: destructive on the left, the main action rightmost.
    ui.add_space(14.0);
    let line = ui.visuals().widgets.noninteractive.bg_stroke;
    let y = ui.cursor().top();
    ui.painter().hline(ui.max_rect().x_range(), y, line);
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let discard = if failed { "Discard session" } else { "Discard" };
        if widgets::ghost_button(ui, Some(Icon::X), discard).clicked() {
            push(AppCommand::Recording(RecordingInput::Discard));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if failed {
                if recording.mp4
                    && widgets::primary_button(ui, None, "Start without audio").clicked()
                {
                    push(AppCommand::Recording(RecordingInput::StartWithoutAudio {
                        now: Duration::ZERO,
                    }));
                }
                if widgets::icon_button(ui, Icon::Settings, "Recording settings").clicked() {
                    push(AppCommand::Show(crate::app::events::Window::Settings));
                }
                return;
            }
            if recording.uploaded {
                if widgets::icon_button(ui, Icon::History, "Show uploads").clicked() {
                    push(AppCommand::FocusHistory(
                        crate::app::events::HistoryFocus::InProgress,
                    ));
                }
            } else {
                if widgets::primary_button(ui, Some(Icon::Upload), "Upload").clicked() {
                    push(AppCommand::Recording(RecordingInput::Upload));
                }
                if widgets::toggle_button(ui, false, None, "Run tasks")
                    .on_hover_text("Run the selected after-capture tasks")
                    .clicked()
                {
                    push(AppCommand::RunRecordingTasks);
                }
            }
            if widgets::toggle_button(ui, false, None, "Save").clicked() {
                push(AppCommand::Recording(RecordingInput::Save));
            }
            if widgets::ghost_button(ui, None, "Close").clicked() {
                push(AppCommand::CloseRecordingReview);
            }
        });
    });
    if recording.uploaded {
        widgets::hint(ui, "Cancel its job in Uploads before discarding the source.");
    }
    commands
}
