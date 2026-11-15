use std::time::Duration as D;
use vectrcap::{
    capture::geometry::PhysicalRect,
    recording::{
        audio::{devices::*, mix::*, resample::*, *},
        clock::MediaClock,
        options::*,
    },
};
#[test]
fn off_and_gif_never_start_audio_devices() {
    for mp4 in [false, true] {
        let o = RecordingOptions::new("test".into(), PhysicalRect::new(0, 0, 16, 16).unwrap(), mp4);
        let mut calls = 0;
        open_selected(o.audio(), |_, _| {
            calls += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, 0);
    }
}
#[test]
fn only_selected_sources_are_opened() {
    for (mode, count) in [
        (AudioMode::System, 1),
        (AudioMode::Microphone, 1),
        (AudioMode::Both, 2),
    ] {
        let o = AudioOptions {
            mode,
            ..Default::default()
        };
        let mut calls = Vec::new();
        open_selected(Some(&o), |kind, _| {
            calls.push(kind);
            Ok(())
        })
        .unwrap();
        assert_eq!(calls.len(), count);
    }
}
#[test]
fn mono_microphone_duplicates_to_stereo_explicitly() {
    let p = normalize_pcm(&[0, 64, 0, 192], 2, SampleFormat::Pcm16, 1, false).unwrap();
    assert_eq!(p, vec![0.5, 0.5, -0.5, -0.5]);
}
#[test]
fn source_rates_resample_to_48000() {
    for rate in [44100, 48000, 96000] {
        let mut r = Resampler::new(rate).unwrap();
        let tone: Vec<f32> = (0..rate / 10)
            .flat_map(|i| {
                let x = (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin();
                [x, x]
            })
            .collect();
        let out = r.process(&tone).unwrap();
        assert!((out.len() as isize / 2 - 4800).abs() < 64);
        assert!(out.iter().all(|x| x.is_finite() && x.abs() < 1.1));
    }
}
#[test]
fn two_sources_mix_with_headroom() {
    let o = AudioOptions {
        mode: AudioMode::Both,
        ..Default::default()
    };
    let v = mix_stereo(Some(&[1.0, -1.0]), Some(&[0.5, 0.5]), &o).unwrap();
    assert_eq!(v, vec![24575, -8192]);
    assert!(
        mix_stereo(
            Some(&[f32::NAN, 0.0]),
            None,
            &AudioOptions {
                mode: AudioMode::System,
                ..Default::default()
            }
        )
        .is_err()
    );
}
#[test]
fn silent_packet_never_dereferences_null_data() {
    assert_eq!(
        normalize_pcm(&[], 48, SampleFormat::Float32, 2, true).unwrap(),
        vec![0.0; 96]
    );
}
#[test]
fn audio_queues_stay_bounded() {
    let mut q = AudioQueue::default();
    for i in 0..25 {
        q.push(AudioChunk::new(vec![0; 960], D::from_millis(i * 10)).unwrap())
            .unwrap();
    }
    assert!(
        q.push(AudioChunk::new(vec![0; 960], D::from_millis(250)).unwrap())
            .is_err()
    );
    assert_eq!(q.sample_frames(), 12000);
}
#[test]
fn pause_discards_audio_and_video_interval() {
    let mut c = MediaClock::new(D::ZERO);
    c.pause(D::from_secs(2)).unwrap();
    assert!(c.accept_timestamp(D::from_secs(5)).unwrap().is_none());
    c.resume(D::from_secs(12)).unwrap();
    assert_eq!(
        c.accept_timestamp(D::from_secs(15)).unwrap(),
        Some(D::from_secs(5))
    );
}
#[test]
fn device_or_timestamp_failure_does_not_silently_switch() {
    let o = AudioOptions {
        mode: AudioMode::Both,
        ..Default::default()
    };
    let mut n = 0;
    assert!(
        open_selected(Some(&o), |_, _| {
            n += 1;
            Err::<(), _>(vectrcap::app::error::AppError::Capture(
                "selected device unavailable",
            ))
        })
        .is_err()
    );
    assert_eq!(n, 1);
    let mut c = PacketClock::new(48000).unwrap();
    c.observe(0, 10000, 480).unwrap();
    assert!(c.observe(480, 9000, 480).is_err());
}
#[test]
fn audio_mux_uses_common_active_timeline() {
    let mut c = MediaClock::new(D::ZERO);
    c.pause(D::from_secs(2)).unwrap();
    c.resume(D::from_secs(12)).unwrap();
    let t = c.elapsed(D::from_secs(15)).unwrap();
    let chunk = AudioChunk::new(vec![0; 960], t).unwrap();
    assert_eq!(chunk.timestamp, D::from_secs(5));
    assert_eq!(chunk.duration(), D::from_millis(10));
}
