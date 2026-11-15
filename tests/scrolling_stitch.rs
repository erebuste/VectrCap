use image::{Rgba, RgbaImage, imageops::crop_imm};
use std::time::Duration;
use vectrcap::scrolling::{overlap::*, stable::StableFrames, stitch::Stitcher};
fn pattern(w: u32, h: u32) -> RgbaImage {
    RgbaImage::from_fn(w, h, |x, y| {
        let n = (y.wrapping_mul(0x9e3779b9) ^ x.wrapping_mul(0x85ebca6b)).wrapping_mul(0xc2b2ae35);
        Rgba([n as u8, (n >> 8) as u8, (n >> 16) as u8, 255])
    })
}
fn view(i: &RgbaImage, y: u32) -> RgbaImage {
    crop_imm(i, 0, y, i.width(), 40).to_image()
}
#[test]
fn known_overlap_has_no_duplicate_or_missing_rows() {
    for w in [3, 17, 101] {
        let original = pattern(w, 100);
        let mut s = Stitcher::new(view(&original, 0), 64000000).unwrap();
        let mut previous = view(&original, 0);
        for y in [13, 26, 39, 52] {
            let next = view(&original, y);
            assert_eq!(
                match_vertical_overlap(&previous, &next).unwrap(),
                OverlapResult::Matched { new_rows: 13 }
            );
            s.append(&next, 13).unwrap();
            previous = next;
        }
        assert_eq!(
            s.finish().unwrap(),
            crop_imm(&original, 0, 0, w, 92).to_image()
        );
    }
}
#[test]
fn identical_frame_does_not_append() {
    let i = pattern(17, 40);
    assert_eq!(
        match_vertical_overlap(&i, &i).unwrap(),
        OverlapResult::Identical
    );
    let mut s = Stitcher::new(i.clone(), 64000000).unwrap();
    assert!(s.append(&i, 0).is_err());
    assert_eq!(s.finish().unwrap(), i);
}
#[test]
fn repeated_patterns_are_ambiguous() {
    for i in [
        RgbaImage::from_pixel(17, 100, Rgba([255; 4])),
        RgbaImage::from_fn(17, 100, |x, y| Rgba([x as u8, (y % 4) as u8, 0, 255])),
    ] {
        assert_eq!(
            match_vertical_overlap(&view(&i, 0), &view(&i, 1)).unwrap(),
            OverlapResult::Ambiguous
        );
    }
}
#[test]
fn insufficient_overlap_pauses_before_append() {
    let i = pattern(17, 120);
    for y in [39, 60] {
        assert_eq!(
            match_vertical_overlap(&view(&i, 0), &view(&i, y)).unwrap(),
            OverlapResult::NoOverlap
        );
    }
}
#[test]
fn changed_viewport_width_is_rejected() {
    assert!(match_vertical_overlap(&pattern(17, 40), &pattern(18, 40)).is_err());
    let mut s = Stitcher::new(pattern(17, 40), 64000000).unwrap();
    assert!(s.append(&pattern(18, 40), 10).is_err());
    assert!(s.append(&pattern(17, 40), 41).is_err());
    assert_eq!(s.finish().unwrap(), pattern(17, 40));
}
#[test]
fn reverse_scroll_does_not_corrupt_prefix() {
    let i = pattern(17, 100);
    assert_eq!(
        match_vertical_overlap(&view(&i, 20), &view(&i, 0)).unwrap(),
        OverlapResult::NoOverlap
    );
}
#[test]
fn animated_or_unsettled_frames_are_not_accepted() {
    let i = pattern(17, 100);
    let mut filter = StableFrames::default();
    assert!(filter.observe(view(&i, 0), Duration::ZERO).is_none());
    assert!(
        filter
            .observe(view(&i, 1), Duration::from_millis(200))
            .is_none()
    );
    assert!(
        filter
            .observe(view(&i, 1), Duration::from_millis(300))
            .is_none()
    );
    assert!(
        filter
            .observe(view(&i, 1), Duration::from_millis(400))
            .is_some()
    );
}
#[test]
fn output_pixel_budget_checked_before_append() {
    let i = pattern(17, 100);
    let first = view(&i, 0);
    let mut s = Stitcher::new(first.clone(), 17 * 45).unwrap();
    assert!(s.append(&view(&i, 13), 13).is_err());
    assert_eq!(s.finish().unwrap(), first);
    assert!(Stitcher::new(pattern(17, 40), 679).is_err());
}
#[test]
fn fixed_headers_and_partial_changes_cannot_create_seams() {
    let i = pattern(101, 100);
    let previous = view(&i, 0);
    let mut next = view(&i, 13);
    for x in 0..101 {
        for y in 0..4 {
            next.put_pixel(x, y, *previous.get_pixel(x, y));
        }
    }
    assert!(!matches!(
        match_vertical_overlap(&previous, &next).unwrap(),
        OverlapResult::Matched { .. }
    ));
    let mut next = view(&i, 13);
    next.put_pixel(50, 12, Rgba([0; 4]));
    assert!(!matches!(
        match_vertical_overlap(&previous, &next).unwrap(),
        OverlapResult::Matched { .. }
    ));
}
#[test]
fn disagreeing_bands_and_near_ties_are_ambiguous() {
    let i = pattern(99, 100);
    let previous = view(&i, 0);
    let mut next = view(&i, 13);
    for y in 0..40 {
        for x in 66..99 {
            next.put_pixel(x, y, *i.get_pixel(x, y + 14));
        }
    }
    assert_eq!(
        match_vertical_overlap(&previous, &next).unwrap(),
        OverlapResult::Ambiguous
    );
    let mut repeated = RgbaImage::from_fn(99, 100, |x, y| Rgba([x as u8, (y % 4) as u8, 0, 255]));
    repeated.put_pixel(50, 20, Rgba([7, 8, 9, 255]));
    assert!(!matches!(
        match_vertical_overlap(&view(&repeated, 0), &view(&repeated, 4)).unwrap(),
        OverlapResult::Matched { .. }
    ));
}
#[test]
fn stable_observations_are_spaced_and_clock_regression_resets_evidence() {
    let i = pattern(17, 40);
    let mut f = StableFrames::default();
    assert!(f.observe(i.clone(), Duration::from_millis(1000)).is_none());
    assert!(f.observe(i.clone(), Duration::from_millis(1100)).is_none());
    assert!(f.observe(i.clone(), Duration::from_millis(900)).is_none());
    assert!(f.observe(i.clone(), Duration::from_millis(1150)).is_none());
    assert!(f.observe(i, Duration::from_millis(1350)).is_some());
}
#[test]
fn candidate_queue_drops_oldest_before_next_allocation() {
    use vectrcap::scrolling::stable::CandidateQueue;
    let mut q = CandidateQueue::default();
    for n in 0..5 {
        q.reserve_latest();
        q.push(n);
        assert!(q.len() <= 2);
    }
    assert_eq!(q.pop(), Some(3));
    assert_eq!(q.pop(), Some(4));
    assert_eq!(q.pop(), None);
}
#[test]
fn extra_backing_bytes_cannot_bypass_the_pixel_budget() {
    let malformed = RgbaImage::from_raw(17, 40, vec![0; 17 * 40 * 4 + 12]).unwrap();
    assert!(Stitcher::new(malformed.clone(), 680).is_err());
    assert!(match_vertical_overlap(&pattern(17, 40), &malformed).is_err());
    let mut s = Stitcher::new(RgbaImage::new(17, 40), 1000).unwrap();
    assert!(s.append(&malformed, 8).is_err());
    assert_eq!(s.finish().unwrap().as_raw().len(), 17 * 40 * 4);
}
