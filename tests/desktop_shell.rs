use std::sync::Arc;
use vectrcap::{
    app::{
        destinations::{DestinationSelection, Destinations, resolve_destination},
        types::ArtifactKind,
    },
    persistence::{
        artifacts::ArtifactStore,
        history::{History, JobStatus},
        settings::ServerOrigin,
    },
};

#[test]
fn shell_queue_snapshot_is_bounded_and_does_not_recover_active_work() {
    let dir = tempfile::tempdir().unwrap();
    let history = Arc::new(History::open(&dir.path().join("history.db")).unwrap());
    let spool = ArtifactStore::new(dir.path().join("spool")).unwrap();
    let source = dir.path().join("file.txt");
    std::fs::write(&source, b"synthetic").unwrap();
    let artifact = spool.stage_file(&source, ArtifactKind::File).unwrap();
    let mut destinations = Destinations::default();
    let id = destinations
        .add(
            "Personal",
            ServerOrigin::parse("https://example.invalid").unwrap(),
            None,
        )
        .unwrap();
    destinations
        .bind_credential(id, &Default::default())
        .unwrap();
    let snapshot = resolve_destination(
        DestinationSelection::Inherit,
        DestinationSelection::Inherit,
        &destinations,
    )
    .unwrap();
    let running = history.create_job(&artifact, &snapshot).unwrap();
    history.transition(running.id, JobStatus::Running).unwrap();
    for _ in 0..102 {
        history.create_job(&artifact, &snapshot).unwrap();
    }
    let jobs = history.shell_jobs().unwrap();
    assert_eq!(jobs.len(), 100);
    assert!(
        jobs.iter()
            .any(|j| j.id == running.id && j.status == JobStatus::Running)
    );
    assert_eq!(
        history.job(running.id).unwrap().unwrap().status,
        JobStatus::Running
    );
}

#[test]
fn offline_connection_can_save_with_exact_origin_token_and_default() {
    use vectrcap::{persistence::settings::Settings, ui::destinations::ConnectionDraft};
    let settings = Settings::default();
    let mut draft = ConnectionDraft::new(&settings, None);
    draft.name = "Personal".into();
    draft.origin = "https://example.invalid".into();
    *draft.token = "synthetic-token".into();
    let edit = draft.prepare_save(&settings).unwrap();
    assert_eq!(edit.destinations.len(), 1);
    assert_eq!(edit.tokens.len(), 1);
    assert!(draft.token.is_empty());
    assert_eq!(edit.destinations.default_id(), Some(edit.tokens[0].0));
    assert_eq!(
        edit.destinations.iter().next().unwrap().origin.as_str(),
        "https://example.invalid"
    );
    assert!(draft.metadata.is_none());
}

#[test]
fn invalid_token_is_rejected_without_blanking_the_draft_field() {
    use vectrcap::{persistence::settings::Settings, ui::destinations::ConnectionDraft};
    let settings = Settings::default();
    let mut draft = ConnectionDraft::new(&settings, None);
    draft.name = "Personal".into();
    draft.origin = "https://example.invalid".into();
    // Trailing newline from a copy-paste is outside the printable-ASCII
    // range UploadToken requires, so this must fail validation.
    *draft.token = "synthetic-token\n".into();
    assert!(draft.prepare_save(&settings).is_err());
    assert_eq!(draft.token.as_str(), "synthetic-token\n");
}

#[test]
fn stale_metadata_never_verifies_changed_form_and_only_404_allows_manual_domain() {
    use vectrcap::{
        app::error::AppError, persistence::settings::Settings, ui::destinations::ConnectionDraft,
        vectr::uploader_info::UploaderInfoSupport,
    };
    let settings = Settings::default();
    let mut draft = ConnectionDraft::new(&settings, None);
    draft.name = "Personal".into();
    draft.origin = "https://example.invalid".into();
    *draft.token = "synthetic-token".into();
    let old = uuid::Uuid::new_v4();
    draft.pending_test = Some(old);
    draft.invalidate_test();
    draft.accept_metadata(old, Ok(UploaderInfoSupport::Unsupported404), 0);
    assert!(draft.metadata.is_none());
    draft.domain = Some("https://files.example.invalid".into());
    assert!(draft.prepare_save(&settings).is_err());
    let current = uuid::Uuid::new_v4();
    draft.pending_test = Some(current);
    draft.accept_metadata(current, Err(AppError::Tls), 0);
    assert!(draft.prepare_save(&settings).is_err());
    let current = uuid::Uuid::new_v4();
    draft.pending_test = Some(current);
    draft.accept_metadata(current, Ok(UploaderInfoSupport::Unsupported404), 0);
    assert!(draft.prepare_save(&settings).is_ok());
}

#[test]
fn metadata_from_previous_settings_revision_cannot_verify_current_form() {
    use vectrcap::{
        persistence::settings::Settings, ui::destinations::ConnectionDraft,
        vectr::uploader_info::UploaderInfoSupport,
    };
    let mut draft = ConnectionDraft::new(&Settings::default(), None);
    let request = uuid::Uuid::new_v4();
    draft.pending_test = Some(request);
    draft.accept_metadata(request, Ok(UploaderInfoSupport::Unsupported404), 1);
    assert!(draft.metadata.is_none());
    assert!(draft.pending_test.is_none());
}

