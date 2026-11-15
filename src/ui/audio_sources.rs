use crate::{app::error::AppError, recording::options::*};
pub trait MeterStream: Send {
    fn peak(&mut self) -> Result<f32, AppError>;
}
pub trait MeterFactory {
    fn open(&self, options: &AudioOptions) -> Result<Box<dyn MeterStream>, AppError>;
}
#[derive(Default)]
pub struct AudioTest {
    stream: Option<Box<dyn MeterStream>>,
}
impl AudioTest {
    pub fn start(&mut self, o: &AudioOptions, f: &dyn MeterFactory) -> Result<(), AppError> {
        self.stop();
        o.validate()?;
        if o.mode != AudioMode::Off {
            self.stream = Some(f.open(o)?);
        }
        Ok(())
    }
    pub fn stop(&mut self) {
        self.stream = None;
    }
    pub fn active(&self) -> bool {
        self.stream.is_some()
    }
    pub fn peak(&mut self) -> Result<f32, AppError> {
        self.stream.as_mut().map_or(Ok(0.0), |s| s.peak())
    }
}
pub fn source_summary(o: Option<&AudioOptions>) -> String {
    o.map_or_else(
        || "Audio Off".into(),
        |o| {
            format!(
                "Audio {:?}; System {:?}; Microphone {:?}",
                o.mode, o.system, o.microphone
            )
        },
    )
}

pub struct NativeMeterFactory;
struct NativeMeter {
    sources: Vec<crate::recording::audio::wasapi::AudioSourceHandle>,
}
impl MeterFactory for NativeMeterFactory {
    fn open(&self, o: &AudioOptions) -> Result<Box<dyn MeterStream>, AppError> {
        let sources = crate::recording::audio::devices::open_selected(Some(o), |kind, e| {
            let s = crate::recording::audio::wasapi::AudioSourceHandle::start(kind, e.clone())?;
            s.resume()?;
            Ok(s)
        })?;
        Ok(Box::new(NativeMeter { sources }))
    }
}
impl MeterStream for NativeMeter {
    fn peak(&mut self) -> Result<f32, AppError> {
        let mut peak = 0.0_f32;
        for source in &self.sources {
            while let Some(b) = source.pop()? {
                for sample in b.stereo {
                    peak = peak.max(sample.abs());
                }
            }
        }
        Ok(peak.min(1.0))
    }
}
/// Only peaks cross this boundary. Native packets are consumed and dropped on
/// this owner; no recording encoder or timeline is available to a meter.
pub struct MeterService {
    cancel: tokio_util::sync::CancellationToken,
    thread: Option<std::thread::JoinHandle<()>>,
    pub status: std::sync::Arc<std::sync::Mutex<Result<f32, AppError>>>,
}
impl MeterService {
    pub fn start(options: AudioOptions) -> Result<Self, AppError> {
        let cancel = tokio_util::sync::CancellationToken::new();
        let token = cancel.clone();
        let status = std::sync::Arc::new(std::sync::Mutex::new(Ok(0.0)));
        let shared = status.clone();
        let thread = std::thread::Builder::new()
            .name("vectrcap-audio-test".into())
            .spawn(move || {
                let mut test = AudioTest::default();
                let result = (|| {
                    test.start(&options, &NativeMeterFactory)?;
                    while !token.is_cancelled() {
                        let peak = test.peak()?;
                        *shared
                            .lock()
                            .map_err(|_| AppError::Capture("audio meter state unavailable"))? =
                            Ok(peak);
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Ok(())
                })();
                if let Err(e) = result
                    && let Ok(mut s) = shared.lock()
                {
                    *s = Err(e);
                }
                test.stop();
            })
            .map_err(|e| AppError::io("starting audio test", &e))?;
        Ok(Self {
            cancel,
            thread: Some(thread),
            status,
        })
    }
    pub fn stop(mut self) {
        self.cancel.cancel();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
impl Drop for MeterService {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
pub use crate::recording::audio::devices::AudioEndpoint;
pub fn selectors(ui: &mut eframe::egui::Ui, o: &mut AudioOptions, endpoints: &[AudioEndpoint]) {
    use crate::recording::audio::devices::SourceKind;
    use eframe::egui;
    egui::ComboBox::from_id_salt("audio-mode")
        .selected_text(format!("{:?}", o.mode))
        .show_ui(ui, |ui| {
            for m in [
                AudioMode::Off,
                AudioMode::System,
                AudioMode::Microphone,
                AudioMode::Both,
            ] {
                ui.selectable_value(&mut o.mode, m, format!("{m:?}"));
            }
        });
    for (label, kind, selection, gain, enabled) in [
        (
            "Playback device",
            SourceKind::System,
            &mut o.system,
            &mut o.system_gain,
            matches!(o.mode, AudioMode::System | AudioMode::Both),
        ),
        (
            "Input device",
            SourceKind::Microphone,
            &mut o.microphone,
            &mut o.microphone_gain,
            matches!(o.mode, AudioMode::Microphone | AudioMode::Both),
        ),
    ] {
        ui.add_enabled_ui(enabled, |ui| {
            let name = match selection {
                EndpointSelection::Default => "Use default (resolved once at Start)".to_owned(),
                EndpointSelection::Id(id) => endpoints.iter().find(|e| e.id == *id).map_or_else(
                    || format!("Selected endpoint unavailable: {id}"),
                    |e| e.name.clone(),
                ),
            };
            ui.label(label);
            egui::ComboBox::from_id_salt(label)
                .selected_text(name)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        selection,
                        EndpointSelection::Default,
                        "Use default (resolved once at Start)",
                    );
                    for e in endpoints.iter().filter(|e| e.kind == kind) {
                        ui.selectable_value(
                            selection,
                            EndpointSelection::Id(e.id.clone()),
                            &e.name,
                        );
                    }
                });
            ui.add(egui::Slider::new(gain, 0..=100).text("Source volume %"));
        });
    }
    ui.label(
        "System loopback includes other apps and notifications on the selected playback device.",
    );
    egui::ComboBox::from_id_salt("aac-rate")
        .selected_text(format!("{} kbps AAC", o.aac_kbps))
        .show_ui(ui, |ui| {
            for rate in [96, 128, 160, 192] {
                ui.selectable_value(&mut o.aac_kbps, rate, format!("{rate} kbps"));
            }
        });
}
