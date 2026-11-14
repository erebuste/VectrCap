//! Pure completion-notice state. These tests never touch the OS, clipboard,
//! network, credential store, or history database.

use vectrcap::app::error::AppError;
use vectrcap::app::notifications::{
    CompletionNotice, CompletionStatus, JobCompletion, LocalPresence, NotificationCenter,
    NotificationEffect, RequestedCompletionAction,
};
use vectrcap::app::types::{JobId, UploadReceipt};

fn receipt(token: &str) -> UploadReceipt {
    UploadReceipt::new(&format!("https://share.example/{token}"), "kept.png").unwrap()
}

fn job(
    name: &str,
    available: bool,
    safe_to_open: bool,
    proof: Option<UploadReceipt>,
    status: CompletionStatus,
) -> JobCompletion {
    JobCompletion::try_new(
        JobId::new_random(),
        name,
        available,
        safe_to_open,
        proof,
        status,
    )
    .unwrap()
}

fn uploaded(name: &str, available: bool, safe_to_open: bool) -> JobCompletion {
    let id = JobId::new_random();
    JobCompletion::try_new(
        id,
        name,
        available,
        safe_to_open,
        Some(receipt(&id.0.to_string())),
        CompletionStatus::uploaded(),
    )
    .unwrap()
}

fn show_only(effects: &[NotificationEffect]) -> &vectrcap::app::notifications::NoticeView {
    assert_eq!(effects.len(), 1, "recording must not emit a second effect");
    match &effects[0] {
        NotificationEffect::Show(view) => {
            assert!(!view.steals_focus());
            assert!(!view.auto_opens());
            assert!(view.executed().is_none());
            view
        }
        other => panic!("expected only Show, got {other:?}"),
    }
}

fn assert_no_destructive_effect(effects: &[NotificationEffect]) {
    assert!(!effects.is_empty());
    for effect in effects {
        match effect {
            NotificationEffect::Show(view) => {
                assert!(!view.steals_focus());
                assert!(!view.auto_opens());
                assert!(view.executed().is_none());
            }
            NotificationEffect::Dismissed { .. } | NotificationEffect::Suppressed => {}
        }
    }
}

fn job_ids(actions: &[RequestedCompletionAction]) -> Vec<JobId> {
    actions
        .iter()
        .flat_map(RequestedCompletionAction::job_ids)
        .collect()
}

#[test]
fn completion_actions_resolve_their_own_job() {
    let mut center = NotificationCenter::new();
    let openable = uploaded("alpha.png", true, true);
    let reveal = uploaded("beta.exe", true, false);
    let alpha = openable.job_id();
    let beta = reveal.job_id();

    let alpha_notice = show_only(&center.record(CompletionNotice::Job(openable)).unwrap()).id();
    let beta_notice = show_only(&center.record(CompletionNotice::Job(reveal)).unwrap()).id();

    let alpha_actions = center.actions(alpha_notice).unwrap();
    assert!(alpha_actions.contains(&RequestedCompletionAction::OpenLocal(alpha)));
    assert!(alpha_actions.contains(&RequestedCompletionAction::OpenLink(alpha)));
    assert!(alpha_actions.contains(&RequestedCompletionAction::CopyLink(alpha)));
    assert_eq!(job_ids(&alpha_actions), vec![alpha, alpha, alpha]);
    assert!(
        !alpha_actions
            .iter()
            .any(|action| action.job_ids().contains(&beta))
    );

    let beta_actions = center.actions(beta_notice).unwrap();
    assert_eq!(
        beta_actions,
        vec![
            RequestedCompletionAction::ShowFolder(beta),
            RequestedCompletionAction::OpenLink(beta),
            RequestedCompletionAction::CopyLink(beta),
        ]
    );

    assert_eq!(
        center
            .request(alpha_notice, &RequestedCompletionAction::CopyLink(alpha))
            .unwrap(),
        RequestedCompletionAction::CopyLink(alpha)
    );
    assert_eq!(
        center
            .request(beta_notice, &RequestedCompletionAction::ShowFolder(beta))
            .unwrap(),
        RequestedCompletionAction::ShowFolder(beta)
    );
    assert_eq!(
        center
            .request(alpha_notice, &RequestedCompletionAction::CopyLink(beta))
            .unwrap_err(),
        AppError::InvalidInput("completion action is not available")
    );
    assert_eq!(
        center
            .request(beta_notice, &RequestedCompletionAction::OpenLocal(alpha))
            .unwrap_err(),
        AppError::InvalidInput("completion action is not available")
    );
}

