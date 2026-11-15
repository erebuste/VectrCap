use vectrcap::{capture::geometry::PhysicalRect, recording::options::*};
fn opts(mp4: bool) -> RecordingOptions {
    RecordingOptions::new(
        "test-monitor".into(),
        PhysicalRect::new(31, 17, 641, 359).unwrap(),
        mp4,
    )
}
#[test]
fn recording_defaults_match_approved_controls() {
    let o = opts(true);
    assert!(o.cursor && o.confirm_discard);
    assert_eq!(o.countdown_seconds, 3);
    assert_eq!(o.timed_stop_seconds, None);
    assert_eq!(o.maximum_active_seconds, 600);
    assert_eq!(o.output_size, OutputSize::Original);
    let FormatOptions::Mp4(m) = o.format else {
        panic!()
    };
    assert_eq!(m.fps, 30);
    assert_eq!(m.quality, Quality::Balanced);
    assert_eq!(m.audio.mode, AudioMode::Off);
    assert_eq!(m.audio.aac_kbps, 128);
    assert_eq!((m.audio.system_gain, m.audio.microphone_gain), (100, 100));
    assert_eq!(opts(false).maximum_active_seconds, 60);
    assert_eq!(opts(false).fps(), 15);
}
#[test]
fn format_specific_fps_and_audio_are_validated() {
    let mut o = opts(false);
    o.format = FormatOptions::Gif(GifOptions { fps: 30 });
    assert!(o.validate().is_err());
    o.format = FormatOptions::Mp4(Mp4Options {
        fps: 20,
        ..Default::default()
    });
    assert!(o.validate().is_err());
    o = opts(true);
    o.countdown_seconds = 61;
    assert!(o.validate().is_err());
}
#[test]
fn timed_stop_cannot_exceed_active_cap() {
    let mut o = opts(false);
    for t in [0, 61] {
        o.timed_stop_seconds = Some(t);
        assert!(o.validate().is_err());
    }
    o.timed_stop_seconds = Some(60);
    assert!(o.validate().is_ok());
    assert_eq!(o.active_limit_seconds(), 60);
}
#[test]
fn quality_presets_scale_bitrate_to_output() {
    let mut m = Mp4Options::default();
    assert_eq!(m.bitrate(1920, 1080), 7_465_000);
    m.quality = Quality::Small;
    assert_eq!(m.bitrate(1920, 1080), 3_732_000);
    m.quality = Quality::High;
    assert_eq!(m.bitrate(1920, 1080), 12_442_000);
}
#[test]
fn custom_bitrate_and_aac_choices_are_bounded() {
    let mut o = opts(true);
    for b in [499_999, 100_000_001] {
        o.format = FormatOptions::Mp4(Mp4Options {
            custom_bitrate: Some(b),
            ..Default::default()
        });
        assert!(o.validate().is_err());
    }
    for rate in [96, 128, 160, 192] {
        let mut m = Mp4Options::default();
        m.audio.aac_kbps = rate;
        o.format = FormatOptions::Mp4(m);
        assert!(o.validate().is_ok());
    }
}
#[test]
fn downscale_preserves_aspect_without_upscaling() {
    assert_eq!(
        OutputSize::Fit1280x720.dimensions(2560, 1440).unwrap(),
        (1280, 720)
    );
    assert_eq!(
        OutputSize::Fit1280x720.dimensions(600, 1000).unwrap(),
        (432, 720)
    );
    assert_eq!(
        OutputSize::Fit1920x1080.dimensions(641, 359).unwrap(),
        (641, 359)
    );
}
#[test]
fn odd_output_padding_never_reads_outside_selection() {
    assert_eq!(opts(true).dimensions().unwrap(), (642, 360));
    assert_eq!(opts(false).dimensions().unwrap(), (641, 359));
}
