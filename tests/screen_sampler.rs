use vectrcap::{
    capture::geometry::{MonitorGeometry, PhysicalPoint as Point, PhysicalRect as Rect},
    tools::{
        UtilitySnapshot,
        color_result::{ColorResultInput, ColorResultState, CopyFeedback},
        sampler::{SamplerState, sample_pixel},
        session::{UtilityEffect, UtilityInput, UtilityKind, UtilitySession},
    },
};

fn snapshot() -> UtilitySnapshot {
    let bounds = Rect::new(-20, -10, 40, 30).unwrap();
    let mut pixels = vec![0; bounds.rgba_len().unwrap()];
    for (i, p) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        p.copy_from_slice(&[(i % 40) as u8, (i / 40) as u8, 89, 255]);
    }
    let i = ((15 * 40 + 20) * 4) as usize;
    pixels[i..i + 4].copy_from_slice(&[245, 183, 89, 255]);
    UtilitySnapshot::new(
        pixels,
        bounds,
        vec![
            MonitorGeometry::new("left".into(), Rect::new(-20, -10, 18, 30).unwrap(), 1.0).unwrap(),
            MonitorGeometry::new("right".into(), Rect::new(0, -10, 20, 30).unwrap(), 2.0).unwrap(),
        ],
        7,
    )
    .unwrap()
}
fn point() -> Point {
    Point { x: 0, y: 5 }
}
#[test]
fn center_pixel_matches_magnifier_crosshair() {
    let source = snapshot();
    let lens = SamplerState::new(point()).readout(&source).unwrap();
    assert_eq!(lens.grid_size(), 15);
    assert_eq!(lens.pixel(7, 7), Some([245, 183, 89, 255]));
    assert_eq!(
        lens.center().unwrap()[..3],
        sample_pixel(&source, point()).unwrap().rgb
    );
}
#[test]
fn negative_origin_and_mixed_dpi_sample_exact_pixel() {
    let s = snapshot();
    for (p, expected) in [
        (Point { x: -19, y: -9 }, [1, 1, 89]),
        (Point { x: 1, y: 6 }, [21, 16, 89]),
    ] {
        let color = sample_pixel(&s, p).unwrap();
        assert_eq!(color.point, p);
        assert_eq!(color.rgb, expected);
    }
}
#[test]
fn grid_and_readout_never_enter_source_sample() {
    let s = snapshot();
    let before = s.pixels().to_vec();
    let ctx = eframe::egui::Context::default();
    let mut output = ctx.run_ui(Default::default(), |ui| {
        let ctx = ui.ctx();
        SamplerState::new(point())
            .readout(&s)
            .unwrap()
            .paint(
                ctx,
                eframe::egui::pos2(10.0, 10.0),
                eframe::egui::Rect::from_min_size(
                    eframe::egui::Pos2::ZERO,
                    eframe::egui::vec2(800.0, 600.0),
                ),
                &SamplerState::preferences(),
            )
            .unwrap();
    });
    output.textures_delta.clear();
    assert_eq!(s.pixels(), before);
    assert_eq!(sample_pixel(&s, point()).unwrap().rgb, [245, 183, 89]);
}
#[test]
fn monitor_gap_has_no_color() {
    assert!(sample_pixel(&snapshot(), Point { x: -1, y: 0 }).is_err());
}
#[test]
fn rgb245_183_89_formats_as_f5b759() {
    let c = sample_pixel(&snapshot(), point()).unwrap();
    assert_eq!(c.hex_text(), "#F5B759");
    assert_eq!(c.rgb_text(), "245, 183, 89");
}
#[test]
fn selecting_color_opens_result_without_copy() {
    let mut s = UtilitySession::new(UtilityKind::Sampler, snapshot(), point());
    let effects = s.handle(UtilityInput::Select);
    assert!(matches!(
        effects.as_slice(),
        [UtilityEffect::ShowColorResult(_)]
    ));
    assert!(s.snapshot().is_none());
}
#[test]
fn copy_buttons_copy_exact_displayed_values() {
    let mut result = ColorResultState::new(sample_pixel(&snapshot(), point()).unwrap());
    assert_eq!(
        result.handle(ColorResultInput::CopyHex),
        vec![UtilityEffect::CopyText("#F5B759".into())]
    );
    result.handle(ColorResultInput::ClipboardOutcome(Ok(())));
    assert_eq!(result.feedback(), &CopyFeedback::Copied);
    assert_eq!(
        result.handle(ColorResultInput::CopyRgb),
        vec![UtilityEffect::CopyText("245, 183, 89".into())]
    );
}
#[test]
fn copy_failure_keeps_sample_for_retry() {
    let c = sample_pixel(&snapshot(), point()).unwrap();
    let mut result = ColorResultState::new(c);
    result.handle(ColorResultInput::CopyHex);
    result.handle(ColorResultInput::ClipboardOutcome(Err(
        vectrcap::app::error::AppError::Persistence("clipboard busy"),
    )));
    assert!(matches!(result.feedback(), CopyFeedback::Error(_)));
    assert_eq!(result.color(), c);
    assert_eq!(
        result.handle(ColorResultInput::CopyHex),
        vec![UtilityEffect::CopyText(c.hex_text())]
    );
}
#[test]
fn pick_again_refreshes_source() {
    let mut result = ColorResultState::new(sample_pixel(&snapshot(), point()).unwrap());
    assert_eq!(
        result.handle(ColorResultInput::PickAgain),
        vec![UtilityEffect::RefreshSource]
    );
}

#[test]
fn default_lens_is_300_physical_display_pixels_at_every_ui_scale() {
    use eframe::egui::{Pos2, Rect, pos2, vec2};
    for scale in [1.0, 1.5, 2.0, 4.0] {
        let layout = vectrcap::ui::magnifier::MagnifierLayout::fit_physical(
            pos2(10.0, 10.0),
            Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 1000.0)),
            15,
            20,
            scale,
        )
        .unwrap();
        assert_eq!(layout.rect.width() * scale, 300.0);
    }
}
