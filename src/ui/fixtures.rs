//! Sample History/Settings data for UI tests. Never used by the running app.
use super::settings::SettingsCategory;
use crate::{
    app::{
        controller::BatchSummaryRow,
        desktop::DesktopView,
        destinations::{CredentialKey, DestinationSelection, DestinationSnapshot},
        events::{HistoryFocus, Window},
        types::{Artifact, ArtifactId, ArtifactKind, JobId, UploadReceipt},
    },
    persistence::{
        history::{
            JobStatus, UploadJob,
            browser::{HistoryPage, HistoryRecord},
        },
        settings::{ServerOrigin, Settings, StorageMode},
    },
    workflow::{
        profile::{CaptureTask, EffectiveProfile},
        runner::{StepResult, TaskEffect, TaskInput, TaskIntent, TaskRunner, WorkflowContext},
        steps::TaskStep,
    },
};
use std::{path::PathBuf, sync::Arc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockupVariant {
    HistoryBusy,
    HistoryEmpty,
    HistoryPaused,
    Settings(SettingsCategory),
}

/// Where `write_mockup_media` puts the sample images the fixture records point at.
pub fn media_dir() -> PathBuf {
    std::env::temp_dir().join("vectrcap-mockup")
}

/// Writes small synthetic images for the fixture's local files so previews load.
pub fn write_mockup_media() -> std::io::Result<()> {
    let dir = media_dir();
    std::fs::create_dir_all(&dir)?;
    for (i, sample) in SAMPLES.iter().enumerate() {
        if let Some(bytes) = sample_media(i, sample) {
            std::fs::write(dir.join(sample.name), bytes)?;
        }
    }
    Ok(())
}

/// A deterministic PNG padded to the sample's listed size, so the preview
/// loader's size and SHA-256 checks pass against `mockup_view`'s artifact.
fn sample_media(i: usize, sample: &Sample) -> Option<Vec<u8>> {
    if sample.kind != "screenshot" || !sample.local {
        return None;
    }
    let mut image = image::RgbaImage::new(320, 180);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let panel = (16..304).contains(&x) && (13..167).contains(&y);
        let shade = if panel { 23 } else { 18 };
        let accent = i as u32 * 29 % 120;
        *pixel = if panel && y > 40 && y % 14 < 3 && x < 120 + accent {
            image::Rgba([110, 110, 110, 255])
        } else {
            image::Rgba([shade, shade, shade, 255])
        };
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).ok()?;
    let mut bytes = bytes.into_inner();
    // PNG decoders stop at IEND; the zero tail only sets the file size.
    bytes.resize(bytes.len().max(sample.size as usize), 0);
    Some(bytes)
}

fn sample_sha256(i: usize, sample: &Sample) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    sample_media(i, sample).map_or([7; 32], |bytes| Sha256::digest(bytes).into())
}

struct Sample {
    name: &'static str,
    kind: &'static str,
    size: u64,
    minutes_ago: i64,
    favorite: bool,
    local: bool,
    status: Status,
}
#[derive(Clone, Copy)]
enum Status {
    Uploaded,
    Local,
    Uploading,
    Failed,
}
const SAMPLES: [Sample; 9] = [
    Sample {
        name: "capture_2026-11-17_2304.png",
        kind: "screenshot",
        size: 248_000,
        minutes_ago: 0,
        favorite: true,
        local: true,
        status: Status::Uploaded,
    },
    Sample {
        name: "terminal_build_output.png",
        kind: "screenshot",
        size: 94_000,
        minutes_ago: 46,
        favorite: false,
        local: true,
        status: Status::Uploaded,
    },
    Sample {
        name: "annotated_navigation.png",
        kind: "screenshot",
        size: 186_000,
        minutes_ago: 58,
        favorite: false,
        local: true,
        status: Status::Local,
    },
    Sample {
        name: "recording_2026-11-17.mp4",
        kind: "mp4",
        size: 3_800_000,
        minutes_ago: 70,
        favorite: false,
        local: true,
        status: Status::Uploaded,
    },
    Sample {
        name: "release_assets.zip",
        kind: "file",
        size: 24_600_000,
        minutes_ago: 83,
        favorite: false,
        local: true,
        status: Status::Uploading,
    },
    Sample {
        name: "scrolling_documentation.png",
        kind: "screenshot",
        size: 1_200_000,
        minutes_ago: 1_489,
        favorite: false,
        local: true,
        status: Status::Failed,
    },
    Sample {
        name: "menu_walkthrough.gif",
        kind: "gif",
        size: 2_100_000,
        minutes_ago: 1_516,
        favorite: true,
        local: true,
        status: Status::Uploaded,
    },
    Sample {
        name: "project_notes.pdf",
        kind: "file",
        size: 412_000,
        minutes_ago: 1_592,
        favorite: false,
        local: false,
        status: Status::Uploaded,
    },
    Sample {
        name: "capture_2026-11-16_1944.png",
        kind: "screenshot",
        size: 172_000,
        minutes_ago: 1_640,
        favorite: false,
        local: false,
        status: Status::Uploaded,
    },
];
fn artifact_kind(kind: &str) -> ArtifactKind {
    match kind {
        "gif" => ArtifactKind::Gif,
        "mp4" => ArtifactKind::Mp4,
        "file" => ArtifactKind::File,
        _ => ArtifactKind::Screenshot,
    }
}

