use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vectrcap::{
    app::error::AppError,
    recording::{options::*, preferences::RecordingPreferences},
    ui::audio_sources::{AudioTest, MeterFactory, MeterStream},
};
struct Factory(Arc<AtomicUsize>);
struct Stream(Arc<AtomicUsize>);
impl Drop for Stream {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl MeterStream for Stream {
    fn peak(&mut self) -> Result<f32, AppError> {
        Ok(0.5)
    }
}
impl MeterFactory for Factory {
    fn open(&self, _: &AudioOptions) -> Result<Box<dyn MeterStream>, AppError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Stream(self.0.clone())))
    }
}
#[test]
fn settings_remember_separate_gif_mp4_options() {
    let mut p = RecordingPreferences::default();
    p.gif.fps = 20;
    p.mp4.fps = 60;
    let bytes = serde_json::to_vec(&p).unwrap();
    let q: RecordingPreferences = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(q, p);
    assert_eq!((q.gif.fps, q.mp4.fps), (20, 60));
}
#[test]
fn audio_off_never_starts_meter_stream() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut t = AudioTest::default();
    t.start(&AudioOptions::default(), &Factory(count.clone()))
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert!(!t.active());
}
#[test]
fn test_audio_is_explicit_and_stops_on_close() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut t = AudioTest::default();
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let o = AudioOptions {
        mode: AudioMode::Both,
        ..Default::default()
    };
    t.start(&o, &Factory(count.clone())).unwrap();
    assert!(t.active());
    assert_eq!(t.peak().unwrap(), 0.5);
    t.stop();
    assert_eq!(count.load(Ordering::SeqCst), 0);
    t.start(&o, &Factory(count.clone())).unwrap();
    drop(t);
    assert_eq!(count.load(Ordering::SeqCst), 0);
}
#[test]
fn test_audio_packets_never_enter_recording() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut t = AudioTest::default();
    t.start(
        &AudioOptions {
            mode: AudioMode::Microphone,
            ..Default::default()
        },
        &Factory(count.clone()),
    )
    .unwrap();
    t.stop();
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(t.peak().unwrap(), 0.0);
}
#[test]
fn source_selection_visible_before_start() {
    let p = RecordingPreferences::default();
    assert_eq!(p.mp4.audio.mode, AudioMode::Off);
    assert_eq!(p.mp4.audio.microphone, EndpointSelection::Default);
    assert!(vectrcap::ui::audio_sources::source_summary(Some(&p.mp4.audio)).contains("Off"));
}
#[test]
fn default_endpoint_is_resolved_once() {
    use vectrcap::recording::audio::devices::SourceKind;
    use vectrcap::recording::session::*;
    let mut calls = 0;
    let r = resolve_sources(
        Some(&AudioOptions {
            mode: AudioMode::Both,
            ..Default::default()
        }),
        |_, _| {
            calls += 1;
            Ok("resolved".into())
        },
    )
    .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(r.len(), 2);
    assert!(r[0].was_default);
    assert!(r[0].affected_by(SourceKind::System, Some("new"), None));
}
#[test]
fn unrelated_default_change_keeps_explicit_endpoint() {
    use vectrcap::recording::audio::devices::SourceKind;
    use vectrcap::recording::session::*;
    let r = resolve_sources(
        Some(&AudioOptions {
            mode: AudioMode::System,
            system: EndpointSelection::Id("explicit".into()),
            ..Default::default()
        }),
        |_, _| Ok("explicit".into()),
    )
    .unwrap();
    assert!(!r[0].affected_by(SourceKind::System, Some("new-default"), None));
    assert!(r[0].affected_by(SourceKind::System, None, Some("explicit")));
}
#[test]
fn permission_or_device_failure_requires_user_choice() {
    let r = vectrcap::recording::session::resolve_sources(
        Some(&AudioOptions {
            mode: AudioMode::Both,
            ..Default::default()
        }),
        |_, _| Err(AppError::Capture("permission denied")),
    );
    assert!(r.is_err());
}
#[test]
fn quality_countdown_size_and_timed_stop_controls_validate() {
    let mut p = RecordingPreferences::default();
    assert!(p.validate().is_ok());
    p.countdown_seconds = 61;
    assert!(p.validate().is_err());
    p.countdown_seconds = 3;
    p.mp4.custom_bitrate = Some(1);
    assert!(p.validate().is_err());
    p.mp4.custom_bitrate = None;
    p.gif_timed_stop = Some(0);
    assert!(p.validate().is_err());
}

#[test]
fn enabled_mp4_resolves_only_selected_sources() {
    use vectrcap::recording::{audio::devices::SourceKind, session::resolve_sources};
    for mode in [
        AudioMode::Off,
        AudioMode::System,
        AudioMode::Microphone,
        AudioMode::Both,
    ] {
        let mut seen = vec![];
        let options = AudioOptions {
            mode,
            ..Default::default()
        };
        resolve_sources(Some(&options), |kind, _| {
            seen.push(kind);
            Ok("endpoint".into())
        })
        .unwrap();
        assert_eq!(
            seen.contains(&SourceKind::System),
            matches!(mode, AudioMode::System | AudioMode::Both)
        );
        assert_eq!(
            seen.contains(&SourceKind::Microphone),
            matches!(mode, AudioMode::Microphone | AudioMode::Both)
        );
    }
}

#[test]
fn saved_global_recording_bindings_dispatch_once_per_press() {
    use vectrcap::app::{destinations::DestinationSelection, events::AppCommand, shortcuts::*};
    struct Registrar;
    impl ShortcutRegistrar for Registrar {
        fn register(&self, _: RegistrationId, _: KeyChord) -> Result<(), AppError> {
            Ok(())
        }
        fn unregister(&self, _: RegistrationId) {}
    }
    let actions = [
        ShortcutAction::ToggleGifRecording,
        ShortcutAction::ToggleMp4Recording,
        ShortcutAction::StopRecording,
        ShortcutAction::PauseResumeRecording,
        ShortcutAction::CancelRecording,
    ];
    let bindings = actions
        .into_iter()
        .enumerate()
        .map(|(i, action)| {
            let mut b = ShortcutBinding::unassigned(action);
            b.enabled = true;
            b.chord = Some(KeyChord {
                ctrl: true,
                shift: true,
                alt: false,
                win: false,
                key: KeyCode::from_vk(0x31 + i as u32).unwrap(),
            });
            b
        })
        .collect();
    let (m, failed) = ShortcutManager::new(Box::new(Registrar), bindings);
    assert!(failed.is_empty());
    for action in actions {
        assert!(
            m.handle(
                ShortcutEvent {
                    action,
                    repeat: false
                },
                ShortcutFocus::Normal
            )
            .is_some()
        );
        assert!(
            m.handle(
                ShortcutEvent {
                    action,
                    repeat: true
                },
                ShortcutFocus::Normal
            )
            .is_none()
        );
        assert!(
            m.handle(
                ShortcutEvent {
                    action,
                    repeat: false
                },
                ShortcutFocus::ShortcutEntry
            )
            .is_none()
        );
    }
    assert!(matches!(
        m.handle(
            ShortcutEvent {
                action: ShortcutAction::ToggleGifRecording,
                repeat: false
            },
            ShortcutFocus::Normal
        ),
        Some(ShortcutOutcome::Command(AppCommand::ToggleRecordingFor {
            mp4: false,
            selection: DestinationSelection::Inherit
        }))
    ));
}
