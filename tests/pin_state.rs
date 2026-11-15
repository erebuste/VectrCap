use image::RgbaImage;
use std::sync::Arc;
use vectrcap::{
    capture::geometry::{PhysicalPoint, PhysicalRect},
    pin::{PinEffect, PinInput, PinManager, PinnedImage},
};

fn image(w: u32, h: u32) -> PinnedImage {
    PinnedImage::new(Arc::new(RgbaImage::new(w, h)), None).unwrap()
}
fn manager() -> PinManager {
    let mut manager = PinManager::default();
    manager
        .update_work_areas(
            vec![PhysicalRect::new(-100, -100, 1920, 1080).unwrap()],
            PhysicalPoint { x: 0, y: 0 },
        )
        .unwrap();
    manager
}
#[test]
fn pins_keep_independent_display_state() {
    let mut m = manager();
    let a = m
        .open(image(200, 100), Some(PhysicalPoint { x: 20, y: 30 }))
        .unwrap();
    let b = m.open(image(100, 200), None).unwrap();
    m.handle(a, PinInput::Zoom(1)).unwrap();
    m.handle(a, PinInput::Opacity(-1)).unwrap();
    m.handle(a, PinInput::Move { dx: 1, dy: -10 }).unwrap();
    let s = m.get(a).unwrap();
    assert_eq!(
        (s.state.zoom_percent(), s.state.opacity_percent()),
        (110, 95)
    );
    assert_eq!(s.state.position(), PhysicalPoint { x: 21, y: 20 });
    assert_eq!(
        (
            m.get(b).unwrap().state.zoom_percent(),
            m.get(b).unwrap().state.opacity_percent()
        ),
        (100, 100)
    );
    m.handle(a, PinInput::Reset).unwrap();
    assert_eq!(
        (
            m.get(a).unwrap().state.zoom_percent(),
            m.get(a).unwrap().state.opacity_percent()
        ),
        (100, 100)
    );
}
#[test]
fn zoom_and_opacity_clamp_to_spec_bounds() {
    let mut m = manager();
    let id = m.open(image(40, 20), None).unwrap();
    for _ in 0..200 {
        m.handle(id, PinInput::Zoom(-1)).unwrap();
        m.handle(id, PinInput::Opacity(-1)).unwrap();
    }
    assert_eq!(
        (
            m.get(id).unwrap().state.zoom_percent(),
            m.get(id).unwrap().state.opacity_percent()
        ),
        (20, 10)
    );
    for _ in 0..200 {
        m.handle(id, PinInput::Zoom(1)).unwrap();
        m.handle(id, PinInput::Opacity(1)).unwrap();
    }
    assert_eq!(
        (
            m.get(id).unwrap().state.zoom_percent(),
            m.get(id).unwrap().state.opacity_percent()
        ),
        (500, 100)
    );
    assert_eq!(m.get(id).unwrap().display_size(), (200, 100));
}
#[test]
fn copy_uses_full_resolution_final_pixels() {
    let mut m = manager();
    let source = image(320, 160);
    let pixels = source.pixels().clone();
    let id = m.open(source, None).unwrap();
    m.handle(id, PinInput::Zoom(20)).unwrap();
    m.handle(id, PinInput::Opacity(-10)).unwrap();
    m.handle(id, PinInput::ToggleMinimized).unwrap();
    let effects = m.handle(id, PinInput::Copy).unwrap();
    let [PinEffect::CopyImage(actual)] = effects.as_slice() else {
        panic!("only clipboard effect expected")
    };
    assert!(Arc::ptr_eq(actual, &pixels));
}
#[test]
fn close_never_deletes_or_uploads() {
    let mut m = manager();
    let source = image(20, 10);
    let pixels = source.pixels().clone();
    let id = m.open(source, None).unwrap();
    assert!(
        matches!(m.handle(id, PinInput::Close).unwrap().as_slice(), [PinEffect::CloseViewport(closed)] if *closed == id)
    );
    assert!(m.get(id).is_none());
    assert_eq!(Arc::strong_count(&pixels), 1);
    assert!(m.close_all().is_empty());
}
#[test]
fn preview_restores_expanded_size() {
    let mut m = manager();
    let id = m.open(image(400, 200), None).unwrap();
    m.handle(id, PinInput::Zoom(5)).unwrap();
    assert_eq!(m.get(id).unwrap().display_size(), (600, 300));
    m.handle(id, PinInput::ToggleMinimized).unwrap();
    assert_eq!(m.get(id).unwrap().display_size(), (96, 48));
    m.handle(id, PinInput::ToggleMinimized).unwrap();
    assert_eq!(m.get(id).unwrap().display_size(), (600, 300));
}
#[test]
fn monitor_removal_repositions_pin() {
    let mut m = manager();
    let id = m
        .open(image(200, 100), Some(PhysicalPoint { x: 1000, y: 500 }))
        .unwrap();
    let remaining = PhysicalRect::new(-1200, 0, 1000, 800).unwrap();
    m.update_work_areas(vec![remaining], PhysicalPoint { x: -500, y: 100 })
        .unwrap();
    assert!(remaining.contains(m.get(id).unwrap().state.position()));
    assert_eq!(m.get(id).unwrap().image.pixels().dimensions(), (200, 100));
    assert!(
        m.update_work_areas(vec![], PhysicalPoint { x: 0, y: 0 })
            .is_err()
    );
}
#[test]
fn pin_limits_refuse_without_evicting() {
    let mut m = manager();
    let ids: Vec<_> = (0..16)
        .map(|_| m.open(image(1, 1), None).unwrap())
        .collect();
    assert!(m.open(image(1, 1), None).is_err());
    assert!(ids.iter().all(|id| m.get(*id).is_some()));
    assert_eq!(m.close_all().len(), 16);
    // Reuse immutable sources to exercise the exact aggregate boundary without
    // allocating three separate 256-MB buffers.
    let big = image(8000, 8000);
    let a = m.open(big.clone(), None).unwrap();
    let b = m.open(big.clone(), None).unwrap();
    assert!(m.open(image(1, 1), None).is_err());
    m.handle(a, PinInput::Close).unwrap();
    assert!(m.get(b).is_some());
    assert!(m.open(big, None).is_ok());
    assert!(PinnedImage::new(Arc::new(RgbaImage::new(0, 1)), None).is_err());
}
