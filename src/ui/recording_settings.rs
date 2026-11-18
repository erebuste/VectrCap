use super::{tokens, widgets};
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
fn switch(ui: &mut Ui, label: &str, description: Option<&str>, on: &mut bool) {
    widgets::setting_row(ui, label, description, |ui| {
        widgets::toggle_switch(ui, on, label);
    });
}
/// Frame-rate choices as chips (the muted toggled look).
fn fps_row(ui: &mut Ui, fps: &mut u32, choices: [u32; 3]) {
    widgets::setting_row(
        ui,
        "Frame rate",
        Some("Higher is smoother and makes larger files."),
        |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            // Right-to-left column: add the choices in reverse to read low to high.
            for value in choices.into_iter().rev() {
                if widgets::chip(ui, *fps == value, &format!("{value} fps")).clicked() {
                    *fps = value;
                }
            }
        },
    );
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
        let limits = &mut self.limits;
        widgets::setting_group(
            ui,
            "Recordings",
            Some(
                "GIF and MP4 keep their own settings. Changes apply the next time you start recording.",
            ),
            |ui| {
                switch(ui, "Show the mouse pointer", None, &mut p.cursor);
                switch(
                    ui,
                    "Ask before discarding",
                    Some("Confirms discard or quit while recording or paused."),
                    &mut p.confirm_discard,
                );
                widgets::setting_row(
                    ui,
                    "Countdown",
                    Some("Seconds before recording starts."),
                    |ui| {
                        ui.add(egui::Slider::new(&mut p.countdown_seconds, 0..=60).suffix(" s"));
                    },
                );
                widgets::setting_row(ui, "Output size", None, |ui| {
                    let label = |size: OutputSize| match size {
                        OutputSize::Original => "Original size",
                        OutputSize::Fit1280x720 => "Fit 1280 × 720",
                        OutputSize::Fit1920x1080 => "Fit 1920 × 1080",
                    };
                    egui::ComboBox::from_id_salt("recording-size")
                        .selected_text(label(p.output_size))
                        .show_ui(ui, |ui| {
                            for size in [
                                OutputSize::Original,
                                OutputSize::Fit1280x720,
                                OutputSize::Fit1920x1080,
                            ] {
                                ui.selectable_value(&mut p.output_size, size, label(size));
                            }
                        });
                });
            },
        );
        ui.add_space(tokens::GROUP_GAP);
        widgets::setting_group(ui, "GIF", Some("GIFs never include sound."), |ui| {
            fps_row(ui, &mut p.gif.fps, [10, 15, 20]);
            widgets::setting_row(
                ui,
                "Longest GIF",
                Some("Stops at this much active time."),
                |ui| {
                    ui.add(egui::Slider::new(&mut limits.gif_max_seconds, 1..=300).suffix(" s"));
                },
            );
            timed_stop(ui, "gif", &mut p.gif_timed_stop, limits.gif_max_seconds);
        });
        ui.add_space(tokens::GROUP_GAP);
        widgets::setting_group(ui, "MP4 · H.264", None, |ui| {
            fps_row(ui, &mut p.mp4.fps, [15, 30, 60]);
            widgets::setting_row(ui, "Quality", None, |ui| {
                egui::ComboBox::from_id_salt("video-quality")
                    .selected_text(format!("{:?}", p.mp4.quality))
                    .show_ui(ui, |ui| {
                        for quality in [Quality::Small, Quality::Balanced, Quality::High] {
                            ui.selectable_value(
                                &mut p.mp4.quality,
                                quality,
                                format!("{quality:?}"),
                            );
                        }
                    });
            });
            let mut custom = p.mp4.custom_bitrate.is_some();
            let label = "Custom bitrate";
            widgets::setting_row(ui, label, Some("Overrides the quality setting."), |ui| {
                if widgets::toggle_switch(ui, &mut custom, label).changed() {
                    p.mp4.custom_bitrate = custom.then_some(8_000_000);
                }
            });
            if let Some(bitrate) = &mut p.mp4.custom_bitrate {
                widgets::setting_row(ui, "Bitrate", Some("Video bits per second."), |ui| {
                    ui.add(egui::Slider::new(bitrate, 500_000..=100_000_000));
                });
            }
            widgets::setting_row(
                ui,
                "Longest MP4",
                Some("Stops at this much active time."),
                |ui| {
                    ui.add(egui::Slider::new(&mut limits.mp4_max_minutes, 1..=60).suffix(" min"));
                },
            );
            timed_stop(
                ui,
                "mp4",
                &mut p.mp4_timed_stop,
                limits.mp4_max_minutes * 60,
            );
        });
        ui.add_space(tokens::GROUP_GAP);
        widgets::setting_group(ui, "MP4 audio", None, |ui| {
            widgets::card_block(ui, |ui| {
                audio_sources::selectors(ui, &mut p.mp4.audio, endpoints);
            });
            if self.testing
                && let Some(status) = meter
            {
                widgets::card_block(ui, |ui| match status {
                    Ok(peak) => {
                        widgets::hint(ui, "Testing: nothing is recorded or stored.");
                        ui.add(egui::ProgressBar::new(*peak).text("Selected sources level"));
                    }
                    Err(e) => {
                        ui.colored_label(ui.visuals().error_fg_color, e.to_string());
                    }
                });
            }
            widgets::card_footer(ui, |ui| {
                if widgets::ghost_button(ui, None, "Refresh devices").clicked() {
                    commands.push(AppCommand::RefreshAudioEndpoints);
                }
                if self.testing {
                    if widgets::toggle_button(ui, false, None, "End audio test").clicked() {
                        self.testing = false;
                        commands.push(AppCommand::StopAudioTest);
                    }
                } else {
                    let start = ui
                        .add_enabled_ui(p.mp4.audio.mode != AudioMode::Off, |ui| {
                            widgets::toggle_button(ui, false, None, "Test audio").clicked()
                        })
                        .inner;
                    if start {
                        self.testing = true;
                        commands.push(AppCommand::StartAudioTest(p.mp4.audio.clone()));
                    }
                }
            });
        });
        if p.mp4.audio.mode == AudioMode::Off && self.testing {
            self.testing = false;
            commands.push(AppCommand::StopAudioTest);
        }
        ui.add_space(14.0);
        match p.validate_limits(*limits) {
            Ok(()) => {
                if widgets::primary_button(ui, None, "Save recording settings").clicked() {
                    self.testing = false;
                    commands.push(AppCommand::StopAudioTest);
                    commands.push(AppCommand::SetRecordingPreferences {
                        preferences: p.clone(),
                        limits: *limits,
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
/// "Stop automatically" switch plus, when on, its length.
fn timed_stop(ui: &mut Ui, id: &str, value: &mut Option<u32>, max: u32) {
    ui.push_id(id, |ui| {
        let mut enabled = value.is_some();
        let label = "Stop automatically";
        widgets::setting_row(
            ui,
            label,
            Some("Ends the recording after a set active time."),
            |ui| {
                if widgets::toggle_switch(ui, &mut enabled, label).changed() {
                    *value = enabled.then_some(max.min(30));
                }
            },
        );
        if let Some(seconds) = value {
            widgets::setting_row(ui, "Stop after", None, |ui| {
                ui.add(egui::Slider::new(seconds, 1..=max).suffix(" s"));
            });
        }
    });
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
