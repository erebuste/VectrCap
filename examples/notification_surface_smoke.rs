//! Manual native smoke check: cargo run --example notification_surface_smoke.
//! Shows synthetic local-only content; does not read settings or upload.
#![allow(unsafe_code)]
use eframe::egui;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use vectrcap::{
    app::{
        controller::NoticeDisplay, desktop::DesktopView, events::AppCommand, notifications::*,
        types::JobId,
    },
    ui::completion_notification,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetForegroundWindow, IsWindowVisible,
};

struct Smoke {
    view: DesktopView,
    shown: Arc<std::sync::Mutex<HashMap<NoticeId, Instant>>>,
    start: Instant,
    visible: Arc<AtomicBool>,
    focused: Arc<AtomicBool>,
    expired: Arc<AtomicBool>,
    root_visible: Arc<AtomicBool>,
}
impl eframe::App for Smoke {
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        if self.start.elapsed() > Duration::from_secs(8) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(50));
        if self.expired.load(Ordering::Relaxed) {
            self.view.notices.clear();
        }
        let root_title: Vec<u16> = "VectrCap hidden smoke root\0".encode_utf16().collect();
        unsafe {
            let root = FindWindowW(std::ptr::null(), root_title.as_ptr());
            self.root_visible.fetch_or(
                !root.is_null() && IsWindowVisible(root) != 0,
                Ordering::Relaxed,
            );
        }
        if self.view.notices.is_empty() {
            return;
        }
        let view = self.view.clone();
        let shown = self.shown.clone();
        let visible = self.visible.clone();
        let focused = self.focused.clone();
        let expired = self.expired.clone();
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of("smoke-notice"),
            completion_notification::viewport_builder(),
            move |ui, _| {
                let title: Vec<u16> = "VectrCap notifications\0".encode_utf16().collect();
                unsafe {
                    let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
                    if !hwnd.is_null() {
                        visible.fetch_or(IsWindowVisible(hwnd) != 0, Ordering::Relaxed);
                        focused.fetch_or(GetForegroundWindow() == hwnd, Ordering::Relaxed);
                    }
                }
                let mut shown = shown.lock().unwrap();
                for cmd in completion_notification::draw(ui, &view, &mut shown) {
                    if matches!(cmd, AppCommand::DismissNotice(_)) {
                        expired.store(true, Ordering::Relaxed);
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                ui.ctx().request_repaint_after(Duration::from_millis(50));
            },
        );
        if self.start.elapsed() > Duration::from_secs(7) {
            self.view.notices.clear();
        }
    }
    fn ui(&mut self, _: &mut egui::Ui, _: &mut eframe::Frame) {}
}
fn main() -> eframe::Result {
    let mut center = NotificationCenter::new();
    center
        .record(CompletionNotice::Job(
            JobCompletion::try_new(
                JobId(uuid::Uuid::new_v4()),
                "Synthetic capture.png",
                true,
                true,
                None,
                CompletionStatus::local_only(),
            )
            .unwrap(),
        ))
        .unwrap();
    let notice = center.visible().remove(0);
    let actions = center.actions(notice.id()).unwrap();
    let view = DesktopView {
        notices: vec![NoticeDisplay {
            view: notice,
            actions,
        }],
        ..Default::default()
    };
    let visible = Arc::new(AtomicBool::new(false));
    let focused = Arc::new(AtomicBool::new(false));
    let expired = Arc::new(AtomicBool::new(false));
    let root_visible = Arc::new(AtomicBool::new(false));
    let (v, f, e, r) = (
        visible.clone(),
        focused.clone(),
        expired.clone(),
        root_visible.clone(),
    );
    eframe::run_native(
        "VectrCap hidden smoke root",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_visible(false)
                .with_active(false),
            renderer: eframe::Renderer::Glow,
            ..Default::default()
        },
        Box::new(move |_| {
            Ok(Box::new(Smoke {
                view,
                shown: Default::default(),
                start: Instant::now(),
                visible: v,
                focused: f,
                expired: e,
                root_visible: r,
            }))
        }),
    )?;
    assert!(
        visible.load(Ordering::Relaxed),
        "native notice was never visible"
    );
    assert!(!focused.load(Ordering::Relaxed), "native notice took focus");
    assert!(expired.load(Ordering::Relaxed), "notice did not expire");
    assert!(
        !root_visible.load(Ordering::Relaxed),
        "hidden root became visible"
    );
    println!(
        "Native notice appeared and expired without taking focus or revealing the main window."
    );
    Ok(())
}
