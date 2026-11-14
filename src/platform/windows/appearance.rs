//! Read-only system appearance, sampled on native wake/repaint (no idle timer).
#![allow(unsafe_code)]
use eframe::egui::{Color32, Stroke, Visuals};
use windows_sys::Win32::{
    Graphics::Gdi::{
        COLOR_BTNFACE, COLOR_BTNTEXT, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT,
        COLOR_WINDOW, COLOR_WINDOWTEXT, GetSysColor,
    },
    UI::{
        Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
        WindowsAndMessaging::{SPI_GETHIGHCONTRAST, SystemParametersInfoW},
    },
};
pub fn visuals() -> Visuals {
    let mut visual = Visuals::dark();
    visual.panel_fill = Color32::from_rgb(28, 30, 33);
    visual.window_fill = visual.panel_fill;
    visual.selection.bg_fill = Color32::from_rgb(109, 79, 36);
    visual.selection.stroke = Stroke::new(1.5, Color32::from_rgb(247, 211, 151));
    unsafe {
        let mut contrast = HIGHCONTRASTW {
            cbSize: size_of::<HIGHCONTRASTW>() as u32,
            ..std::mem::zeroed()
        };
        // Failure selects a conservative high-contrast palette rather than
        // assuming the user has not enabled a contrast theme.
        let known = SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            contrast.cbSize,
            (&mut contrast as *mut HIGHCONTRASTW).cast(),
            0,
        ) != 0;
        if !known || contrast.dwFlags & HCF_HIGHCONTRASTON != 0 {
            let color = |index| {
                let c = GetSysColor(index);
                Color32::from_rgb(c as u8, (c >> 8) as u8, (c >> 16) as u8)
            };
            let text = color(COLOR_WINDOWTEXT);
            let background = color(COLOR_WINDOW);
            visual.panel_fill = background;
            visual.window_fill = background;
            visual.extreme_bg_color = background;
            visual.faint_bg_color = background;
            visual.override_text_color = Some(text);
            visual.selection.bg_fill = color(COLOR_HIGHLIGHT);
            visual.selection.stroke = Stroke::new(2.0, color(COLOR_HIGHLIGHTTEXT));
            for widget in [
                &mut visual.widgets.noninteractive,
                &mut visual.widgets.inactive,
                &mut visual.widgets.hovered,
                &mut visual.widgets.active,
                &mut visual.widgets.open,
            ] {
                widget.bg_fill = color(COLOR_BTNFACE);
                widget.weak_bg_fill = widget.bg_fill;
                widget.bg_stroke = Stroke::new(1.0, text);
                widget.fg_stroke = Stroke::new(1.5, color(COLOR_BTNTEXT));
            }
            visual.weak_text_color = Some(color(COLOR_GRAYTEXT));
            visual.warn_fg_color = text;
            visual.error_fg_color = text;
        }
    }
    visual
}

/// The tray's hidden top-level window receives broadcast appearance changes,
/// including changes which winit does not classify as a light/dark transition.
/// Attach/detach and destruction all run on its owning UI thread.
pub struct AppearanceWake {
    window: windows_sys::Win32::Foundation::HWND,
    context: Option<Box<eframe::egui::Context>>,
}
impl AppearanceWake {
    /// # Safety
    /// `window` must be a valid, currently-open HWND owned by the calling
    /// thread for the lifetime of the returned `AppearanceWake` (it detaches
    /// the subclass on drop and must not outlive the window).
    pub unsafe fn attach(
        window: windows_sys::Win32::Foundation::HWND,
        context: eframe::egui::Context,
    ) -> Result<Self, crate::app::error::AppError> {
        let context = Box::new(context);
        unsafe {
            if windows_sys::Win32::UI::Shell::SetWindowSubclass(
                window,
                Some(appearance_message),
                0x56434150,
                (&*context as *const eframe::egui::Context) as usize,
            ) == 0
            {
                return Err(crate::app::error::AppError::Persistence(
                    "appearance event subscription failed",
                ));
            }
        }
        Ok(Self {
            window,
            context: Some(context),
        })
    }
}
unsafe extern "system" fn appearance_message(
    window: windows_sys::Win32::Foundation::HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
    _id: usize,
    data: usize,
) -> isize {
    use windows_sys::Win32::UI::{
        Shell::DefSubclassProc,
        WindowsAndMessaging::{WM_SETTINGCHANGE, WM_SYSCOLORCHANGE, WM_THEMECHANGED},
    };
    if matches!(
        message,
        WM_SETTINGCHANGE | WM_SYSCOLORCHANGE | WM_THEMECHANGED
    ) {
        // No panic may cross the native callback boundary. Reference lifetime
        // is bounded by the successful subclass registration and guard below.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            (&*(data as *const eframe::egui::Context)).request_repaint();
        }));
    }
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}
impl Drop for AppearanceWake {
    fn drop(&mut self) {
        unsafe {
            let removed = windows_sys::Win32::UI::Shell::RemoveWindowSubclass(
                self.window,
                Some(appearance_message),
                0x56434150,
            ) != 0;
            if !removed && windows_sys::Win32::UI::WindowsAndMessaging::IsWindow(self.window) != 0 {
                // Prefer one leaked context over a live native dangling pointer.
                if let Some(context) = self.context.take() {
                    Box::leak(context);
                }
            }
        }
    }
}
