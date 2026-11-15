//! A validated draft: no persistence or session changes until explicit Apply.
use crate::{
    app::{error::AppError, events::AppCommand},
    capture::preferences::{CaptureMouseAction, LensShape, RegionCapturePreferences, SizePreset},
    persistence::settings::Settings,
};
use eframe::egui::{self, Ui};

pub struct RegionSettingsPane {
    draft: RegionCapturePreferences,
    original: RegionCapturePreferences,
    revision: u64,
    error: Option<AppError>,
}
impl RegionSettingsPane {
    pub fn new(settings: &Settings) -> Self {
        let mut draft = settings.region.clone();
        // Older files had one switch. Preserve their disabled presentation when
        // expanding to the two independent field checkboxes.
        draft.show_coordinates &= draft.show_position_size;
        draft.show_dimensions &= draft.show_position_size;
        draft.show_position_size = true;
        Self {
            draft,
            original: settings.region.clone(),
            revision: settings.revision,
            error: None,
        }
    }
    pub fn draw(&mut self, ui: &mut Ui, settings: &Settings) -> Option<AppCommand> {
        if settings.region == self.draft && settings.region != self.original {
            *self = Self::new(settings);
        }
        let p = &mut self.draft;
        ui.checkbox(&mut p.window_detection, "Detect window regions");
        ui.add(egui::Slider::new(&mut p.dim_percent, 0..=80).text("Background dim %"));
        ui.label("Information near pointer (read only)");
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut p.show_coordinates, "Coordinates");
            ui.checkbox(&mut p.show_dimensions, "Dimensions");
            ui.checkbox(&mut p.show_rgb, "RGB");
            ui.checkbox(&mut p.show_hex, "HEX");
        });
        ui.checkbox(&mut p.show_magnifier, "Show selection magnifier");
        ui.add_enabled_ui(p.show_magnifier, |ui| {
            egui::ComboBox::from_id_salt("lens-shape")
                .selected_text(match p.lens_shape {
                    LensShape::Circle => "Circle",
                    LensShape::Square => "Square",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut p.lens_shape, LensShape::Circle, "Circle");
                    ui.selectable_value(&mut p.lens_shape, LensShape::Square, "Square");
                });
            let mut count = p.lens_source_count;
            if ui
                .add(
                    egui::Slider::new(&mut count, 9..=35)
                        .step_by(2.0)
                        .text("Source pixels per side"),
                )
                .changed()
            {
                p.lens_source_count = count | 1;
            }
            ui.add(
                egui::Slider::new(&mut p.lens_scale, 4..=24)
                    .text("Display pixels per source pixel"),
            );
        });
        ui.checkbox(&mut p.center_crosshair, "Center crosshair");
        ui.checkbox(&mut p.screen_crosshair, "Screen-wide crosshair");
        ui.checkbox(
            &mut p.active_monitor_only,
            "Restrict selection to launch monitor",
        );
        ui.label("Color fields never copy colors or open a dialog. Guides and dimming are excluded from output.");
        ui.collapsing("Advanced", |ui| {
            ui.checkbox(&mut p.fixed_size, "Fixed-size region");
            ui.add_enabled_ui(p.fixed_size, |ui| size_controls(ui, &mut p.fixed_dimensions));
            ui.label("Alt-drag sizes (up to 16 presets)");
            let mut remove = None;
            for (index, size) in p.presets.iter_mut().enumerate() {
                ui.push_id(index, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        size_controls(ui, size);
                        if ui.button("Remove size").clicked() { remove = Some(index); }
                    });
                });
            }
            if let Some(index) = remove { p.presets.remove(index); }
            if ui.add_enabled(p.presets.len() < 16, egui::Button::new("Add size preset")).clicked() {
                p.presets.push(SizePreset { width: 640, height: 480 });
            }
            ui.label("Capture mouse assignments");
            for (id, label, action) in [
                ("right", "Right button", &mut p.mouse.right),
                ("middle", "Middle button", &mut p.mouse.middle),
                ("button4", "Button 4", &mut p.mouse.button4),
                ("button5", "Button 5", &mut p.mouse.button5),
            ] {
                ui.horizontal_wrapped(|ui| {
                    ui.label(label);
                    egui::ComboBox::from_id_salt(id).selected_text(action_label(*action)).show_ui(ui, |ui| {
                        for value in [CaptureMouseAction::None, CaptureMouseAction::CancelActiveOrCapture, CaptureMouseAction::ToggleSelectionDrawing, CaptureMouseAction::SelectFullscreen, CaptureMouseAction::SelectActiveMonitor] {
                            ui.selectable_value(action, value, action_label(value));
                        }
                    });
                });
            }
            ui.label("Escape always cancels an active edit first. Selecting monitor/fullscreen bounds opens review only.");
        });
        if let Err(error) = p.validate() {
            self.error = Some(error);
        } else {
            self.error = None;
        }
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error.to_string());
        }
        if self.revision != settings.revision && settings.region != self.draft {
            ui.label("Settings changed. Reload saved region settings before applying.");
        }
        let apply = ui
            .add_enabled(
                self.error.is_none() && self.revision == settings.revision,
                egui::Button::new("Apply region settings"),
            )
            .clicked();
        let command = apply.then(|| AppCommand::SetRegionPreferences {
            preferences: self.draft.clone(),
            revision: self.revision,
        });
        if ui.button("Reload saved region settings").clicked() {
            *self = Self::new(settings);
        }
        command
    }
}
fn size_controls(ui: &mut Ui, size: &mut SizePreset) {
    ui.horizontal_wrapped(|ui| {
        ui.label("Width");
        ui.add(
            egui::DragValue::new(&mut size.width)
                .range(1..=64_000_000)
                .suffix(" px"),
        );
        ui.label("Height");
        ui.add(
            egui::DragValue::new(&mut size.height)
                .range(1..=64_000_000)
                .suffix(" px"),
        );
    });
}
fn action_label(action: CaptureMouseAction) -> &'static str {
    match action {
        CaptureMouseAction::None => "Unassigned",
        CaptureMouseAction::CancelActiveOrCapture => "Cancel edit or capture",
        CaptureMouseAction::ToggleSelectionDrawing => "Toggle region / drawing",
        CaptureMouseAction::SelectFullscreen => "Select fullscreen for review",
        CaptureMouseAction::SelectActiveMonitor => "Select launch monitor for review",
    }
}