pub fn mockup_view(variant: MockupVariant) -> DesktopView {
    let mut settings = Settings::default();
    let work = settings
        .destinations
        .add(
            "Vectr (work)",
            ServerOrigin::parse("https://files.vectr.example").expect("fixture origin"),
            None,
        )
        .expect("fixture destination");
    settings
        .destinations
        .bind_credential(work, &Default::default())
        .expect("fixture credential");
    settings
        .destinations
        .add(
            "Personal",
            ServerOrigin::parse("https://vectr.home.example").expect("fixture origin"),
            None,
        )
        .expect("fixture destination");
    settings
        .destinations
        .set_default(work)
        .expect("fixture default");
    let origin = ServerOrigin::parse("https://files.vectr.example").expect("fixture origin");
    let snapshot = DestinationSnapshot {
        id: work,
        label: "Vectr (work)".into(),
        origin: origin.clone(),
        credential: CredentialKey::new(work, origin, 1),
        link_domain: None,
    };
    let busy = variant != MockupVariant::HistoryEmpty;
    let dir = media_dir();
    let mut jobs = vec![];
    let mut records = vec![];
    for (i, sample) in SAMPLES.iter().enumerate() {
        let artifact = Artifact {
            id: ArtifactId::new_random(),
            path: dir.join(sample.name),
            display_name: sample.name.into(),
            kind: artifact_kind(sample.kind),
            size: sample.size,
            sha256: sample_sha256(i, sample),
        };
        let status = match (sample.status, busy) {
            (Status::Local, _) => None,
            (Status::Uploading, true) => Some(JobStatus::Running),
            (Status::Failed, true) => Some(JobStatus::Failed {
                code: "transient".into(),
            }),
            _ => Some(JobStatus::Completed(
                UploadReceipt::new(
                    &format!("https://files.vectr.example/i/{}", &sample.name[..4]),
                    sample.name,
                )
                .expect("fixture receipt"),
            )),
        };
        let job = status.map(|status| UploadJob {
            id: JobId::new_random(),
            artifact: artifact.clone(),
            destination: snapshot.clone(),
            session: None,
            status,
            paused: false,
            attempt: 1,
        });
        if let Some(job) = &job {
            jobs.push(job.clone());
        }
        records.push(HistoryRecord {
            recording: None,
            id: uuid::Uuid::new_v4(),
            name: sample.name.into(),
            kind: sample.kind.into(),
            size: sample.size,
            created_ms: chrono::Utc::now().timestamp_millis() - sample.minutes_ago * 60_000,
            favorite: sample.favorite,
            local_available: sample.local,
            retryable: matches!(sample.status, Status::Failed) && busy,
            artifact,
            job,
        });
    }
    let mut view = DesktopView {
        window: Some(match variant {
            MockupVariant::Settings(_) => Window::Settings,
            _ => Window::History,
        }),
        queue_paused: variant == MockupVariant::HistoryPaused,
        notifications_enabled: true,
        ..Default::default()
    };
    if busy {
        let uploading = jobs
            .iter()
            .find(|j| matches!(j.status, JobStatus::Running))
            .map(|j| j.id);
        if let Some(id) = uploading {
            view.transfers.insert(id, (15_300_000, Some(2_100_000.0)));
        }
        let waiting = UploadJob {
            id: JobId::new_random(),
            artifact: Artifact {
                id: ArtifactId::new_random(),
                path: dir.join("meeting_notes.pdf"),
                display_name: "meeting_notes.pdf".into(),
                kind: ArtifactKind::File,
                size: 380_000,
                sha256: [7; 32],
            },
            destination: snapshot,
            session: None,
            status: JobStatus::Pending,
            paused: false,
            attempt: 0,
        };
        view.batch_summary = jobs
            .iter()
            .filter(|j| j.artifact.kind == ArtifactKind::File)
            .chain(std::iter::once(&waiting))
            .map(|j| BatchSummaryRow {
                id: j.id,
                job: Some(j.clone()),
                local_available: true,
            })
            .collect();
        jobs.push(waiting);
        let first = Arc::new(records[0].artifact.clone());
        // No automatic saving, so every step of the finished run can succeed.
        let mut unsaved = settings.clone();
        unsaved.archive.storage_mode = StorageMode::NeverAutomatic;
        view.task_workflows = vec![
            needs_attention(&settings),
            run_workflow(&unsaved, TaskIntent::Direct(CaptureTask::CopyImage), first),
        ];
        // The mockup opens with the first card selected.
        view.history_focus = Some(HistoryFocus::Artifact(records[0].artifact.id));
    }
    view.jobs = jobs;
    view.history_page = HistoryPage {
        records,
        has_more: false,
    };
    view.settings = Some(settings);
    view
}