#[test]
fn missing_local_file_keeps_remote_actions() {
    let mut center = NotificationCenter::new();
    let missing = uploaded("gone.png", false, false);
    let id = missing.job_id();
    let notice = show_only(&center.record(CompletionNotice::Job(missing)).unwrap()).id();

    assert_eq!(
        center.actions(notice).unwrap(),
        vec![
            RequestedCompletionAction::OpenLink(id),
            RequestedCompletionAction::CopyLink(id),
        ]
    );
    let visible = center.visible();
    let item = &visible[0].items()[0];
    assert_eq!(item.job_id(), id);
    assert_eq!(item.local(), LocalPresence::Missing);
    assert!(item.has_confirmed_receipt());
    assert_eq!(item.status(), &CompletionStatus::uploaded());
    assert_eq!(center.summary_actions(notice, id).unwrap().len(), 2);
}

#[test]
fn notification_copy_failure_never_reuploads() {
    let mut center = NotificationCenter::new();
    let proof = UploadReceipt::new("https://share.example/CANARYURLTOKEN", "kept.png").unwrap();
    let completion = JobCompletion::try_new(
        JobId::new_random(),
        "CANARY_HOSTILE_NAME.png",
        true,
        true,
        Some(proof),
        CompletionStatus::uploaded(),
    )
    .unwrap();
    let id = completion.job_id();
    let notice = show_only(&center.record(CompletionNotice::Job(completion)).unwrap()).id();

    assert!(
        center
            .actions(notice)
            .unwrap()
            .contains(&RequestedCompletionAction::CopyLink(id))
    );
    center.note_copy_failed(notice, id).unwrap();
    center.note_copy_failed(notice, id).unwrap();

    let actions = center.actions(notice).unwrap();
    assert_eq!(
        actions,
        vec![
            RequestedCompletionAction::OpenLocal(id),
            RequestedCompletionAction::OpenLink(id),
            RequestedCompletionAction::CopyAgain(id),
        ]
    );
    for action in &actions {
        match action {
            RequestedCompletionAction::OpenLocal(job)
            | RequestedCompletionAction::OpenLink(job)
            | RequestedCompletionAction::CopyAgain(job) => assert_eq!(*job, id),
            RequestedCompletionAction::CopyLink(_)
            | RequestedCompletionAction::ShowFolder(_)
            | RequestedCompletionAction::OpenBatchSummary { .. }
            | RequestedCompletionAction::CopySuccessfulLinks { .. } => {
                panic!("copy failure changed the action set: {action:?}")
            }
        }
        assert!(!format!("{action:?}").contains("CANARYURLTOKEN"));
        assert!(!format!("{action:?}").contains("CANARY_HOSTILE_NAME"));
    }
    let visible = center.visible();
    let item = &visible[0].items()[0];
    assert!(item.has_confirmed_receipt());
    assert!(item.copy_failed());
    assert_eq!(item.status(), &CompletionStatus::uploaded());
    assert_eq!(item.display_name(), "CANARY_HOSTILE_NAME.png");
    assert_eq!(
        center
            .request(notice, &RequestedCompletionAction::CopyAgain(id))
            .unwrap(),
        RequestedCompletionAction::CopyAgain(id)
    );
    assert_eq!(
        center
            .request(notice, &RequestedCompletionAction::CopyLink(id))
            .unwrap_err(),
        AppError::InvalidInput("completion action is not available")
    );

    let rendered = format!("{center:?} {:?}", center.visible());
    assert!(!rendered.contains("CANARYURLTOKEN"));
    assert!(!rendered.contains("CANARY_HOSTILE_NAME"));
    assert!(!rendered.contains("https://share.example"));
}

