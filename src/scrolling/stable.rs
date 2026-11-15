//! Capacity-two candidate queue and pixel-free stable-observation evidence.
use image::RgbaImage;
use sha2::{Digest, Sha256};
use std::{collections::VecDeque, time::Duration};
pub struct CandidateQueue<T> {
    frames: VecDeque<T>,
}
impl<T> Default for CandidateQueue<T> {
    fn default() -> Self {
        Self {
            frames: VecDeque::with_capacity(2),
        }
    }
}
impl<T> CandidateQueue<T> {
    /// Called before native readback allocates, so there is never a third queued buffer.
    pub fn reserve_latest(&mut self) {
        if self.frames.len() == 2 {
            self.frames.pop_front();
        }
    }
    pub fn push(&mut self, item: T) {
        self.reserve_latest();
        self.frames.push_back(item);
    }
    pub fn pop(&mut self) -> Option<T> {
        self.frames.pop_front()
    }
    pub fn len(&self) -> usize {
        self.frames.len()
    }
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
    pub fn clear(&mut self) {
        self.frames.clear();
    }
}
#[derive(Default)]
pub struct StableFrames {
    anchor: Option<(Duration, [u8; 32], (u32, u32))>,
    last_sample: Option<Duration>,
}
impl StableFrames {
    pub fn clear(&mut self) {
        self.anchor = None;
        self.last_sample = None;
    }
    pub fn observe(&mut self, image: RgbaImage, now: Duration) -> Option<RgbaImage> {
        if self.last_sample.is_some_and(|last| now < last) {
            self.clear();
            return None;
        }
        if self
            .last_sample
            .is_some_and(|last| now - last < Duration::from_millis(100))
        {
            return None;
        }
        self.last_sample = Some(now);
        let digest: [u8; 32] = Sha256::digest(image.as_raw()).into();
        let size = image.dimensions();
        if let Some((since, previous, dimensions)) = &self.anchor
            && *previous == digest
            && *dimensions == size
        {
            if now - *since >= Duration::from_millis(150) {
                self.anchor = None;
                return Some(image);
            }
            return None;
        }
        self.anchor = Some((now, digest, size));
        None
    }
}