/// A Copy file task with no saved copy stops at NeedsUserAction (choose where to save).
fn needs_attention(settings: &Settings) -> TaskRunner {
    let mut settings = settings.clone();
    settings.archive.storage_mode = StorageMode::NeverAutomatic;
    let artifact = Arc::new(Artifact {
        id: ArtifactId::new_random(),
        path: media_dir().join("capture_2026-11-17_2310.png"),
        display_name: "capture_2026-11-17_2310.png".into(),
        kind: ArtifactKind::Screenshot,
        size: 210_000,
        sha256: [7; 32],
    });
    run_workflow(
        &settings,
        TaskIntent::Direct(CaptureTask::CopyFile),
        artifact,
    )
}

/// Runs a workflow for `artifact`, every executed step succeeding, until it
/// finishes or stops for the user.
fn run_workflow(settings: &Settings, intent: TaskIntent, artifact: Arc<Artifact>) -> TaskRunner {
    let mut runner = TaskRunner::new(WorkflowContext::new(
        artifact.kind,
        EffectiveProfile::snapshot(settings, None),
        intent,
        DestinationSelection::Inherit,
        &settings.destinations,
        None,
    ));
    let mut effects = runner.handle(TaskInput::Start);
    let mut guard = 0;
    while let Some(TaskEffect::Execute(step)) = effects.pop() {
        guard += 1;
        if guard > 40 {
            break;
        }
        let result = match step {
            TaskStep::Finalize => StepResult::Finalized(artifact.clone()),
            _ => StepResult::Succeeded,
        };
        effects.extend(runner.handle(TaskInput::Completed { step, result }));
    }
    runner
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::history::JobStatus;
    use crate::workflow::steps::StepOutcome;

    #[test]
    fn busy_fixture_has_every_in_progress_kind() {
        let v = mockup_view(MockupVariant::HistoryBusy);
        assert!(v.jobs.iter().any(|j| !j.status.is_terminal() && !j.paused));
        assert!(
            v.jobs
                .iter()
                .any(|j| matches!(j.status, JobStatus::Failed { .. }))
        );
        assert!(v.task_workflows.iter().any(|r| {
            r.states()
                .iter()
                .any(|s| matches!(s.outcome, StepOutcome::NeedsUserAction(_)))
        }));
        assert!(!v.batch_summary.is_empty());
    }

    #[test]
    fn empty_fixture_has_nothing_in_progress() {
        let v = mockup_view(MockupVariant::HistoryEmpty);
        assert!(v.jobs.iter().all(|j| j.status.is_terminal()));
        assert!(v.task_workflows.is_empty());
        assert!(v.pending_ingress.is_empty());
    }
}