#[test]
fn disabled_notifications_keep_history() {
    let mut center = NotificationCenter::new();
    assert!(center.notifications_enabled());

    let first = uploaded("kept.png", true, true);
    let first_id = first.job_id();
    let notice = show_only(&center.record(CompletionNotice::Job(first)).unwrap()).id();
    let disabled = center.set_notifications_enabled(false);
    assert_eq!(
        disabled,
        vec![NotificationEffect::Dismissed { notice_id: notice }]
    );
    assert_no_destructive_effect(&disabled);
    assert!(!center.notifications_enabled());
    assert!(center.visible().is_empty());
    assert_eq!(center.retained_job_count(), 0);
    assert_eq!(center.set_notifications_enabled(false), Vec::new());
    assert_eq!(
        center
            .request(notice, &RequestedCompletionAction::CopyLink(first_id))
            .unwrap_err(),
        AppError::InvalidInput("notice is not visible")
    );

    let hidden = uploaded("later.png", true, true);
    let hidden_id = hidden.job_id();
    let suppressed = center.record(CompletionNotice::Job(hidden)).unwrap();
    assert_eq!(suppressed, vec![NotificationEffect::Suppressed]);
    assert_no_destructive_effect(&suppressed);
    assert!(center.visible().is_empty());
    assert_eq!(center.retained_job_count(), 0);

    assert_eq!(center.set_notifications_enabled(true), Vec::new());
    assert!(center.notifications_enabled());
    assert!(
        center.visible().is_empty(),
        "disabling must not replay notices"
    );
    assert_eq!(center.retained_job_count(), 0);

    let again = uploaded("again.png", true, true);
    let again_notice = show_only(&center.record(CompletionNotice::Job(again)).unwrap()).id();
    assert_ne!(again_notice, notice);
    assert!(
        !center.visible()[0]
            .items()
            .iter()
            .any(|item| item.job_id() == hidden_id || item.job_id() == first_id)
    );
}

#[test]
fn batch_notice_is_grouped() {
    let mut center = NotificationCenter::new();
    let first = uploaded("one.png", true, true);
    let second = uploaded("two.png", false, false);
    let third = job(
        "three.png",
        true,
        true,
        None,
        CompletionStatus::failed("network").unwrap(),
    );
    let ids = [first.job_id(), second.job_id(), third.job_id()];
    let names = ["one.png", "two.png", "three.png"];
    let effects = center
        .record(CompletionNotice::Batch(vec![first, second, third]))
        .unwrap();
    let view = show_only(&effects);
    assert!(view.is_batch());
    assert_eq!(view.items().len(), 3);
    assert_eq!(center.visible().len(), 1);
    assert_eq!(center.retained_job_count(), 3);
    for (item, (id, name)) in view.items().iter().zip(ids.iter().zip(names)) {
        assert_eq!(item.job_id(), *id);
        assert_eq!(item.display_name(), name);
    }

    let notice = view.id();
    let top = center.actions(notice).unwrap();
    assert_eq!(
        top,
        vec![
            RequestedCompletionAction::OpenBatchSummary { jobs: ids.to_vec() },
            RequestedCompletionAction::CopySuccessfulLinks {
                jobs: vec![ids[0], ids[1]],
            },
        ]
    );
    assert!(!top.iter().any(|action| {
        matches!(
            action,
            RequestedCompletionAction::OpenLocal(_)
                | RequestedCompletionAction::OpenLink(_)
                | RequestedCompletionAction::CopyLink(_)
                | RequestedCompletionAction::ShowFolder(_)
        )
    }));
    assert_eq!(
        center.summary_actions(notice, ids[0]).unwrap(),
        vec![
            RequestedCompletionAction::OpenLocal(ids[0]),
            RequestedCompletionAction::OpenLink(ids[0]),
            RequestedCompletionAction::CopyLink(ids[0]),
        ]
    );
    assert_eq!(
        center.summary_actions(notice, ids[1]).unwrap(),
        vec![
            RequestedCompletionAction::OpenLink(ids[1]),
            RequestedCompletionAction::CopyLink(ids[1]),
        ]
    );
    assert_eq!(
        center.summary_actions(notice, ids[2]).unwrap(),
        vec![RequestedCompletionAction::OpenLocal(ids[2])]
    );
}

