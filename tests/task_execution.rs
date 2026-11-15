use std::{path::PathBuf, sync::Arc};
use vectrcap::{
    app::{destinations::*, types::*},
    persistence::{
        history::History,
        settings::{ServerOrigin, Settings, StorageMode},
    },
    workflow::{profile::*, runner::*, steps::*},
};

fn artifact() -> Arc<Artifact> {
    Arc::new(Artifact {
        id: ArtifactId::new_random(),
        path: PathBuf::from("C:/spool/final.png"),
        display_name: "final.png".into(),
        kind: ArtifactKind::Screenshot,
        size: 4,
        sha256: [1; 32],
    })
}
fn settings(upload: bool) -> Settings {
    let mut s = Settings::default();
    s.tasks.global.capture.remove(&CaptureTask::QuickMenu);
    if upload {
        s.tasks.global.capture.insert(CaptureTask::Upload);
    }
    let id = s
        .destinations
        .add(
            "one",
            ServerOrigin::parse("https://one.test").unwrap(),
            None,
        )
        .unwrap();
    s.destinations
        .bind_credential(id, &Default::default())
        .unwrap();
    s
}
fn runner(s: &Settings, intent: TaskIntent) -> TaskRunner {
    TaskRunner::new(WorkflowContext::new(
        ArtifactKind::Screenshot,
        EffectiveProfile::snapshot(s, None),
        intent,
        DestinationSelection::Inherit,
        &s.destinations,
        None,
    ))
}
fn receipt() -> UploadReceipt {
    UploadReceipt::new("https://one.test/link", "final.png").unwrap()
}
fn success(step: TaskStep) -> StepResult {
    match step {
        TaskStep::Finalize => StepResult::Finalized(artifact()),
        TaskStep::Capture(CaptureTask::SaveFinal | CaptureTask::SaveAs) => {
            StepResult::Durable(PathBuf::from("C:/archive/final.png"))
        }
        TaskStep::Capture(CaptureTask::Upload) => StepResult::Receipt(receipt()),
        _ => StepResult::Succeeded,
    }
}
fn drive(r: &mut TaskRunner, first: TaskInput) -> Vec<TaskStep> {
    let mut effects = r.handle(first);
    let mut log = vec![];
    while let Some(effect) = effects.pop() {
        let TaskEffect::Execute(step) = effect;
        log.push(step);
        effects.extend(r.handle(TaskInput::Completed {
            step,
            result: success(step),
        }));
        assert!(log.len() < 40);
    }
    log
}
#[test]
fn review_finishes_before_any_publish() {
    let mut r = runner(&settings(true), TaskIntent::RunSelectedTasks);
    assert_eq!(
        r.handle(TaskInput::Start),
        vec![TaskEffect::Execute(TaskStep::Capture(CaptureTask::Review))]
    );
    assert!(r.context().artifact.is_none());
    let log = drive(
        &mut r,
        TaskInput::Completed {
            step: TaskStep::Capture(CaptureTask::Review),
            result: StepResult::Succeeded,
        },
    );
    assert_eq!(log[0], TaskStep::Finalize);
    assert!(
        log.iter()
            .position(|s| *s == TaskStep::Capture(CaptureTask::SaveFinal))
            .unwrap()
            < log
                .iter()
                .position(|s| *s == TaskStep::Capture(CaptureTask::Upload))
                .unwrap()
    );
}
#[test]
fn one_final_artifact_shared_across_tasks() {
    let mut s = settings(true);
    s.tasks
        .global
        .capture
        .extend([CaptureTask::CopyImage, CaptureTask::Pin]);
    let mut r = runner(&s, TaskIntent::RunSelectedTasks);
    let finalized = artifact();
    let mut effects = r.handle(TaskInput::Start);
    let mut log = vec![];
    while let Some(TaskEffect::Execute(step)) = effects.pop() {
        log.push(step);
        if !matches!(
            step,
            TaskStep::Finalize | TaskStep::Capture(CaptureTask::Review)
        ) {
            assert!(Arc::ptr_eq(
                r.context().artifact.as_ref().unwrap(),
                &finalized
            ));
        }
        let result = if step == TaskStep::Finalize {
            StepResult::Finalized(finalized.clone())
        } else {
            success(step)
        };
        effects.extend(r.handle(TaskInput::Completed { step, result }));
    }
    assert_eq!(log.iter().filter(|s| **s == TaskStep::Finalize).count(), 1);
    assert_eq!(
        log.iter()
            .filter(|s| **s == TaskStep::Capture(CaptureTask::SaveFinal))
            .count(),
        1
    );
    assert_eq!(
        log.iter()
            .filter(|s| **s == TaskStep::Capture(CaptureTask::Upload))
            .count(),
        1
    );
    assert_eq!(r.context().artifact.as_ref().unwrap().id, finalized.id);
    assert!(r.handle(TaskInput::Start).is_empty());
}
#[test]
fn direct_copy_does_not_run_profile_upload() {
    let mut r = runner(&settings(true), TaskIntent::Direct(CaptureTask::CopyImage));
    let log = drive(&mut r, TaskInput::Start);
    assert_eq!(
        log,
        vec![
            TaskStep::Finalize,
            TaskStep::Capture(CaptureTask::SaveFinal),
            TaskStep::Capture(CaptureTask::CopyImage)
        ]
    );
}
#[test]
fn run_selected_tasks_honors_explicit_upload_opt_in() {
    for enabled in [false, true] {
        let mut r = runner(&settings(enabled), TaskIntent::RunSelectedTasks);
        let log = drive(&mut r, TaskInput::Start);
        assert_eq!(
            log.contains(&TaskStep::Capture(CaptureTask::Upload)),
            enabled
        );
    }
}
#[test]
fn quick_menu_does_not_recurse() {
    let mut s = settings(true);
    s.tasks.global.capture.insert(CaptureTask::QuickMenu);
    let mut r = runner(&s, TaskIntent::RunSelectedTasks);
    assert_eq!(
        r.handle(TaskInput::Start),
        vec![TaskEffect::Execute(TaskStep::Capture(
            CaptureTask::QuickMenu
        ))]
    );
    let log = drive(&mut r, TaskInput::Choose(TaskIntent::RunSelectedTasks));
    assert!(!log.contains(&TaskStep::Capture(CaptureTask::QuickMenu)));
}
#[test]
fn failed_archive_pauses_dependent_upload() {
    let mut r = runner(&settings(true), TaskIntent::Direct(CaptureTask::Upload));
    r.handle(TaskInput::Start);
    r.handle(TaskInput::Completed {
        step: TaskStep::Finalize,
        result: success(TaskStep::Finalize),
    });
    assert!(
        r.handle(TaskInput::Completed {
            step: TaskStep::Capture(CaptureTask::SaveFinal),
            result: StepResult::Failed("archive_failed".into())
        })
        .is_empty()
    );
    assert!(matches!(
        r.outcome(TaskStep::Capture(CaptureTask::SaveFinal)),
        Some(StepOutcome::Failed(_))
    ));
    let log = drive(
        &mut r,
        TaskInput::Retry(TaskStep::Capture(CaptureTask::SaveFinal)),
    );
    assert_eq!(
        log.iter()
            .filter(|s| **s == TaskStep::Capture(CaptureTask::Upload))
            .count(),
        1
    );
}
#[test]
fn file_clipboard_requires_durable_path() {
    let mut s = settings(false);
    s.archive.storage_mode = StorageMode::NeverAutomatic;
    let mut r = runner(&s, TaskIntent::Direct(CaptureTask::CopyFile));
    let log = drive(&mut r, TaskInput::Start);
    assert!(!log.contains(&TaskStep::Capture(CaptureTask::CopyFile)));
    assert!(matches!(
        r.outcome(TaskStep::Capture(CaptureTask::CopyFile)),
        Some(StepOutcome::NeedsUserAction(_))
    ));
    let log = drive(
        &mut r,
        TaskInput::ProvideDurable(PathBuf::from("C:/saved/final.png")),
    );
    assert_eq!(log, vec![TaskStep::Capture(CaptureTask::CopyFile)]);
}
#[test]
fn after_upload_requires_confirmed_receipt() {
    let mut r = runner(&settings(true), TaskIntent::Direct(CaptureTask::Upload));
    r.handle(TaskInput::Start);
    r.handle(TaskInput::Completed {
        step: TaskStep::Finalize,
        result: success(TaskStep::Finalize),
    });
    r.handle(TaskInput::Completed {
        step: TaskStep::Capture(CaptureTask::SaveFinal),
        result: success(TaskStep::Capture(CaptureTask::SaveFinal)),
    });
    assert!(
        r.handle(TaskInput::Completed {
            step: TaskStep::Capture(CaptureTask::Upload),
            result: StepResult::Succeeded
        })
        .is_empty()
    );
    assert!(r.context().receipt.is_none());
    assert!(
        r.handle(TaskInput::Completed {
            step: TaskStep::AfterUpload(UploadTask::CopyUrl),
            result: StepResult::Succeeded
        })
        .is_empty()
    );
}
#[test]
fn copy_failure_never_reuploads() {
    let mut r = runner(&settings(true), TaskIntent::Direct(CaptureTask::Upload));
    drive(&mut r, TaskInput::Start);
    // A completed step cannot be overwritten by stale failure events.
    assert!(
        r.handle(TaskInput::Completed {
            step: TaskStep::AfterUpload(UploadTask::CopyUrl),
            result: StepResult::Failed("clipboard_busy".into())
        })
        .is_empty()
    );
    let mut r = runner(&settings(true), TaskIntent::Direct(CaptureTask::Upload));
    let mut effects = r.handle(TaskInput::Start);
    let mut log = vec![];
    while let Some(TaskEffect::Execute(step)) = effects.pop() {
        log.push(step);
        let result = if step == TaskStep::AfterUpload(UploadTask::CopyUrl) {
            StepResult::Failed("clipboard_busy".into())
        } else {
            success(step)
        };
        effects.extend(r.handle(TaskInput::Completed { step, result }));
    }
    assert!(r.context().receipt.is_some());
    assert_eq!(
        drive(
            &mut r,
            TaskInput::Retry(TaskStep::AfterUpload(UploadTask::CopyUrl))
        ),
        vec![TaskStep::AfterUpload(UploadTask::CopyUrl)]
    );
}
#[test]
fn restart_does_not_reopen_browser_automatically() {
    let mut s = settings(true);
    s.tasks.global.after_upload.insert(UploadTask::OpenUrl);
    let mut r = runner(&s, TaskIntent::Direct(CaptureTask::Upload));
    let mut effects = r.handle(TaskInput::Start);
    while let Some(TaskEffect::Execute(step)) = effects.pop() {
        if step == TaskStep::AfterUpload(UploadTask::OpenUrl) {
            break;
        }
        effects.extend(r.handle(TaskInput::Completed {
            step,
            result: success(step),
        }));
    }
    let temp = tempfile::tempdir().unwrap();
    let h = History::open(&temp.path().join("history.db")).unwrap();
    h.save_task_workflow(&r).unwrap();
    let mut recovered = h.task_workflow(r.context().id).unwrap().unwrap();
    assert!(recovered.handle(TaskInput::Resume).is_empty());
    assert!(matches!(
        recovered.outcome(TaskStep::AfterUpload(UploadTask::OpenUrl)),
        Some(StepOutcome::NeedsUserAction(_))
    ));
    assert_eq!(
        drive(
            &mut recovered,
            TaskInput::Retry(TaskStep::AfterUpload(UploadTask::OpenUrl))
        ),
        vec![TaskStep::AfterUpload(UploadTask::OpenUrl)]
    );
}
#[test]
fn cancelled_review_preserves_archive_and_stops_downstream() {
    let mut r = runner(&settings(true), TaskIntent::RunSelectedTasks);
    let path = PathBuf::from("C:/archive/prior.png");
    r.set_durable(path.clone());
    r.handle(TaskInput::Start);
    assert!(r.handle(TaskInput::Cancel).is_empty());
    assert_eq!(r.context().durable_path.as_ref(), Some(&path));
    assert!(r.handle(TaskInput::Start).is_empty());
    assert!(
        r.handle(TaskInput::Completed {
            step: TaskStep::Capture(CaptureTask::Review),
            result: StepResult::Succeeded
        })
        .is_empty()
    );
}
#[test]
fn task_destination_precedes_hotkey_then_default() {
    let mut s = settings(true);
    let first = s.destinations.default_id().unwrap();
    let second = s
        .destinations
        .add(
            "two",
            ServerOrigin::parse("https://two.test").unwrap(),
            None,
        )
        .unwrap();
    s.destinations
        .bind_credential(second, &Default::default())
        .unwrap();
    for (task, hotkey, want) in [
        (
            DestinationSelection::Specific(first),
            DestinationSelection::Specific(second),
            first,
        ),
        (
            DestinationSelection::Inherit,
            DestinationSelection::Specific(second),
            second,
        ),
        (
            DestinationSelection::Inherit,
            DestinationSelection::Inherit,
            first,
        ),
    ] {
        s.tasks.global.destination = task;
        let c = WorkflowContext::new(
            ArtifactKind::Screenshot,
            EffectiveProfile::snapshot(&s, None),
            TaskIntent::RunSelectedTasks,
            hotkey,
            &s.destinations,
            None,
        );
        assert_eq!(c.destination.unwrap().id, want);
    }
}
#[test]
fn destination_selection_does_not_enable_upload() {
    let mut s = settings(false);
    s.tasks.global.destination =
        DestinationSelection::Specific(s.destinations.default_id().unwrap());
    let mut r = runner(&s, TaskIntent::RunSelectedTasks);
    assert!(!drive(&mut r, TaskInput::Start).contains(&TaskStep::Capture(CaptureTask::Upload)));
}
#[test]
fn default_switch_during_review_keeps_target() {
    let mut s = settings(true);
    let old = s.destinations.default_id().unwrap();
    let mut r = runner(&s, TaskIntent::RunSelectedTasks);
    r.handle(TaskInput::Start);
    let new = s
        .destinations
        .add(
            "two",
            ServerOrigin::parse("https://two.test").unwrap(),
            None,
        )
        .unwrap();
    s.destinations.set_default(new).unwrap();
    drive(
        &mut r,
        TaskInput::Completed {
            step: TaskStep::Capture(CaptureTask::Review),
            result: StepResult::Succeeded,
        },
    );
    assert_eq!(r.context().destination.as_ref().unwrap().id, old);
}
#[test]
fn missing_destination_blocks_upload_not_local_finish() {
    let mut s = settings(true);
    s.destinations = Default::default();
    s.tasks.global.capture.insert(CaptureTask::CopyImage);
    let mut r = runner(&s, TaskIntent::RunSelectedTasks);
    let log = drive(&mut r, TaskInput::Start);
    assert!(log.contains(&TaskStep::Capture(CaptureTask::CopyImage)));
    assert!(!log.contains(&TaskStep::Capture(CaptureTask::Upload)));
    assert!(matches!(
        r.outcome(TaskStep::Capture(CaptureTask::Upload)),
        Some(StepOutcome::NeedsUserAction(_))
    ));
}
#[test]
fn removed_reference_never_falls_back_silently() {
    let mut s = settings(true);
    s.tasks.global.destination = DestinationSelection::Specific(DestinationId::new_random());
    let mut r = runner(&s, TaskIntent::RunSelectedTasks);
    assert!(r.context().destination.is_none());
    assert!(!drive(&mut r, TaskInput::Start).contains(&TaskStep::Capture(CaptureTask::Upload)));
}

