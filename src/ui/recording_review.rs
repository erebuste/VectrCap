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
    format!(
        "{} FPS | {} x {} | {:.2}s active | {} bytes | audio {:?}",
        m.fps,
        m.width,
        m.height,
        m.active_millis as f64 / 1000.0,
        m.size,
        m.audio
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
                    [560.0, 230.0]
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
                    commands.extend(draw_review(ui, &recording, snapshot.error.as_ref()));
                    if let Ok(mut player) = state.try_lock() {
                        player.draw(ui);
                    }
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
            ui.add(egui::Image::new(texture).max_size(egui::vec2(640.0, 360.0)));
        }
        if let Some(error) = &self.error {
            ui.label(error.to_string());
        }
    }
}
fn draw_review(
    ui: &mut egui::Ui,
    recording: &RecordingView,
    action_error: Option<&AppError>,
) -> Vec<AppCommand> {
    let mut commands = Vec::new();
    ui.heading(if recording.mp4 {
        "MP4 recording review"
    } else {
        "GIF recording review"
    });
    ui.label(&recording.task_target);
    if let Some(metadata) = &recording.metadata {
        ui.label(metadata_label(metadata));
    }
    if let Some(reason) = recording.interruption {
        ui.colored_label(ui.visuals().warn_fg_color, reason.label());
        ui.label("Interrupted recording: review the valid portion before choosing Upload.");
    }
    for error in [recording.error.as_ref(), action_error]
        .into_iter()
        .flatten()
    {
        ui.colored_label(ui.visuals().error_fg_color, error.to_string());
    }
    if let Some(path) = &recording.archive_path {
        ui.label("Saved archive");
        ui.label(path.to_string_lossy());
    }
    if recording.mp4
        && recording.playback_path.is_some()
        && ui.button("Open MP4 in associated player").clicked()
    {
        commands.push(AppCommand::OpenRecordingPlayer);
    }
    if recording.archive_failed {
        if ui.button("Retry save").clicked() {
            commands.push(AppCommand::RetryRecordingArchive {
                continue_without: false,
            });
        }
        if ui.button("Choose archive folder").clicked() {
            commands.push(AppCommand::ChooseArchiveFolder);
        }
        if ui.button("Continue without local copy").clicked() {
            commands.push(AppCommand::RetryRecordingArchive {
                continue_without: true,
            });
        }
    }
    if recording.state == RecordingState::Review {
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(!recording.uploaded, egui::Button::new("Upload"))
                .clicked()
            {
                commands.push(AppCommand::Recording(RecordingInput::Upload));
            }
            if ui
                .add_enabled(!recording.uploaded, egui::Button::new("Run selected tasks"))
                .clicked()
            {
                commands.push(AppCommand::RunRecordingTasks);
            }
            if ui.button("Save").clicked() {
                commands.push(AppCommand::Recording(RecordingInput::Save));
            }
            if ui.button("Discard").clicked() {
                commands.push(AppCommand::Recording(RecordingInput::Discard));
            }
            if ui.button("Done").clicked() {
                commands.push(AppCommand::CloseRecordingReview);
            }
        });
        if recording.uploaded {
            ui.label(
                "Upload queued. Cancel its job in Uploads before discarding an active source.",
            );
            if ui.button("Show uploads").clicked() {
                commands.push(AppCommand::FocusHistory(
                    crate::app::events::HistoryFocus::InProgress,
                ));
            }
        }
    }
    if recording.state == RecordingState::Failed {
        ui.label("Correct recording/audio settings and select the area again, or explicitly start without audio.");
        if ui.button("Recording settings").clicked() {
            commands.push(AppCommand::Show(crate::app::events::Window::Settings));
        }
        if recording.mp4 && ui.button("Start without audio").clicked() {
            commands.push(AppCommand::Recording(RecordingInput::StartWithoutAudio {
                now: Duration::ZERO,
            }));
        }
        if ui.button("Discard failed session").clicked() {
            commands.push(AppCommand::Recording(RecordingInput::Discard));
        }
    }
    commands
}