#[test]
fn notification_never_steals_focus_or_auto_opens() {
    let mut center = NotificationCenter::new();
    let completion = uploaded("focus.png", true, true);
    let id = completion.job_id();
    let effects = center.record(CompletionNotice::Job(completion)).unwrap();
    let view = show_only(&effects);
    assert!(view.executed().is_none());
    assert!(!view.auto_opens());
    assert!(!view.steals_focus());
    let notice = view.id();
    let available = center.actions(notice).unwrap();
    assert!(available.contains(&RequestedCompletionAction::OpenLocal(id)));
    assert!(available.contains(&RequestedCompletionAction::OpenLink(id)));
    assert_eq!(center.visible().len(), 1);
    assert_eq!(center.visible()[0].id(), notice);
    assert!(center.visible()[0].executed().is_none());
    let queried = center.actions(notice).unwrap();
    assert_eq!(queried, available);
    assert_eq!(center.retained_job_count(), 1);
}

#[test]
fn executable_source_offers_reveal_not_execute() {
    let mut center = NotificationCenter::new();
    let flagged = uploaded("payload.exe", true, false);
    let id = flagged.job_id();
    let notice = show_only(&center.record(CompletionNotice::Job(flagged)).unwrap()).id();
    let actions = center.actions(notice).unwrap();
    assert_eq!(
        actions,
        vec![
            RequestedCompletionAction::ShowFolder(id),
            RequestedCompletionAction::OpenLink(id),
            RequestedCompletionAction::CopyLink(id),
        ]
    );
    assert_eq!(
        center.visible()[0].items()[0].local(),
        LocalPresence::RevealOnly
    );
    for action in &actions {
        let rendered = format!("{action:?}");
        assert!(!rendered.contains("payload.exe"));
        assert!(!rendered.contains('\\'));
        assert!(!rendered.contains("cmd"));
        match action {
            RequestedCompletionAction::ShowFolder(job)
            | RequestedCompletionAction::OpenLink(job)
            | RequestedCompletionAction::CopyLink(job) => assert_eq!(*job, id),
            RequestedCompletionAction::OpenLocal(_) => panic!("executable was offered for open"),
            RequestedCompletionAction::CopyAgain(_)
            | RequestedCompletionAction::OpenBatchSummary { .. }
            | RequestedCompletionAction::CopySuccessfulLinks { .. } => {
                panic!("unexpected {action:?}")
            }
        }
    }

    let trusted = uploaded("payload.exe", true, true);
    let trusted_id = trusted.job_id();
    let trusted_notice = show_only(&center.record(CompletionNotice::Job(trusted)).unwrap()).id();
    assert!(
        center
            .actions(trusted_notice)
            .unwrap()
            .contains(&RequestedCompletionAction::OpenLocal(trusted_id))
    );
    assert!(
        !center
            .actions(trusted_notice)
            .unwrap()
            .iter()
            .any(|action| { matches!(action, RequestedCompletionAction::ShowFolder(_)) })
    );

    let ordinary = job(
        "notes.png",
        true,
        false,
        None,
        CompletionStatus::local_only(),
    );
    let ordinary_id = ordinary.job_id();
    let ordinary_notice = show_only(&center.record(CompletionNotice::Job(ordinary)).unwrap()).id();
    assert_eq!(
        center.actions(ordinary_notice).unwrap(),
        vec![RequestedCompletionAction::ShowFolder(ordinary_id)]
    );
}