#[test]
fn notice_expiration_includes_boundary_and_handles_future_start() {
    use std::{
        collections::HashMap,
        time::{Duration, Instant},
    };
    use vectrcap::app::{
        notifications::{CompletionNotice, CompletionStatus, JobCompletion, NotificationCenter},
        types::JobId,
    };
    use vectrcap::ui::completion_notification::expired_notice_ids;
    let mut center = NotificationCenter::new();
    center
        .record(CompletionNotice::Job(
            JobCompletion::try_new(
                JobId(uuid::Uuid::new_v4()),
                "file.txt",
                true,
                true,
                None,
                CompletionStatus::local_only(),
            )
            .unwrap(),
        ))
        .unwrap();
    let id = center.visible()[0].id();
    let now = Instant::now();
    let ttl = Duration::from_secs(6);
    assert_eq!(
        expired_notice_ids(&HashMap::from([(id, now - ttl)]), now, ttl),
        vec![id]
    );
    assert!(expired_notice_ids(&HashMap::from([(id, now + ttl)]), now, ttl).is_empty());
}

fn completion_view() -> vectrcap::app::desktop::DesktopView {
    use vectrcap::app::{controller::NoticeDisplay, notifications::*, types::JobId};
    let mut center = NotificationCenter::new();
    center
        .record(CompletionNotice::Job(
            JobCompletion::try_new(
                JobId(uuid::Uuid::new_v4()),
                "capture.png",
                true,
                true,
                None,
                CompletionStatus::local_only(),
            )
            .unwrap(),
        ))
        .unwrap();
    let view = center.visible().remove(0);
    let actions = center.actions(view.id()).unwrap();
    vectrcap::app::desktop::DesktopView {
        notices: vec![NoticeDisplay { view, actions }],
        ..Default::default()
    }
}

#[test]
fn completion_surface_is_available_while_main_window_is_hidden_and_withdraws_on_quit() {
    use vectrcap::{app::events::Lifecycle, ui::completion_notification::surface_visible};
    let mut view = completion_view();
    assert!(view.window.is_none());
    assert!(surface_visible(&view));
    view.notifications_enabled = false;
    assert!(!surface_visible(&view));
    view.notifications_enabled = true;
    view.lifecycle = Lifecycle::Draining;
    assert!(!surface_visible(&view));
    view.lifecycle = Lifecycle::Exited;
    assert!(!surface_visible(&view));
}

#[test]
fn completion_surface_requests_no_activation_or_taskbar_entry() {
    let builder = vectrcap::ui::completion_notification::viewport_builder();
    assert_eq!(builder.active, Some(false));
    assert_eq!(builder.taskbar, Some(false));
    assert_eq!(builder.decorations, Some(false));
    assert_eq!(
        builder.window_level,
        Some(eframe::egui::WindowLevel::AlwaysOnTop)
    );
    let size = builder.inner_size.unwrap();
    assert!(size.x <= 240.0 && size.y <= 180.0, "{size:?}");
    let position = builder.position.expect("toast position");
    // The toast follows the pointer's monitor (ShareX), else the primary one.
    use vectrcap::platform::windows::{monitors, windows::physical_pointer};
    let area = physical_pointer()
        .ok()
        .and_then(|p| {
            monitors::enumerate_work_areas()
                .ok()?
                .into_iter()
                .find(|a| a.contains(p))
        })
        .unwrap_or_else(|| monitors::primary_work_area().expect("primary work area"));
    let margin = 16.0;
    let x = (area.right() as f32 - size.x - margin).max(area.left() as f32);
    let y = (area.bottom() as f32 - size.y - margin).max(area.top() as f32);
    assert!(
        (position.x - x).abs() < 0.5 && (position.y - y).abs() < 0.5,
        "position {position:?} expected ({x}, {y}) on {area:?}"
    );
}

fn notice_display(
    jobs: Vec<vectrcap::app::notifications::JobCompletion>,
) -> vectrcap::app::controller::NoticeDisplay {
    use vectrcap::app::notifications::{CompletionNotice, NotificationCenter};
    let mut center = NotificationCenter::new();
    let notice = if jobs.len() == 1 {
        CompletionNotice::Job(jobs.into_iter().next().unwrap())
    } else {
        CompletionNotice::Batch(jobs)
    };
    center.record(notice).unwrap();
    let view = center.visible().remove(0);
    let actions = center.actions(view.id()).unwrap();
    vectrcap::app::controller::NoticeDisplay { view, actions }
}