#[test]
fn upload_admission_and_workflow_link_are_atomic_and_idempotent() {
    let s = settings(true);
    let mut r = runner(&s, TaskIntent::Direct(CaptureTask::Upload));
    r.handle(TaskInput::Start);
    r.handle(TaskInput::Completed {
        step: TaskStep::Finalize,
        result: success(TaskStep::Finalize),
    });
    r.handle(TaskInput::Completed {
        step: TaskStep::Capture(CaptureTask::SaveFinal),
        result: success(TaskStep::Capture(CaptureTask::SaveFinal)),
    });
    let temp = tempfile::tempdir().unwrap();
    let h = History::open(&temp.path().join("h.db")).unwrap();
    let first = h.admit_task_upload(&mut r).unwrap();
    let second = h.admit_task_upload(&mut r).unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(
        h.task_workflow(r.context().id)
            .unwrap()
            .unwrap()
            .context()
            .upload_job,
        Some(first.id)
    );
}
#[test]
fn skipping_upload_cancels_dependent_after_upload_only() {
    let mut s = settings(true);
    s.tasks.global.capture.insert(CaptureTask::CopyImage);
    s.destinations = Default::default();
    let mut r = runner(&s, TaskIntent::RunSelectedTasks);
    let log = drive(&mut r, TaskInput::Start);
    assert!(log.contains(&TaskStep::Capture(CaptureTask::CopyImage)));
    assert!(
        r.handle(TaskInput::Skip(TaskStep::Capture(CaptureTask::Upload)))
            .is_empty()
    );
    assert!(r.complete());
}
#[test]
fn explicit_direct_save_does_not_save_twice() {
    let mut r = runner(&settings(false), TaskIntent::Direct(CaptureTask::SaveAs));
    let log = drive(&mut r, TaskInput::Start);
    assert_eq!(
        log,
        vec![TaskStep::Finalize, TaskStep::Capture(CaptureTask::SaveAs)]
    );
}

