use crate::pin::{PinInput, PinOptions};
use eframe::egui;

pub fn draw(ui: &mut egui::Ui, options: &PinOptions) -> Option<PinInput> {
    let mut value = options.clone();
    ui.checkbox(&mut value.always_on_top, "Always on top");
    ui.checkbox(&mut value.smooth, "Smooth scaling (off: nearest neighbor)");
    let mut width = value.border_width();
    ui.add(egui::Slider::new(&mut width, 0..=16).text("Border width (physical px)"));
    // Slider is bounded by the same validated constructor.
    if value.set_border_width(width).is_err() {
        return None;
    }
    ui.label("Border color");
    ui.color_edit_button_srgba_unmultiplied(&mut value.border_color);
    let mut checkerboard = value.background.is_none();
    if ui
        .checkbox(&mut checkerboard, "Checkerboard behind transparent pixels")
        .changed()
    {
        value.background = if checkerboard {
            None
        } else {
            Some([64, 64, 64, 255])
        };
    }
    if let Some(color) = &mut value.background {
        ui.label("Background color");
        ui.color_edit_button_srgba_unmultiplied(color);
    }
    (value != *options).then_some(PinInput::Options(value))
}
