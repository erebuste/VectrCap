use std::time::Duration as D;
use vectrcap::recording::clock::MediaClock;

#[test]
fn pause_excludes_wall_time_from_media() {
    let mut c = MediaClock::new(D::ZERO);
    c.pause(D::from_secs(2)).unwrap();
    c.resume(D::from_secs(12)).unwrap();
    assert_eq!(c.elapsed(D::from_secs(15)).unwrap(), D::from_secs(5));
}
#[test]
fn repeated_pause_resume_is_idempotent() {
    let mut c = MediaClock::new(D::ZERO);
    c.pause(D::from_secs(2)).unwrap();
    c.pause(D::from_secs(4)).unwrap();
    c.resume(D::from_secs(12)).unwrap();
    c.resume(D::from_secs(13)).unwrap();
    assert_eq!(c.elapsed(D::from_secs(15)).unwrap(), D::from_secs(5));
}
#[test]
fn stop_while_paused_uses_last_active_time() {
    let mut c = MediaClock::new(D::ZERO);
    c.pause(D::from_secs(2)).unwrap();
    assert_eq!(c.elapsed(D::from_secs(12)).unwrap(), D::from_secs(2));
    assert!(c.accept_timestamp(D::from_secs(13)).unwrap().is_none());
}
#[test]
fn resume_has_monotonic_frame_timestamps() {
    let mut c = MediaClock::new(D::ZERO);
    assert_eq!(
        c.accept_timestamp(D::from_secs(1)).unwrap(),
        Some(D::from_secs(1))
    );
    c.pause(D::from_secs(2)).unwrap();
    c.resume(D::from_secs(12)).unwrap();
    assert_eq!(
        c.accept_timestamp(D::from_secs(13)).unwrap(),
        Some(D::from_secs(3))
    );
    assert!(c.elapsed(D::from_secs(12)).is_err());
}
#[test]
fn gif_delay_does_not_include_paused_interval() {
    let mut c = MediaClock::new(D::ZERO);
    c.pause(D::from_secs(2)).unwrap();
    c.resume(D::from_secs(12)).unwrap();
    let end = c.elapsed(D::from_secs(15)).unwrap();
    let ticks = vectrcap::recording::gif::centiseconds(end).unwrap();
    assert_eq!(ticks, 500);
    let ticks: Vec<_> = (0..=15)
        .map(|i| {
            vectrcap::recording::gif::centiseconds(D::from_nanos(i * 1_000_000_000 / 15)).unwrap()
        })
        .collect();
    assert_eq!(ticks[15], 100);
    assert!(ticks.windows(2).all(|w| [6, 7].contains(&(w[1] - w[0]))));
}