#[test]
fn missing_credentials_retry_keeps_the_original_origin() {
    let mut s = settings(true);
    let id = s.destinations.default_id().unwrap();
    s.destinations
        .change_origin(
            id,
            ServerOrigin::parse("https://tokenless.test").unwrap(),
            &Default::default(),
        )
        .unwrap();
    let mut r = runner(&s, TaskIntent::Direct(CaptureTask::Upload));
    s.destinations
        .bind_credential(id, &Default::default())
        .unwrap();
    r.repair_destination(&s.destinations).unwrap();
    assert_eq!(
        r.context().destination.as_ref().unwrap().origin.as_str(),
        "https://tokenless.test"
    );
    let mut s = settings(true);
    let id = s.destinations.default_id().unwrap();
    s.destinations
        .change_origin(
            id,
            ServerOrigin::parse("https://tokenless.test").unwrap(),
            &Default::default(),
        )
        .unwrap();
    let mut r = runner(&s, TaskIntent::Direct(CaptureTask::Upload));
    s.destinations
        .change_origin(
            id,
            ServerOrigin::parse("https://changed.test").unwrap(),
            &Default::default(),
        )
        .unwrap();
    s.destinations
        .bind_credential(id, &Default::default())
        .unwrap();
    assert!(r.repair_destination(&s.destinations).is_err());
    assert!(r.context().destination.is_none());
}

