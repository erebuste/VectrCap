use super::{overlap::*, stable::StableFrames, stitch::Stitcher, *};
use image::RgbaImage;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollState {
    #[default]
    Selecting,
    Countdown,
    Capturing,
    Paused,
    Review,
    Cancelled,
}
pub struct ScrollReview {
    pub image: RgbaImage,
    pub accepted_segments: u32,
    pub partial_reason: Option<&'static str>,
}
pub enum ScrollInput {
    Select {
        options: ScrollOptions,
        identity: TargetIdentity,
        now: Duration,
    },
    Tick(Duration),
    Frame {
        image: RgbaImage,
        identity: TargetIdentity,
        now: Duration,
    },
    Observe(TargetIdentity),
    Resume(TargetIdentity),
    Pause,
    Stop,
    Cancel,
    Interrupt(&'static str),
}
pub enum ScrollEffect {
    HideSelection,
    StartCandidates(ScrollOptions),
    StopCandidates,
    Review,
    Error(AppError),
}
#[derive(Default)]
pub struct ScrollingSession {
    state: ScrollState,
    options: Option<ScrollOptions>,
    identity: Option<TargetIdentity>,
    deadline: Duration,
    start: Duration,
    stable: StableFrames,
    last: Option<RgbaImage>,
    stitcher: Option<Stitcher>,
    segments: u32,
    reason: Option<&'static str>,
    review: Option<ScrollReview>,
}
impl ScrollingSession {
    pub fn state(&self) -> ScrollState {
        self.state
    }
    pub fn accepted_segments(&self) -> u32 {
        self.segments
    }
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        self.stitcher
            .as_ref()
            .map(Stitcher::dimensions)
            .or_else(|| self.review.as_ref().map(|r| r.image.dimensions()))
    }
    pub fn partial_reason(&self) -> Option<&'static str> {
        self.reason
    }
    pub fn take_review(&mut self) -> Option<ScrollReview> {
        self.review.take()
    }
    fn valid(&self, id: &TargetIdentity) -> bool {
        self.identity.as_ref() == Some(id) && id.focused
    }
    fn pause(&mut self, reason: &'static str) -> Vec<ScrollEffect> {
        self.stable.clear();
        self.reason.get_or_insert(reason);
        self.state = ScrollState::Paused;
        vec![ScrollEffect::StopCandidates]
    }
    fn stop(&mut self) -> Vec<ScrollEffect> {
        self.stable.clear();
        self.last = None;
        let mut out = vec![ScrollEffect::StopCandidates];
        if let Some(s) = self.stitcher.take() {
            match s.finish() {
                Ok(image) => {
                    self.review = Some(ScrollReview {
                        image,
                        accepted_segments: self.segments,
                        partial_reason: self.reason,
                    });
                    self.state = ScrollState::Review;
                    out.push(ScrollEffect::Review);
                }
                Err(e) => {
                    self.state = ScrollState::Cancelled;
                    out.push(ScrollEffect::Error(e));
                }
            }
        } else {
            self.state = ScrollState::Cancelled;
        }
        out
    }
    pub fn handle(&mut self, input: ScrollInput) -> Vec<ScrollEffect> {
        match input {
            ScrollInput::Select {
                options,
                identity,
                now,
            } if self.state == ScrollState::Selecting => {
                if let Err(e) = options.validate() {
                    return vec![ScrollEffect::Error(e)];
                }
                if !identity.focused
                    || identity.target != options.target
                    || identity.monitor_id != options.monitor_id
                    || identity.viewport != options.rect
                    || !identity.window_rect.contains_rect(options.rect)
                {
                    return vec![ScrollEffect::Error(AppError::Capture(
                        "scroll target or viewport is invalid",
                    ))];
                }
                self.deadline = now.saturating_add(Duration::from_secs(3));
                self.options = Some(options);
                self.identity = Some(identity);
                self.state = ScrollState::Countdown;
                vec![ScrollEffect::HideSelection]
            }
            ScrollInput::Tick(now) => {
                if self.state == ScrollState::Countdown && now >= self.deadline {
                    self.start = now;
                    self.state = ScrollState::Capturing;
                    return vec![ScrollEffect::StartCandidates(
                        self.options.clone().expect("selected options"),
                    )];
                }
                if matches!(self.state, ScrollState::Capturing | ScrollState::Paused)
                    && self
                        .options
                        .as_ref()
                        .is_some_and(|o| now.saturating_sub(self.start) >= o.maximum_duration)
                {
                    self.reason.get_or_insert("Duration limit reached");
                    return self.stop();
                }
                vec![]
            }
            ScrollInput::Observe(id)
                if matches!(self.state, ScrollState::Countdown | ScrollState::Capturing)
                    && !self.valid(&id) =>
            {
                self.pause("The window lost focus, moved or was resized")
            }
            ScrollInput::Resume(id) if self.state == ScrollState::Paused && self.valid(&id) => {
                self.stable.clear();
                // Frames after a resume must overlap the last accepted one, so
                // the result stays continuous: the pause no longer makes it partial.
                self.reason = None;
                if self.start.is_zero() {
                    self.state = ScrollState::Countdown;
                    vec![]
                } else {
                    self.state = ScrollState::Capturing;
                    vec![ScrollEffect::StartCandidates(
                        self.options.clone().expect("selected options"),
                    )]
                }
            }
            ScrollInput::Frame {
                image,
                identity,
                now,
            } if self.state == ScrollState::Capturing => {
                let effects = self.handle(ScrollInput::Tick(now));
                if self.state != ScrollState::Capturing {
                    return effects;
                }
                if !self.valid(&identity) {
                    return self.pause("The window lost focus, moved or was resized");
                }
                let o = self.options.as_ref().expect("selected options");
                if super::validate_image(&image).is_err() {
                    return self.pause("Invalid source image layout; prefix retained");
                }
                if image.dimensions() != (o.rect.width(), o.rect.height()) {
                    return self.pause("Viewport dimensions changed");
                }
                let Some(image) = self.stable.observe(image, now) else {
                    return vec![];
                };
                if let Some(previous) = &self.last {
                    match match_vertical_overlap(previous, &image) {
                        Ok(OverlapResult::Identical) => return vec![],
                        Ok(OverlapResult::Matched { new_rows }) => {
                            if self
                                .stitcher
                                .as_mut()
                                .expect("accepted prefix")
                                .append(&image, new_rows)
                                .is_err()
                            {
                                self.reason.get_or_insert(
                                    "Output pixel limit or allocation limit reached",
                                );
                                return self.stop();
                            }
                        }
                        _ => {
                            return self.pause("Scrolled too far at once to stitch");
                        }
                    }
                } else {
                    let mut bytes = Vec::new();
                    if bytes.try_reserve_exact(image.as_raw().len()).is_err() {
                        return self.pause("First viewport allocation failed");
                    }
                    bytes.extend_from_slice(image.as_raw());
                    let Some(first) = RgbaImage::from_raw(image.width(), image.height(), bytes)
                    else {
                        return self.pause("Invalid first viewport layout");
                    };
                    match Stitcher::new(first, MAX_CAPTURE_PIXELS) {
                        Ok(s) => self.stitcher = Some(s),
                        Err(_) => return self.pause("First viewport exceeds output limits"),
                    }
                }
                self.last = Some(image);
                self.segments += 1;
                if self.segments >= o.maximum_segments {
                    self.reason.get_or_insert("Segment limit reached");
                    return self.stop();
                }
                vec![]
            }
            ScrollInput::Pause if self.state == ScrollState::Capturing => {
                // Asked for (or a focus change): nothing was lost, so no reason.
                self.stable.clear();
                self.state = ScrollState::Paused;
                vec![ScrollEffect::StopCandidates]
            }
            ScrollInput::Interrupt(reason)
                if matches!(
                    self.state,
                    ScrollState::Selecting
                        | ScrollState::Countdown
                        | ScrollState::Capturing
                        | ScrollState::Paused
                ) =>
            {
                self.reason.get_or_insert(reason);
                self.stop()
            }
            ScrollInput::Stop
                if matches!(
                    self.state,
                    ScrollState::Countdown | ScrollState::Capturing | ScrollState::Paused
                ) =>
            {
                self.stop()
            }
            ScrollInput::Cancel => {
                self.stable.clear();
                self.last = None;
                self.stitcher = None;
                self.review = None;
                self.state = ScrollState::Cancelled;
                vec![ScrollEffect::StopCandidates]
            }
            _ => vec![],
        }
    }
}
