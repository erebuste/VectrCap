//! Pure recording lifecycle. Only worker acknowledgements change live controls.
use super::{
    EncodedRecording,
    audio::devices::{SourceKind, open_selected},
    clock::MediaClock,
    options::*,
    worker::{RecordingControl, RecordingEvent},
};
use crate::{
    app::{error::AppError, events::AppCommand, shortcuts::ShortcutAction},
    capture::geometry::MonitorGeometry,
};
use std::time::Duration;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordingState {
    #[default]
    Idle,
    Selecting,
    Countdown,
    Recording,
    Paused,
    Finalizing,
    Review,
    Interrupted,
    Failed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interruption {
    Locked,
    Sleep,
    DisplayChanged,
    AudioDevice,
    TimingFailure,
}
impl Interruption {
    pub fn label(self) -> &'static str {
        match self {
            Self::Locked => "Session locked",
            Self::Sleep => "Computer suspended",
            Self::DisplayChanged => "Display configuration changed",
            Self::AudioDevice => "Selected audio source changed or became unavailable",
            Self::TimingFailure => "Capture or audio timing failed",
        }
    }
}
pub enum RecordingInput {
    Select,
    Start {
        options: RecordingOptions,
        monitor: MonitorGeometry,
        now: Duration,
    },
    Tick(Duration),
    Worker {
        event: RecordingEvent,
        now: Duration,
    },
    Finalized(Result<EncodedRecording, AppError>),
    Failed(AppError),
    Stop,
    PauseResume,
    Cancel,
    ConfirmDiscard(bool),
    Interrupt(Interruption),
    ToggleFormat(bool),
    Upload,
    Save,
    Discard,
    StartWithoutAudio {
        now: Duration,
    },
}
pub enum RecordingEffect {
    StopMeters,
    StartWorker(RecordingOptions),
    Control(RecordingControl),
    Review,
    Upload,
    Save,
    Discard,
    Error(AppError),
}
#[derive(Default)]
pub struct RecordingSession {
    state: RecordingState,
    options: Option<RecordingOptions>,
    monitor: Option<MonitorGeometry>,
    countdown: Duration,
    clock: Option<MediaClock>,
    elapsed: Duration,
    clock_base: Duration,
    pending: Option<RecordingControl>,
    worker: bool,
    cancelling: bool,
    confirmation: bool,
    output: Option<EncodedRecording>,
    interruption: Option<Interruption>,
    error: Option<AppError>,
}
impl RecordingSession {
    pub fn state(&self) -> RecordingState {
        self.state
    }
    pub fn options(&self) -> Option<&RecordingOptions> {
        self.options.as_ref()
    }
    pub fn output(&self) -> Option<&EncodedRecording> {
        self.output.as_ref()
    }
    pub fn take_output(&mut self) -> Option<EncodedRecording> {
        self.output.take()
    }
    pub fn confirmation_pending(&self) -> bool {
        self.confirmation
    }
    pub fn active_elapsed(&self) -> Duration {
        self.elapsed
    }
    pub fn interruption(&self) -> Option<Interruption> {
        self.interruption
    }
    pub fn error(&self) -> Option<&AppError> {
        self.error.as_ref()
    }
    pub fn acquiring(&self) -> bool {
        matches!(
            self.state,
            RecordingState::Selecting
                | RecordingState::Countdown
                | RecordingState::Recording
                | RecordingState::Paused
                | RecordingState::Finalizing
                | RecordingState::Interrupted
        )
    }
    pub fn countdown_remaining(&self, now: Duration) -> Duration {
        self.countdown.saturating_sub(now)
    }
    fn stop(&mut self) -> Vec<RecordingEffect> {
        if !self.worker
            || matches!(
                self.state,
                RecordingState::Finalizing | RecordingState::Interrupted
            )
        {
            return vec![];
        }
        self.pending = None;
        self.state = RecordingState::Finalizing;
        vec![RecordingEffect::Control(RecordingControl::Stop)]
    }
    fn cancel(&mut self) -> Vec<RecordingEffect> {
        self.confirmation = false;
        if self.worker {
            self.cancelling = true;
            self.pending = None;
            self.state = RecordingState::Finalizing;
            vec![RecordingEffect::Control(RecordingControl::Cancel)]
        } else {
            self.state = RecordingState::Idle;
            vec![]
        }
    }
    pub fn handle(&mut self, input: RecordingInput) -> Vec<RecordingEffect> {
        match input {
            RecordingInput::Select if self.state == RecordingState::Idle => {
                self.state = RecordingState::Selecting;
                vec![]
            }
            RecordingInput::Start {
                options,
                monitor,
                now,
            } if matches!(self.state, RecordingState::Idle | RecordingState::Selecting) => {
                if let Err(e) = options.validate_monitor(&monitor) {
                    return vec![RecordingEffect::Error(e)];
                }
                self.countdown =
                    now.saturating_add(Duration::from_secs(u64::from(options.countdown_seconds)));
                self.options = Some(options);
                self.monitor = Some(monitor);
                self.state = RecordingState::Countdown;
                self.error = None;
                vec![RecordingEffect::StopMeters]
            }
            RecordingInput::Tick(now) => {
                if self.state == RecordingState::Countdown && !self.worker && now >= self.countdown
                {
                    self.worker = true;
                    let mut o = self.options.clone().expect("validated session options");
                    o.countdown_seconds = 0;
                    return vec![RecordingEffect::StartWorker(o)];
                }
                if matches!(
                    self.state,
                    RecordingState::Recording | RecordingState::Paused
                ) && let Some(c) = &mut self.clock
                {
                    match c.elapsed(now) {
                        Ok(t) => self.elapsed = self.clock_base.saturating_add(t),
                        Err(e) => {
                            self.error = Some(e);
                            return self
                                .handle(RecordingInput::Interrupt(Interruption::TimingFailure));
                        }
                    }
                    if self.elapsed
                        >= Duration::from_secs(u64::from(
                            self.options
                                .as_ref()
                                .expect("active options")
                                .active_limit_seconds(),
                        ))
                    {
                        return self.stop();
                    }
                }
                vec![]
            }
            RecordingInput::Worker { event, now } => {
                match event {
                    RecordingEvent::Started
                        if self.worker && self.state == RecordingState::Countdown =>
                    {
                        self.clock = Some(MediaClock::new(now));
                        self.state = RecordingState::Recording;
                    }
                    RecordingEvent::Paused(t)
                        if self.pending == Some(RecordingControl::Pause)
                            && self.state == RecordingState::Recording =>
                    {
                        let mut clock = MediaClock::new(now);
                        let _ = clock.pause(now);
                        self.clock = Some(clock);
                        self.clock_base = t;
                        self.elapsed = t;
                        self.pending = None;
                        self.state = RecordingState::Paused;
                    }
                    RecordingEvent::Resumed(t)
                        if self.pending == Some(RecordingControl::Resume)
                            && self.state == RecordingState::Paused =>
                    {
                        self.clock = Some(MediaClock::new(now));
                        self.clock_base = t;
                        self.elapsed = t;
                        self.pending = None;
                        self.state = RecordingState::Recording;
                    }
                    RecordingEvent::Finalizing if self.worker => {
                        if self.state != RecordingState::Interrupted {
                            self.state = RecordingState::Finalizing;
                        }
                        self.pending = None;
                    }
                    RecordingEvent::Interrupted(e) if self.worker => {
                        self.error = Some(e);
                        self.interruption.get_or_insert(Interruption::TimingFailure);
                        self.state = RecordingState::Interrupted;
                        self.pending = None;
                    }
                    _ => {}
                }
                vec![]
            }
            RecordingInput::PauseResume
                if self.pending.is_none()
                    && !self.confirmation
                    && matches!(
                        self.state,
                        RecordingState::Recording | RecordingState::Paused
                    ) =>
            {
                let c = if self.state == RecordingState::Paused {
                    RecordingControl::Resume
                } else {
                    RecordingControl::Pause
                };
                self.pending = Some(c);
                vec![RecordingEffect::Control(c)]
            }
            RecordingInput::Stop => self.stop(),
            RecordingInput::ToggleFormat(mp4)
                if self
                    .options
                    .as_ref()
                    .is_some_and(|o| matches!(o.format, FormatOptions::Mp4(_)) == mp4) =>
            {
                self.stop()
            }
            RecordingInput::Cancel
                if matches!(self.state, RecordingState::Review | RecordingState::Failed) =>
            {
                vec![RecordingEffect::Discard]
            }
            RecordingInput::Cancel if self.state != RecordingState::Idle => {
                if matches!(
                    self.state,
                    RecordingState::Recording | RecordingState::Paused
                ) && self.options.as_ref().is_some_and(|o| o.confirm_discard)
                {
                    self.confirmation = true;
                    vec![]
                } else {
                    self.cancel()
                }
            }
            RecordingInput::ConfirmDiscard(yes) if self.confirmation => {
                self.confirmation = false;
                if yes { self.cancel() } else { vec![] }
            }
            RecordingInput::Interrupt(reason)
                if self.worker
                    && matches!(
                        self.state,
                        RecordingState::Countdown
                            | RecordingState::Recording
                            | RecordingState::Paused
                    ) =>
            {
                self.interruption = Some(reason);
                self.pending = None;
                self.state = RecordingState::Interrupted;
                vec![RecordingEffect::Control(RecordingControl::Stop)]
            }
            RecordingInput::Interrupt(_)
                if matches!(
                    self.state,
                    RecordingState::Selecting | RecordingState::Countdown
                ) =>
            {
                self.cancel()
            }
            RecordingInput::Finalized(result) if self.worker => {
                self.worker = false;
                self.pending = None;
                if self.cancelling {
                    self.cancelling = false;
                    self.state = RecordingState::Idle;
                    return vec![];
                }
                match result {
                    Ok(o) => {
                        self.elapsed = o.active_duration;
                        self.output = Some(o);
                        self.state = RecordingState::Review;
                        vec![RecordingEffect::Review]
                    }
                    Err(e) => self.handle(RecordingInput::Failed(e)),
                }
            }
            RecordingInput::Failed(e) => {
                let active = self.worker;
                self.worker = false;
                self.pending = None;
                self.state = RecordingState::Failed;
                self.error = Some(e.clone());
                let mut effects = vec![RecordingEffect::Error(e)];
                if active {
                    effects.push(RecordingEffect::Control(RecordingControl::Cancel));
                }
                effects
            }
            RecordingInput::Upload if self.state == RecordingState::Review => {
                vec![RecordingEffect::Upload]
            }
            RecordingInput::Save if self.state == RecordingState::Review => {
                vec![RecordingEffect::Save]
            }
            RecordingInput::Discard
                if matches!(self.state, RecordingState::Review | RecordingState::Failed) =>
            {
                vec![RecordingEffect::Discard]
            }
            RecordingInput::StartWithoutAudio { now }
                if self.state == RecordingState::Failed
                    && !self.worker
                    && self.output.is_none() =>
            {
                let Some(mut o) = self.options.clone() else {
                    return vec![];
                };
                if let FormatOptions::Mp4(m) = &mut o.format {
                    m.audio.mode = AudioMode::Off;
                } else {
                    return vec![];
                }
                self.state = RecordingState::Idle;
                self.handle(RecordingInput::Start {
                    options: o,
                    monitor: self.monitor.clone().expect("validated monitor"),
                    now,
                })
            }
            _ => vec![],
        }
    }
}
pub fn shortcut_command(action: ShortcutAction) -> Option<AppCommand> {
    match action {
        ShortcutAction::ToggleGifRecording => Some(AppCommand::ToggleRecording { mp4: false }),
        ShortcutAction::ToggleMp4Recording => Some(AppCommand::ToggleRecording { mp4: true }),
        ShortcutAction::StopRecording => Some(AppCommand::Recording(RecordingInput::Stop)),
        ShortcutAction::PauseResumeRecording => {
            Some(AppCommand::Recording(RecordingInput::PauseResume))
        }
        ShortcutAction::CancelRecording => Some(AppCommand::Recording(RecordingInput::Cancel)),
        _ => None,
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSource {
    pub kind: SourceKind,
    pub id: String,
    pub was_default: bool,
}
impl ResolvedSource {
    pub fn affected_by(
        &self,
        kind: SourceKind,
        default: Option<&str>,
        invalidated: Option<&str>,
    ) -> bool {
        self.kind == kind
            && (invalidated == Some(self.id.as_str())
                || (self.was_default && default.is_some_and(|d| d != self.id)))
    }
}
pub fn resolve_sources(
    options: Option<&AudioOptions>,
    mut resolve: impl FnMut(SourceKind, &EndpointSelection) -> Result<String, AppError>,
) -> Result<Vec<ResolvedSource>, AppError> {
    open_selected(options, |kind, selection| {
        let id = resolve(kind, selection)?;
        AudioOptions {
            system: EndpointSelection::Id(id.clone()),
            ..Default::default()
        }
        .validate()?;
        Ok(ResolvedSource {
            kind,
            id,
            was_default: matches!(selection, EndpointSelection::Default),
        })
    })
}