#[test]
fn file_clipboard_uses_literal_utf16_paths_and_never_command_text() {
    let path = PathBuf::from("C:/saved/ä & $(data).png");
    let bytes = vectrcap::platform::windows::clipboard::file_drop_bytes(&path).unwrap();
    assert_eq!(&bytes[..4], &20u32.to_le_bytes());
    assert_eq!(&bytes[16..20], &1u32.to_le_bytes());
    let units: Vec<_> = bytes[20..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    assert_eq!(&units[units.len() - 2..], &[0, 0]);
    assert_eq!(
        String::from_utf16(&units[..units.len() - 2]).unwrap(),
        path.to_str().unwrap()
    );
    assert!(
        vectrcap::platform::windows::clipboard::file_drop_bytes(std::path::Path::new(
            "relative.png"
        ))
        .is_err()
    );
}

#[test]
fn corrupt_persisted_destination_binding_cannot_resume() {
    let mut r = runner(&settings(true), TaskIntent::Direct(CaptureTask::Upload));
    drive(&mut r, TaskInput::Start);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("history.db");
    let h = History::open(&path).unwrap();
    h.save_task_workflow(&r).unwrap();
    let mut value = serde_json::to_value(&r).unwrap();
    value["context"]["destination"]["origin"] = serde_json::json!("https://changed.test");
    rusqlite::Connection::open(path)
        .unwrap()
        .execute(
            "UPDATE task_workflows SET state=?1",
            [serde_json::to_string(&value).unwrap()],
        )
        .unwrap();
    assert!(h.task_workflow(r.context().id).is_err());
}

#[test]
fn confirmed_receipt_after_queue_retry_does_not_readmit_the_upload() {
    let mut r = runner(&settings(true), TaskIntent::Direct(CaptureTask::Upload));
    r.handle(TaskInput::Start);
    r.handle(TaskInput::Completed {
        step: TaskStep::Finalize,
        result: success(TaskStep::Finalize),
    });
    r.handle(TaskInput::Completed {
        step: TaskStep::Capture(CaptureTask::SaveFinal),
        result: success(TaskStep::Capture(CaptureTask::SaveFinal)),
    });
    let temp = tempfile::tempdir().unwrap();
    let h = History::open(&temp.path().join("h.db")).unwrap();
    h.admit_task_upload(&mut r).unwrap();
    assert!(
        r.handle(TaskInput::Completed {
            step: TaskStep::Capture(CaptureTask::Upload),
            result: StepResult::Failed("transient".into())
        })
        .is_empty()
    );
    assert_eq!(
        drive(&mut r, TaskInput::ConfirmedReceipt(receipt())),
        vec![TaskStep::AfterUpload(UploadTask::CopyUrl)]
    );
}

#[test]
fn restart_before_first_transition_resumes_durable_work() {
    let mut r = runner(&settings(false), TaskIntent::Direct(CaptureTask::CopyImage));
    r.accept_finalized(artifact(), None);
    let temp = tempfile::tempdir().unwrap();
    let h = History::open(&temp.path().join("h.db")).unwrap();
    h.save_task_workflow(&r).unwrap();
    let mut r = h.task_workflow(r.context().id).unwrap().unwrap();
    assert_eq!(
        drive(&mut r, TaskInput::Resume),
        vec![
            TaskStep::Capture(CaptureTask::SaveFinal),
            TaskStep::Capture(CaptureTask::CopyImage)
        ]
    );
}

#[test]
fn existing_admission_repairs_an_interrupted_workflow_link() {
    let mut r = runner(&settings(true), TaskIntent::Direct(CaptureTask::Upload));
    r.handle(TaskInput::Start);
    r.handle(TaskInput::Completed {
        step: TaskStep::Finalize,
        result: success(TaskStep::Finalize),
    });
    r.handle(TaskInput::Completed {
        step: TaskStep::Capture(CaptureTask::SaveFinal),
        result: success(TaskStep::Capture(CaptureTask::SaveFinal)),
    });
    let mut stale = r.clone();
    let temp = tempfile::tempdir().unwrap();
    let h = History::open(&temp.path().join("h.db")).unwrap();
    let first = h.admit_task_upload(&mut r).unwrap();
    h.save_task_workflow(&stale).unwrap();
    let recovered = h.admit_task_upload(&mut stale).unwrap();
    assert_eq!(first.id, recovered.id);
    assert_eq!(
        h.task_workflow(stale.context().id)
            .unwrap()
            .unwrap()
            .context()
            .upload_job,
        Some(first.id)
    );
}