#[test]
fn at_most_three_notices_are_visible() {
    let mut center = NotificationCenter::new();
    let mut notices = Vec::new();
    let mut jobs = Vec::new();
    for index in 0..3 {
        let completion = uploaded(&format!("file-{index}.png"), true, true);
        jobs.push(completion.job_id());
        let effects = center.record(CompletionNotice::Job(completion)).unwrap();
        assert_eq!(effects.len(), 1);
        notices.push(show_only(&effects).id());
    }
    assert_eq!(center.visible().len(), 3);
    assert_eq!(center.retained_job_count(), 3);

    let fourth = uploaded("file-3.png", true, true);
    let fourth_id = fourth.job_id();
    let effects = center.record(CompletionNotice::Job(fourth)).unwrap();
    assert_no_destructive_effect(&effects);
    assert_eq!(effects.len(), 2);
    match &effects[0] {
        NotificationEffect::Dismissed { notice_id } => assert_eq!(*notice_id, notices[0]),
        other => panic!("oldest notice was not withdrawn: {other:?}"),
    }
    match &effects[1] {
        NotificationEffect::Show(view) => {
            assert_eq!(view.items()[0].job_id(), fourth_id);
            assert!(!view.steals_focus());
            assert!(view.executed().is_none());
        }
        other => panic!("expected the new notice, got {other:?}"),
    }
    let visible_jobs: Vec<_> = center
        .visible()
        .iter()
        .map(|view| view.items()[0].job_id())
        .collect();
    assert_eq!(visible_jobs, vec![jobs[1], jobs[2], fourth_id]);
    assert_eq!(center.retained_job_count(), 3);
    assert_eq!(
        center.actions(notices[0]).unwrap_err(),
        AppError::InvalidInput("notice is not visible")
    );
    assert!(center.actions(notices[1]).is_ok());
    assert!(center.actions(notices[2]).is_ok());
}

#[test]
fn oversized_batch_is_refused() {
    let mut center = NotificationCenter::new();
    let keeper = uploaded("keeper.png", true, true);
    let keeper_id = keeper.job_id();
    let keeper_notice = show_only(&center.record(CompletionNotice::Job(keeper)).unwrap()).id();

    let too_many: Vec<_> = (0..101)
        .map(|index| uploaded(&format!("batch-{index}.png"), true, true))
        .collect();
    assert_eq!(
        center
            .record(CompletionNotice::Batch(too_many))
            .unwrap_err(),
        AppError::InvalidInput("completion batch exceeds 100 items")
    );
    assert_eq!(center.visible().len(), 1);
    assert_eq!(center.visible()[0].id(), keeper_notice);
    assert_eq!(center.retained_job_count(), 1);
    assert_eq!(center.visible()[0].items()[0].job_id(), keeper_id);

    let exact: Vec<_> = (0..100)
        .map(|index| uploaded(&format!("ok-{index}.png"), true, true))
        .collect();
    let exact_ids: Vec<_> = exact.iter().map(JobCompletion::job_id).collect();
    let effects = center.record(CompletionNotice::Batch(exact)).unwrap();
    let view = show_only(&effects);
    assert!(view.is_batch());
    assert_eq!(view.items().len(), 100);
    assert_eq!(
        view.items()
            .iter()
            .map(|item| item.job_id())
            .collect::<Vec<_>>(),
        exact_ids
    );
    assert_eq!(center.visible().len(), 2);
    assert_eq!(center.retained_job_count(), 101);
    assert_eq!(center.visible()[0].id(), keeper_notice);
}

