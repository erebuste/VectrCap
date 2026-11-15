use std::{
    sync::{Arc, Condvar, Mutex},
    time::Duration as D,
};
use vectrcap::{
    app::error::AppError,
    capture::geometry::PhysicalRect,
    recording::{frame::CapturedFrame, options::*, worker::FrameQueue, *},
};
fn frame(t: u64) -> CapturedFrame {
    CapturedFrame::new(vec![120; 16 * 16 * 4], 16, 16, D::from_millis(t)).unwrap()
}
fn opts() -> RecordingOptions {
    let mut o = RecordingOptions::new(
        "test".into(),
        PhysicalRect::new(0, 0, 16, 16).unwrap(),
        false,
    );
    o.countdown_seconds = 0;
    o
}
#[test]
fn crop_uses_nonzero_origin() {
    let mut src = vec![255; 8 * 6 * 4];
    for y in 2..5 {
        for x in 3..6 {
            src[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4].copy_from_slice(&[10, 20, 30, 255]);
        }
    }
    let f = CapturedFrame::crop(
        &src,
        (8, 6),
        32,
        PhysicalRect::new(3, 2, 3, 3).unwrap(),
        OutputSize::Original,
        false,
        D::ZERO,
    )
    .unwrap();
    assert!(
        f.rgba()
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [10, 20, 30, 255])
    );
}
#[test]
fn padding_never_reads_neighbor_pixels() {
    let f = CapturedFrame::crop(
        &[255; 8 * 6 * 4],
        (8, 6),
        32,
        PhysicalRect::new(3, 2, 3, 3).unwrap(),
        OutputSize::Original,
        true,
        D::ZERO,
    )
    .unwrap();
    assert_eq!((f.width(), f.height()), (4, 4));
    for y in 0..4 {
        for x in 0..4 {
            let p = &f.rgba()[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
            assert_eq!(
                p,
                if x == 3 || y == 3 {
                    &[0, 0, 0, 255]
                } else {
                    &[255, 255, 255, 255]
                }
            );
        }
    }
}
#[test]
fn stationary_frames_preserve_duration() {
    let root = tempfile::tempdir().unwrap();
    let mut enc = gif::GifEncoder::new(root.path(), opts()).unwrap();
    enc.push_video(&frame(0)).unwrap();
    enc.end_at(D::from_secs(5)).unwrap();
    let out = Box::new(enc).finish().unwrap();
    let mut decoder = ::gif::DecodeOptions::new();
    decoder.set_color_output(::gif::ColorOutput::RGBA);
    let mut r = decoder
        .read_info(std::fs::File::open(&out.path).unwrap())
        .unwrap();
    let mut ticks = 0;
    while let Some(f) = r.read_next_frame().unwrap() {
        ticks += u32::from(f.delay);
    }
    assert_eq!(ticks, 500);
}
#[test]
fn slow_encoder_bounds_pending_frames() {
    let q = Arc::new(Mutex::new(FrameQueue::default()));
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let qc = q.clone();
    let gc = gate.clone();
    let t = std::thread::spawn(move || {
        let (lock, cv) = &*gc;
        let _guard = cv.wait_while(lock.lock().unwrap(), |b| !*b).unwrap();
        qc.lock().unwrap().pop()
    });
    for i in 0..100 {
        q.lock().unwrap().push(frame(i));
        assert!(q.lock().unwrap().len() <= 3);
    }
    assert_eq!(q.lock().unwrap().dropped(), 97);
    *gate.0.lock().unwrap() = true;
    gate.1.notify_one();
    t.join().unwrap();
}
#[test]
fn cancel_never_publishes_partial() {
    let root = tempfile::tempdir().unwrap();
    {
        let mut e = gif::GifEncoder::new(root.path(), opts()).unwrap();
        e.push_video(&frame(0)).unwrap();
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
#[test]
fn finish_failure_never_returns_success() {
    let root = tempfile::tempdir().unwrap();
    let e = gif::GifEncoder::new(root.path(), opts()).unwrap();
    assert!(matches!(Box::new(e).finish(), Err(AppError::Encoder(_))));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
