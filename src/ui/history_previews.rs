//! One bounded decoder worker, generation-keyed results, and a page-sized texture cache.
use crate::persistence::history::browser::{HistoryRecord, verified_local};
use eframe::egui::{self, ColorImage, TextureHandle};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::Duration,
};
use uuid::Uuid;
struct Request {
    generation: u64,
    records: Vec<HistoryRecord>,
}
struct ResultImage {
    generation: u64,
    id: Uuid,
    image: Option<ColorImage>,
}
pub struct HistoryPreviews {
    requests: Option<SyncSender<Request>>,
    results: Receiver<ResultImage>,
    generation: Arc<AtomicU64>,
    signature: Option<u64>,
    pub images: HashMap<Uuid, Option<TextureHandle>>,
    remaining: usize,
}
impl Default for HistoryPreviews {
    fn default() -> Self {
        let (tx, rx) = mpsc::sync_channel::<Request>(1);
        let (output, results) = mpsc::sync_channel(4);
        let generation = Arc::new(AtomicU64::new(0));
        let check = generation.clone();
        let spawned = std::thread::Builder::new()
            .name("history-preview".into())
            .spawn(move || {
                while let Ok(request) = rx.recv() {
                    for record in request.records {
                        if check.load(Ordering::Acquire) != request.generation {
                            break;
                        }
                        let image = if record.local_available {
                            decode(&record)
                        } else {
                            None
                        };
                        if check.load(Ordering::Acquire) != request.generation {
                            break;
                        }
                        if output
                            .send(ResultImage {
                                generation: request.generation,
                                id: record.id,
                                image,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            })
            .is_ok();
        Self {
            requests: spawned.then_some(tx),
            results,
            generation,
            signature: None,
            images: HashMap::new(),
            remaining: 0,
        }
    }
}
impl HistoryPreviews {
    pub fn suspend(&mut self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.signature = None;
        self.images.clear();
        self.remaining = 0;
        while self.results.try_recv().is_ok() {}
    }
    pub fn update(&mut self, ctx: &egui::Context, records: &[HistoryRecord]) {
        let mut hash = std::hash::DefaultHasher::new();
        for r in records {
            r.id.hash(&mut hash);
            r.artifact.sha256.hash(&mut hash);
            r.artifact.path.hash(&mut hash);
            r.local_available.hash(&mut hash);
        }
        let signature = hash.finish();
        if self.requests.is_none() {
            self.signature = Some(signature);
            self.remaining = 0;
            self.images.extend(records.iter().map(|r| (r.id, None)));
            return;
        }
        if self.signature != Some(signature) {
            self.generation.fetch_add(1, Ordering::AcqRel);
            self.images.clear();
            self.remaining = 0;
            let generation = self.generation.load(Ordering::Acquire);
            if let Some(tx) = &self.requests
                && tx
                    .try_send(Request {
                        generation,
                        records: records.to_vec(),
                    })
                    .is_ok()
            {
                self.signature = Some(signature);
                self.remaining = records.len();
            }
        }
        while let Ok(result) = self.results.try_recv() {
            if result.generation == self.generation.load(Ordering::Acquire)
                && records.iter().any(|r| r.id == result.id)
            {
                self.remaining = self.remaining.saturating_sub(1);
                self.images.insert(
                    result.id,
                    result.image.map(|image| {
                        ctx.load_texture(
                            format!("history-{}", result.id),
                            image,
                            egui::TextureOptions::LINEAR,
                        )
                    }),
                );
            }
        }
        if self.remaining > 0 || self.signature != Some(signature) {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}
impl Drop for HistoryPreviews {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }
}
pub fn decode(record: &HistoryRecord) -> Option<ColorImage> {
    decode_artifact(&record.name, &record.kind, &record.artifact)
}

/// Verified local preview. Callers must not run this on the UI thread.
pub fn decode_artifact(
    name: &str,
    kind: &str,
    artifact: &crate::app::types::Artifact,
) -> Option<ColorImage> {
    let extension = std::path::Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(kind, "screenshot" | "gif" | "mp4")
        && !matches!(
            extension.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "mp4"
        )
    {
        return None;
    }
    let file = verified_local(artifact, 64 * 1024 * 1024).ok()?;
    if kind == "mp4" || name.to_ascii_lowercase().ends_with(".mp4") {
        return crate::platform::windows::poster::mp4_poster(file);
    }
    let mut reader = image::ImageReader::new(std::io::BufReader::new(file))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode().ok()?;
    let image = decoded
        .thumbnail(256.min(decoded.width()), 256.min(decoded.height()))
        .to_rgba8();
    Some(ColorImage::from_rgba_unmultiplied(
        [image.width() as usize, image.height() as usize],
        image.as_raw(),
    ))
}