#[test]
fn distinct_jobs_reject_stale_dismissed_ids() {
    let mut center = NotificationCenter::new();
    let alpha = uploaded("alpha.png", true, true);
    let beta = uploaded("beta.png", true, false);
    let alpha_id = alpha.job_id();
    let beta_id = beta.job_id();
    let notice = show_only(
        &center
            .record(CompletionNotice::Batch(vec![alpha, beta]))
            .unwrap(),
    )
    .id();

    assert_eq!(
        job_ids(&center.summary_actions(notice, alpha_id).unwrap()),
        vec![alpha_id, alpha_id, alpha_id]
    );
    assert_eq!(
        center.summary_actions(notice, beta_id).unwrap(),
        vec![
            RequestedCompletionAction::ShowFolder(beta_id),
            RequestedCompletionAction::OpenLink(beta_id),
            RequestedCompletionAction::CopyLink(beta_id),
        ]
    );
    assert_eq!(
        center
            .summary_actions(notice, JobId::new_random())
            .unwrap_err(),
        AppError::InvalidInput("job is not in this notice")
    );
    assert_eq!(
        center
            .request(notice, &RequestedCompletionAction::OpenLocal(alpha_id))
            .unwrap(),
        RequestedCompletionAction::OpenLocal(alpha_id)
    );

    let dismissed = center.dismiss(notice).unwrap();
    assert_eq!(
        dismissed,
        vec![NotificationEffect::Dismissed { notice_id: notice }]
    );
    assert_no_destructive_effect(&dismissed);
    assert!(center.visible().is_empty());
    assert_eq!(center.retained_job_count(), 0);
    for action in [
        RequestedCompletionAction::OpenLocal(alpha_id),
        RequestedCompletionAction::ShowFolder(beta_id),
        RequestedCompletionAction::CopySuccessfulLinks {
            jobs: vec![alpha_id, beta_id],
        },
    ] {
        assert_eq!(
            center.request(notice, &action).unwrap_err(),
            AppError::InvalidInput("notice is not visible")
        );
    }
    assert_eq!(
        center.dismiss(notice).unwrap_err(),
        AppError::InvalidInput("notice is not visible")
    );

    let foreign = {
        let mut other = NotificationCenter::new();
        show_only(
            &other
                .record(CompletionNotice::Job(uploaded("other.png", true, true)))
                .unwrap(),
        )
        .id()
    };
    assert_eq!(
        center.dismiss(foreign).unwrap_err(),
        AppError::InvalidInput("notice is not visible")
    );
    assert_eq!(center.retained_job_count(), 0);
}

#[test]
fn batch_copy_keeps_successful_submission_order() {
    let mut center = NotificationCenter::new();
    let failed = job(
        "failed.png",
        true,
        true,
        None,
        CompletionStatus::failed("tls").unwrap(),
    );
    let first = uploaded("first.png", false, false);
    let local = job(
        "local.png",
        true,
        true,
        None,
        CompletionStatus::local_only(),
    );
    let second = uploaded("second.png", true, false);
    let third = uploaded("third.png", true, true);
    let failed_id = failed.job_id();
    let first_id = first.job_id();
    let local_id = local.job_id();
    let second_id = second.job_id();
    let third_id = third.job_id();
    let order = vec![failed_id, first_id, local_id, second_id, third_id];
    let notice = show_only(
        &center
            .record(CompletionNotice::Batch(vec![
                failed, first, local, second, third,
            ]))
            .unwrap(),
    )
    .id();

    let successful = vec![first_id, second_id, third_id];
    assert_eq!(
        center.actions(notice).unwrap(),
        vec![
            RequestedCompletionAction::OpenBatchSummary {
                jobs: order.clone()
            },
            RequestedCompletionAction::CopySuccessfulLinks {
                jobs: successful.clone(),
            },
        ]
    );
    assert_eq!(
        center
            .request(
                notice,
                &RequestedCompletionAction::CopySuccessfulLinks {
                    jobs: successful.clone(),
                },
            )
            .unwrap(),
        RequestedCompletionAction::CopySuccessfulLinks { jobs: successful }
    );
    for wrong in [
        vec![third_id, second_id, first_id],
        vec![first_id],
        vec![first_id, second_id, third_id, failed_id],
        vec![failed_id],
    ] {
        assert_eq!(
            center
                .request(
                    notice,
                    &RequestedCompletionAction::CopySuccessfulLinks { jobs: wrong },
                )
                .unwrap_err(),
            AppError::InvalidInput("completion action is not available")
        );
    }
    assert!(
        !center
            .summary_actions(notice, failed_id)
            .unwrap()
            .iter()
            .any(|action| matches!(
                action,
                RequestedCompletionAction::CopyLink(_) | RequestedCompletionAction::OpenLink(_)
            ))
    );
    assert_eq!(
        center.visible()[0]
            .items()
            .iter()
            .map(|item| item.job_id())
            .collect::<Vec<_>>(),
        order
    );
}

