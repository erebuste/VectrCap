use crate::app::error::AppError;
use std::time::Duration;

/// Inputs are monotonic elapsed time (QPC for native capture), never wall-clock time.
#[derive(Debug)]
pub struct MediaClock {
    last: Duration,
    active: Duration,
    paused: bool,
}
impl MediaClock {
    pub fn new(now: Duration) -> Self {
        Self {
            last: now,
            active: Duration::ZERO,
            paused: false,
        }
    }
    pub fn elapsed(&mut self, now: Duration) -> Result<Duration, AppError> {
        let delta = now
            .checked_sub(self.last)
            .ok_or(AppError::Capture("monotonic clock moved backwards"))?;
        let next = if self.paused {
            self.active
        } else {
            self.active
                .checked_add(delta)
                .ok_or(AppError::Capture("media clock overflow"))?
        };
        // Both native sample time and duration are signed 100-nanosecond values.
        if next.as_nanos() / 100 > i64::MAX as u128 {
            return Err(AppError::Capture(
                "media clock exceeds native timestamp range",
            ));
        }
        self.last = now;
        self.active = next;
        Ok(next)
    }
    pub fn pause(&mut self, now: Duration) -> Result<Duration, AppError> {
        let t = self.elapsed(now)?;
        self.paused = true;
        Ok(t)
    }
    pub fn resume(&mut self, now: Duration) -> Result<Duration, AppError> {
        let t = self.elapsed(now)?;
        self.paused = false;
        Ok(t)
    }
    pub fn is_paused(&self) -> bool {
        self.paused
    }
    pub fn accept_timestamp(&mut self, now: Duration) -> Result<Option<Duration>, AppError> {
        let t = self.elapsed(now)?;
        Ok((!self.paused).then_some(t))
    }
}