fn click_notice(
    view: &vectrcap::app::desktop::DesktopView,
) -> Vec<vectrcap::app::events::AppCommand> {
    use eframe::egui::{Context, Event, Modifiers, PointerButton, RawInput, Rect, Vec2, pos2};
    use std::collections::HashMap;
    use vectrcap::ui::completion_notification::draw;
    let ctx = Context::default();
    let mut shown = HashMap::new();
    let input = |events| RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(800.0, 600.0))),
        events,
        ..Default::default()
    };
    let mut output = ctx.run_ui(input(vec![]), |ui| {
        let _ = draw(ui, view, &mut shown);
    });
    output.textures_delta.clear();
    let at = pos2(106.0, 64.0);
    let mut output = ctx.run_ui(
        input(vec![
            Event::PointerMoved(at),
            Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ]),
        |ui| {
            let _ = draw(ui, view, &mut shown);
        },
    );
    output.textures_delta.clear();
    let mut commands = vec![];
    let mut output = ctx.run_ui(
        input(vec![Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }]),
        |ui| {
            commands = draw(ui, view, &mut shown);
        },
    );
    output.textures_delta.clear();
    commands
}

#[test]
fn uploaded_thumbnail_click_opens_the_browser_link() {
    use vectrcap::app::{
        desktop::DesktopView,
        events::AppCommand,
        notifications::{CompletionStatus, JobCompletion, RequestedCompletionAction},
        types::{JobId, UploadReceipt},
    };
    let job = JobId(uuid::Uuid::new_v4());
    let receipt = UploadReceipt::new("https://example.com/capture.png", "capture.png").unwrap();
    let view = DesktopView {
        notices: vec![notice_display(vec![
            JobCompletion::try_new(
                job,
                "capture.png",
                true,
                true,
                Some(receipt),
                CompletionStatus::uploaded(),
            )
            .unwrap(),
        ])],
        ..Default::default()
    };
    assert!(matches!(
        vectrcap::ui::completion_notification::primary_action(&view.notices[0]),
        Some(RequestedCompletionAction::OpenLink(id)) if id == job
    ));
    let commands = click_notice(&view);
    assert!(
        matches!(
            commands.as_slice(),
            [
                AppCommand::ExecuteCompletionAction {
                    action: RequestedCompletionAction::OpenLink(opened),
                    ..
                },
                AppCommand::DismissNotice(_),
            ] if *opened == job
        ),
        "expected open-link then dismiss, got {} commands",
        commands.len()
    );
}

#[test]
fn local_only_thumbnail_click_does_not_open_a_browser() {
    use vectrcap::app::events::AppCommand;
    let view = completion_view();
    assert!(vectrcap::ui::completion_notification::primary_action(&view.notices[0]).is_none());
    let commands = click_notice(&view);
    assert!(
        !commands
            .iter()
            .any(|command| matches!(command, AppCommand::ExecuteCompletionAction { .. })),
        "local-only click ran {} commands",
        commands.len()
    );
}

#[test]
fn batch_thumbnail_click_opens_the_summary() {
    use vectrcap::app::{
        desktop::DesktopView,
        events::AppCommand,
        notifications::{CompletionStatus, JobCompletion, RequestedCompletionAction},
        types::{JobId, UploadReceipt},
    };
    let uploaded = JobId(uuid::Uuid::new_v4());
    let failed = JobId(uuid::Uuid::new_v4());
    let receipt = UploadReceipt::new("https://example.com/one.png", "one.png").unwrap();
    let view = DesktopView {
        notices: vec![notice_display(vec![
            JobCompletion::try_new(
                uploaded,
                "one.png",
                true,
                true,
                Some(receipt),
                CompletionStatus::uploaded(),
            )
            .unwrap(),
            JobCompletion::try_new(
                failed,
                "two.png",
                true,
                true,
                None,
                CompletionStatus::failed("transient").unwrap(),
            )
            .unwrap(),
        ])],
        ..Default::default()
    };
    assert!(matches!(
        vectrcap::ui::completion_notification::primary_action(&view.notices[0]),
        Some(RequestedCompletionAction::OpenBatchSummary { .. })
    ));
    let commands = click_notice(&view);
    assert!(
        matches!(
            commands.as_slice(),
            [
                AppCommand::ExecuteCompletionAction {
                    action: RequestedCompletionAction::OpenBatchSummary { .. },
                    ..
                },
                AppCommand::DismissNotice(_),
            ]
        ),
        "expected batch summary then dismiss, got {} commands",
        commands.len()
    );
}

#[test]
fn completion_surface_expires_notices_and_clears_disabled_timers() {
    use std::{
        collections::HashMap,
        time::{Duration, Instant},
    };
    use vectrcap::{app::events::AppCommand, ui::completion_notification::draw};
    let mut view = completion_view();
    let id = view.notices[0].view.id();
    let mut shown = HashMap::from([(id, Instant::now() - Duration::from_secs(7))]);
    let ctx = eframe::egui::Context::default();
    let mut commands = vec![];
    let mut output = ctx.run_ui(Default::default(), |ui| {
        commands = draw(ui, &view, &mut shown);
    });
    output.textures_delta.clear();
    assert!(matches!(commands.as_slice(), [AppCommand::DismissNotice(expired)] if *expired == id));
    view.notifications_enabled = false;
    let mut output = ctx.run_ui(Default::default(), |ui| {
        commands = draw(ui, &view, &mut shown);
    });
    output.textures_delta.clear();
    assert!(commands.is_empty());
    assert!(shown.is_empty());
}
