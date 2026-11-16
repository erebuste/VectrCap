use crate::tools::{
    color_result::{ColorResultInput, ColorResultState, CopyFeedback},
    session::UtilityInput,
};
use eframe::egui::{self, Color32};
pub fn show(ui: &mut egui::Ui, state: &ColorResultState) -> Option<UtilityInput> {
    let color = state.color();
    let (swatch, _) = ui.allocate_exact_size(egui::vec2(64.0, 48.0), egui::Sense::hover());
    ui.painter().rect_filled(
        swatch,
        4,
        Color32::from_rgb(color.rgb[0], color.rgb[1], color.rgb[2]),
    );
    ui.label(format!(
        "Physical X: {}  Y: {}",
        color.point.x, color.point.y
    ));
    let mut input = None;
    for (label, mut text, copy) in [
        ("HEX", color.hex_text(), ColorResultInput::CopyHex),
        ("RGB", color.rgb_text(), ColorResultInput::CopyRgb),
    ] {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.add(
                egui::TextEdit::singleline(&mut text)
                    .interactive(false)
                    .desired_width(160.0),
            );
            if ui.button(format!("Copy {label}")).clicked() {
                input = Some(UtilityInput::Result(copy));
            }
        });
    }
    feedback(ui, state.feedback());
    ui.horizontal(|ui| {
        if ui.button("Pick again").clicked() {
            input = Some(UtilityInput::Result(ColorResultInput::PickAgain));
        }
        if ui.button("Close").clicked() {
            input = Some(UtilityInput::Close);
        }
    });
    input
}
pub(crate) fn feedback(ui: &mut egui::Ui, state: &CopyFeedback) {
    match state {
        CopyFeedback::None => {}
        CopyFeedback::Copying => {
            ui.label("Copying…");
        }
        CopyFeedback::Copied => {
            ui.label("Copied");
        }
        CopyFeedback::Error(e) => {
            ui.colored_label(
                egui::Color32::LIGHT_RED,
                format!("Copy failed. Try again: {e}"),
            );
        }
    }
}