#[test]
fn invalid_completion_snapshot_is_refused() {
    let mut center = NotificationCenter::new();
    let keeper = uploaded("keeper.png", true, true);
    let keeper_notice = show_only(&center.record(CompletionNotice::Job(keeper)).unwrap()).id();
    let before = center.retained_job_count();

    let cases = [
        JobCompletion::try_new(
            JobId::new_random(),
            "",
            true,
            true,
            None,
            CompletionStatus::local_only(),
        ),
        JobCompletion::try_new(
            JobId::new_random(),
            &"n".repeat(256),
            true,
            true,
            None,
            CompletionStatus::local_only(),
        ),
        JobCompletion::try_new(
            JobId::new_random(),
            "bad\nname.png",
            true,
            true,
            None,
            CompletionStatus::local_only(),
        ),
        JobCompletion::try_new(
            JobId::new_random(),
            "dir/shot.png",
            true,
            true,
            None,
            CompletionStatus::local_only(),
        ),
        JobCompletion::try_new(
            JobId::new_random(),
            "dir\\shot.png",
            true,
            true,
            None,
            CompletionStatus::local_only(),
        ),
        JobCompletion::try_new(
            JobId(uuid::Uuid::nil()),
            "nil.png",
            true,
            true,
            None,
            CompletionStatus::local_only(),
        ),
        JobCompletion::try_new(
            JobId::new_random(),
            "missing-safe.png",
            false,
            true,
            None,
            CompletionStatus::local_only(),
        ),
        JobCompletion::try_new(
            JobId::new_random(),
            "no-link.png",
            true,
            true,
            None,
            CompletionStatus::uploaded(),
        ),
        JobCompletion::try_new(
            JobId::new_random(),
            "extra-link.png",
            true,
            true,
            Some(receipt("extra")),
            CompletionStatus::failed("network").unwrap(),
        ),
    ];
    for result in cases {
        assert!(result.is_err(), "invalid snapshot was accepted");
    }
    assert_eq!(
        CompletionStatus::failed("Not a typed code").unwrap_err(),
        AppError::InvalidInput("failure code must be a short lowercase identifier")
    );
    assert_eq!(
        CompletionStatus::failed(&"x".repeat(65)).unwrap_err(),
        AppError::InvalidInput("failure code must be a short lowercase identifier")
    );
    assert!(CompletionStatus::failed("http.status").is_ok());

    let duplicate_id = JobId::new_random();
    let duplicate = |name: &str| {
        JobCompletion::try_new(
            duplicate_id,
            name,
            true,
            true,
            None,
            CompletionStatus::local_only(),
        )
        .unwrap()
    };
    assert_eq!(
        center
            .record(CompletionNotice::Batch(vec![
                duplicate("a.png"),
                duplicate("b.png"),
            ]))
            .unwrap_err(),
        AppError::InvalidInput("duplicate completion job")
    );
    let visible_duplicate = JobCompletion::try_new(
        center.visible()[0].items()[0].job_id(),
        "again.png",
        true,
        true,
        None,
        CompletionStatus::local_only(),
    )
    .unwrap();
    assert_eq!(
        center
            .record(CompletionNotice::Job(visible_duplicate))
            .unwrap_err(),
        AppError::InvalidInput("duplicate completion job")
    );
    assert_eq!(
        center.record(CompletionNotice::Batch(vec![])).unwrap_err(),
        AppError::InvalidInput("completion batch is empty")
    );
    assert_eq!(center.visible().len(), 1);
    assert_eq!(center.visible()[0].id(), keeper_notice);
    assert_eq!(center.retained_job_count(), before);

    center.set_notifications_enabled(false);
    assert_eq!(
        JobCompletion::try_new(
            JobId::new_random(),
            "bad\u{0001}name.png",
            true,
            true,
            None,
            CompletionStatus::local_only(),
        )
        .unwrap_err(),
        AppError::InvalidInput("display name is empty, too long, or contains control characters")
    );
    assert_eq!(center.retained_job_count(), 0);
}
