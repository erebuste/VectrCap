use crate::{
    app::events::AppCommand,
    persistence::settings::{RecordingLimits, Settings},
    recording::{options::*, preferences::RecordingPreferences},
    ui::audio_sources::{self, AudioEndpoint},
};
use eframe::egui::{self, Ui};
pub struct RecordingSettingsPane {
    pub preferences: RecordingPreferences,
    pub limits: RecordingLimits,
    testing: bool,
}
impl RecordingSettingsPane {
    pub fn new(s: &Settings) -> Self {
        Self {
            preferences: s.recording_options.clone(),
            limits: s.recording,
            testing: false,
        }
    }
    pub fn stop_test(&mut self) -> Option<AppCommand> {
        if self.testing {
            self.testing = false;
            Some(AppCommand::StopAudioTest)
        } else {
            None
        }
    }
    pub fn draw(
        &mut self,
        ui: &mut Ui,
        s: &Settings,
        endpoints: &[AudioEndpoint],
        meter: Option<&Result<f32, crate::app::error::AppError>>,
    ) -> Vec<AppCommand> {
        let mut commands = vec![];
        let p = &mut self.preferences;
        ui.label("GIF and MP4 keep their own frame rate and audio preferences. Changes apply to the next Start.");
        ui.checkbox(&mut p.cursor, "Show cursor in recordings");
        ui.checkbox(
            &mut p.confirm_discard,
            "Confirm discard / quit while recording or paused",
        );
        ui.add(egui::Slider::new(&mut p.countdown_seconds, 0..=60).text("Countdown seconds"));
        ui.horizontal(|ui| {
            for seconds in [0, 1, 3, 5] {
                if ui.button(format!("{seconds}s")).clicked() {
                    p.countdown_seconds = seconds;
                }
            }
        });
        egui::ComboBox::from_id_salt("recording-size")
            .selected_text(format!("{:?}", p.output_size))
            .show_ui(ui, |ui| {
                for size in [
                    OutputSize::Original,
                    OutputSize::Fit1280x720,
                    OutputSize::Fit1920x1080,
                ] {
                    ui.selectable_value(&mut p.output_size, size, format!("{size:?}"));
                }
            });
        ui.heading("GIF");
        ui.horizontal(|ui| {
            ui.label("FPS");
            for fps in [10, 15, 20] {
                ui.selectable_value(&mut p.gif.fps, fps, fps.to_string());
            }
        });
        ui.add(
            egui::Slider::new(&mut self.limits.gif_max_seconds, 1..=300)
                .text("GIF active limit (seconds)"),
        );
        timed_stop(
            ui,
            "GIF timed stop",
            &mut p.gif_timed_stop,
            self.limits.gif_max_seconds,
        );
        ui.heading("MP4 · H.264");
        ui.horizontal(|ui| {
            ui.label("FPS");
            for fps in [15, 30, 60] {
                ui.selectable_value(&mut p.mp4.fps, fps, fps.to_string());
            }
        });
        egui::ComboBox::from_id_salt("video-quality")
            .selected_text(format!("{:?}", p.mp4.quality))
            .show_ui(ui, |ui| {
                for quality in [Quality::Small, Quality::Balanced, Quality::High] {
                    ui.selectable_value(&mut p.mp4.quality, quality, format!("{quality:?}"));
                }
            });
        let mut custom = p.mp4.custom_bitrate.is_some();
        if ui
            .checkbox(&mut custom, "Advanced target bitrate")
            .changed()
        {
            p.mp4.custom_bitrate = custom.then_some(8_000_000);
        }
        if let Some(b) = &mut p.mp4.custom_bitrate {
            ui.add(egui::Slider::new(b, 500_000..=100_000_000).text("Video bits/sec"));
        }
        ui.add(
            egui::Slider::new(&mut self.limits.mp4_max_minutes, 1..=60)
                .text("MP4 active limit (minutes)"),
        );
        timed_stop(
            ui,
            "MP4 timed stop",
            &mut p.mp4_timed_stop,
            self.limits.mp4_max_minutes * 60,
        );
        ui.heading("MP4 audio");
        audio_sources::selectors(ui, &mut p.mp4.audio, endpoints);
        if ui.button("Refresh playback and input devices").clicked() {
            commands.push(AppCommand::RefreshAudioEndpoints);
        }
        if self.testing {
            ui.label("Test audio active · no audio is stored");
            if ui.button("End audio test").clicked() {
                self.testing = false;
                commands.push(AppCommand::StopAudioTest);
            }
        } else if ui
            .add_enabled(
                p.mp4.audio.mode != AudioMode::Off,
                egui::Button::new("Test audio"),
            )
            .clicked()
        {
            self.testing = true;
            commands.push(AppCommand::StartAudioTest(p.mp4.audio.clone()));
        }
        if p.mp4.audio.mode == AudioMode::Off && self.testing {
            self.testing = false;
            commands.push(AppCommand::StopAudioTest);
        }
        if self.testing
            && let Some(status) = meter
        {
            match status {
                Ok(peak) => {
                    ui.add(egui::ProgressBar::new(*peak).text("Selected sources level"));
                }
                Err(e) => {
                    ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                }
            }
        }
        match p.validate_limits(self.limits) {
            Ok(()) => {
                if ui.button("Save recording settings").clicked() {
                    self.testing = false;
                    commands.push(AppCommand::StopAudioTest);
                    commands.push(AppCommand::SetRecordingPreferences {
                        preferences: p.clone(),
                        limits: self.limits,
                        revision: s.revision,
                    });
                }
            }
            Err(e) => {
                ui.colored_label(ui.visuals().error_fg_color, e.to_string());
            }
        }
        commands
    }
}
fn timed_stop(ui: &mut Ui, label: &str, value: &mut Option<u32>, max: u32) {
    let mut enabled = value.is_some();
    if ui.checkbox(&mut enabled, label).changed() {
        *value = enabled.then_some(max.min(30));
    }
    if let Some(seconds) = value {
        ui.add(egui::Slider::new(seconds, 1..=max).text("Active seconds"));
    }
}

pub fn selection_summary(
    settings: &Settings,
    mp4: bool,
    rect: Option<crate::capture::geometry::PhysicalRect>,
) -> String {
    let p = &settings.recording_options;
    let mut summary = if mp4 {
        format!(
            "MP4: {} FPS; {:?}; {}s maximum active. {}",
            p.mp4.fps,
            p.mp4.quality,
            settings.recording.mp4_max_minutes * 60,
            audio_sources::source_summary(Some(&p.mp4.audio))
        )
    } else {
        format!(
            "GIF: {} FPS; {}s maximum active; audio Off",
            p.gif.fps, settings.recording.gif_max_seconds
        )
    };
    if let Some(rect) = rect {
        let o = p.options(mp4, "preview".into(), rect, settings.recording);
        if let Ok((w, h)) = o.dimensions() {
            summary.push_str(&format!("; output {w} x {h}"));
            if let FormatOptions::Mp4(m) = o.format {
                summary.push_str(&format!("; H.264 {} bits/sec", m.bitrate(w, h)));
            }
        }
    }
    summary.push_str(&format!(
        "; cursor {}; countdown {}s",
        p.cursor, p.countdown_seconds
    ));
    summary
}
