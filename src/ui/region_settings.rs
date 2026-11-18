//! A validated draft: no persistence or session changes until explicit Apply.
use super::{tokens, widgets};
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
fn switch(ui: &mut Ui, label: &str, description: Option<&str>, on: &mut bool) {
    widgets::setting_row(ui, label, description, |ui| {
        widgets::toggle_switch(ui, on, label);
    });
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
        widgets::setting_group(
            ui,
            "Screenshots",
            Some(
                "How region, window and fullscreen selection behaves. Guides and dimming never appear in the capture.",
            ),
            |ui| {
                switch(
                    ui,
                    "Detect windows",
                    Some("Hover a window to select it in one click."),
                    &mut p.window_detection,
                );
                widgets::setting_row(
                    ui,
                    "Dim the background",
                    Some("Darkens the screen outside your selection."),
                    |ui| {
                        ui.add(egui::Slider::new(&mut p.dim_percent, 0..=80).suffix("%"));
                    },
                );
                switch(
                    ui,
                    "Show coordinates",
                    Some("Shown next to the pointer while selecting."),
                    &mut p.show_coordinates,
                );
                switch(ui, "Show dimensions", None, &mut p.show_dimensions);
                switch(
                    ui,
                    "Show RGB",
                    Some("Read only: colors are never copied."),
                    &mut p.show_rgb,
                );
                switch(ui, "Show HEX", None, &mut p.show_hex);
                switch(
                    ui,
                    "Magnifier while selecting",
                    Some("Shows a zoomed view next to the pointer."),
                    &mut p.show_magnifier,
                );
                if p.show_magnifier {
                    widgets::setting_row(ui, "Magnifier shape", None, |ui| {
                        egui::ComboBox::from_id_salt("lens-shape")
                            .selected_text(match p.lens_shape {
                                LensShape::Circle => "Circle",
                                LensShape::Square => "Square",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut p.lens_shape, LensShape::Circle, "Circle");
                                ui.selectable_value(&mut p.lens_shape, LensShape::Square, "Square");
                            });
                    });
                    widgets::setting_row(
                        ui,
                        "Pixels shown",
                        Some("Source pixels per side."),
                        |ui| {
                            let mut count = p.lens_source_count;
                            if ui
                                .add(egui::Slider::new(&mut count, 9..=35).step_by(2.0))
                                .changed()
                            {
                                p.lens_source_count = count | 1;
                            }
                        },
                    );
                    widgets::setting_row(
                        ui,
                        "Zoom",
                        Some("Screen pixels per source pixel."),
                        |ui| {
                            ui.add(egui::Slider::new(&mut p.lens_scale, 4..=24).suffix("×"));
                        },
                    );
                }
                switch(ui, "Center crosshair", None, &mut p.center_crosshair);
                switch(ui, "Screen-wide crosshair", None, &mut p.screen_crosshair);
                switch(
                    ui,
                    "Stay on the starting monitor",
                    Some("Selection can't cross onto another display."),
                    &mut p.active_monitor_only,
                );
            },
        );
        ui.add_space(tokens::GROUP_GAP);
        widgets::setting_group(
            ui,
            "Advanced selection",
            Some("Fixed sizes, Alt-drag presets and extra mouse buttons."),
            |ui| {
                switch(ui, "Fixed-size region", None, &mut p.fixed_size);
                if p.fixed_size {
                    widgets::card_block(ui, |ui| size_controls(ui, &mut p.fixed_dimensions));
                }
                widgets::card_block(ui, |ui| {
                    ui.label(
                        egui::RichText::new("Alt-drag sizes")
                            .font(tokens::medium(tokens::BODY_SIZE))
                            .color(ui.visuals().text_color()),
                    );
                    widgets::hint(ui, "Up to 16 presets.");
                    let mut remove = None;
                    for (index, size) in p.presets.iter_mut().enumerate() {
                        ui.push_id(index, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                size_controls(ui, size);
                                if widgets::ghost_button(ui, None, "Remove").clicked() {
                                    remove = Some(index);
                                }
                            });
                        });
                    }
                    if let Some(index) = remove {
                        p.presets.remove(index);
                    }
                    let add = ui
                        .add_enabled_ui(p.presets.len() < 16, |ui| {
                            widgets::toggle_button(
                                ui,
                                false,
                                Some(super::icons::Icon::Plus),
                                "Add size",
                            )
                            .clicked()
                        })
                        .inner;
                    if add {
                        p.presets.push(SizePreset {
                            width: 640,
                            height: 480,
                        });
                    }
                });
                for (id, label, action) in [
                    ("right", "Right button", &mut p.mouse.right),
                    ("middle", "Middle button", &mut p.mouse.middle),
                    ("button4", "Button 4", &mut p.mouse.button4),
                    ("button5", "Button 5", &mut p.mouse.button5),
                ] {
                    widgets::setting_row(ui, label, None, |ui| {
                        egui::ComboBox::from_id_salt(id)
                            .selected_text(action_label(*action))
                            .show_ui(ui, |ui| {
                                for value in [
                                    CaptureMouseAction::None,
                                    CaptureMouseAction::CancelActiveOrCapture,
                                    CaptureMouseAction::ToggleSelectionDrawing,
                                    CaptureMouseAction::SelectFullscreen,
                                    CaptureMouseAction::SelectActiveMonitor,
                                ] {
                                    ui.selectable_value(action, value, action_label(value));
                                }
                            });
                    });
                }
                widgets::card_block(ui, |ui| {
                    widgets::hint(
                        ui,
                        "Escape always cancels an active edit first. Selecting monitor or fullscreen bounds opens review only.",
                    );
                });
            },
        );
        if let Err(error) = p.validate() {
            self.error = Some(error);
        } else {
            self.error = None;
        }
        ui.add_space(14.0);
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error.to_string());
        }
        if self.revision != settings.revision && settings.region != self.draft {
            widgets::hint(
                ui,
                "Settings changed elsewhere. Reload the saved selection settings before applying.",
            );
        }
        let mut apply = false;
        let mut reload = false;
        ui.horizontal_wrapped(|ui| {
            apply = ui
                .add_enabled_ui(
                    self.error.is_none() && self.revision == settings.revision,
                    |ui| widgets::primary_button(ui, None, "Apply").clicked(),
                )
                .inner;
            reload = widgets::ghost_button(ui, None, "Reload saved").clicked();
        });
        let command = apply.then(|| AppCommand::SetRegionPreferences {
            preferences: self.draft.clone(),
            revision: self.revision,
        });
        if reload {
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
