//! Owns the native handle off both the GUI and controller threads.
use super::{
    options::RecordingOptions,
    session::RecordingInput,
    worker::{RecordingControl, RecordingWorker},
};
use crate::app::error::AppError;
use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
pub struct RuntimeMessage {
    pub session: uuid::Uuid,
    pub input: RecordingInput,
}
pub struct RecordingRuntime {
    controls: mpsc::SyncSender<RecordingControl>,
    cancel: CancellationToken,
    thread: Option<thread::JoinHandle<()>>,
}
impl RecordingRuntime {
    pub fn start(
        session: uuid::Uuid,
        options: RecordingOptions,
        sources: Vec<super::session::ResolvedSource>,
        events: tokio::sync::mpsc::Sender<RuntimeMessage>,
        origin: Instant,
    ) -> Result<Self, AppError> {
        let (tx, rx) = mpsc::sync_channel(8);
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let thread = thread::Builder::new()
            .name("vectrcap-recording-session".into())
            .spawn(move || {
                let send = |input| {
                    events
                        .blocking_send(RuntimeMessage { session, input })
                        .is_ok()
                };
                let (event_tx, event_rx) = mpsc::sync_channel(16);
                let result = (|| {
                    let handle =
                        RecordingWorker::start_resolved(options, token.clone(), event_tx, sources)?;
                    loop {
                        while let Ok(event) = event_rx.try_recv() {
                            if !send(RecordingInput::Worker {
                                event,
                                now: origin.elapsed(),
                            }) {
                                token.cancel();
                            }
                        }
                        if handle.is_finished() {
                            break;
                        }
                        if token.is_cancelled() {
                            let _ = handle.control(RecordingControl::Cancel);
                        }
                        match rx.recv_timeout(Duration::from_millis(10)) {
                            Ok(c) => handle.control(c)?,
                            Err(mpsc::RecvTimeoutError::Disconnected) => token.cancel(),
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                        }
                    }
                    while let Ok(event) = event_rx.try_recv() {
                        send(RecordingInput::Worker {
                            event,
                            now: origin.elapsed(),
                        });
                    }
                    handle.finish()
                })();
                send(RecordingInput::Finalized(result));
            })
            .map_err(|e| AppError::io("starting recording session", &e))?;
        Ok(Self {
            controls: tx,
            cancel,
            thread: Some(thread),
        })
    }
    pub fn control(&self, c: RecordingControl) -> Result<(), AppError> {
        if c == RecordingControl::Cancel {
            self.cancel.cancel();
            return Ok(());
        }
        self.controls
            .try_send(c)
            .map_err(|_| AppError::InUse("recording session control queue is full or closed"))
    }
    pub fn join(mut self) {
        self.cancel.cancel();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
impl Drop for RecordingRuntime {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
