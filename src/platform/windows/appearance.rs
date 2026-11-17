//! Read-only system appearance, sampled on native wake/repaint (no idle timer).
//!
//! The non-contrast palette below is sampled directly from the Vectr web
//! panel's own design tokens (`web/src/index.css`, the `.dark` block) so the
//! desktop app and the web panel read as one product: dark neutral surfaces
//! with a lavender primary/selection/focus accent, rather than an invented
//! desktop-only palette. Every value here is a verified sRGB conversion of
//! that file's `oklch(...)` tokens (see each constant's doc comment for the
//! source custom property); nothing is guessed. An enabled Windows contrast
//! theme still fully overrides this palette below, unchanged from before.
#![allow(unsafe_code)]
use eframe::egui::{Color32, CornerRadius, Stroke, Visuals};
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

/// `--background` (`oklch(0.145 0 0)`).
const VECTR_BACKGROUND: Color32 = Color32::from_rgb(10, 10, 10);
/// `--foreground` (`oklch(0.985 0 0)`).
const VECTR_FOREGROUND: Color32 = Color32::from_rgb(250, 250, 250);
/// `--card` / `--popover` (`oklch(0.205 0 0)`).
const VECTR_CARD: Color32 = Color32::from_rgb(23, 23, 23);
/// `--muted` / `--accent` / `--secondary` (`oklch(0.269 0 0)`).
const VECTR_MUTED: Color32 = Color32::from_rgb(38, 38, 38);
/// A lightened `--muted` for hover feedback; the CSS expresses hover with an
/// opacity utility rather than a separate token.
const VECTR_MUTED_HOVER: Color32 = Color32::from_rgb(54, 54, 54);
/// `--muted-foreground` (`oklch(0.708 0 0)`).
const VECTR_MUTED_FOREGROUND: Color32 = Color32::from_rgb(161, 161, 161);
/// `--primary` / `--ring` (`oklch(0.78 0.11 305)`): the panel's lavender accent.
const VECTR_PRIMARY: Color32 = Color32::from_rgb(200, 166, 240);
/// `--primary-foreground` (`oklch(0.2 0.04 305)`).
const VECTR_PRIMARY_FOREGROUND: Color32 = Color32::from_rgb(26, 17, 37);
/// `--destructive` (`oklch(0.704 0.191 22.216)`).
const VECTR_DESTRUCTIVE: Color32 = Color32::from_rgb(255, 100, 103);
/// `--border` (`oklch(1 0 0 / 10%)`): translucent white over the background.
const VECTR_BORDER: Color32 = Color32::from_rgba_premultiplied(26, 26, 26, 26);
/// Mockup status green (no Vectr token; `docs/mockups/2026-11-17-main-window.html`).
const VECTR_SUCCESS: Color32 = Color32::from_rgb(134, 209, 161);
/// Mockup attention amber.
const VECTR_WARNING: Color32 = Color32::from_rgb(242, 196, 109);
/// Mockup thumbnail backdrop.
const VECTR_THUMBNAIL: Color32 = Color32::from_rgb(18, 18, 18);
/// `rgba(255,255,255,.18)` premultiplied.
const VECTR_TOGGLED_BORDER: Color32 = Color32::from_rgba_premultiplied(46, 46, 46, 46);
/// `rgba(255,255,255,.24)` premultiplied.
const VECTR_CARD_HOVER_BORDER: Color32 = Color32::from_rgba_premultiplied(61, 61, 61, 61);
/// `--radius: 0.625rem` scaled down, matching `--radius-sm`/`--radius-md`.
const VECTR_RADIUS_CONTROL: u8 = 6;
/// `--radius` itself, used for window/menu chrome.
const VECTR_RADIUS_SURFACE: u8 = 10;

pub fn visuals() -> Visuals {
    let mut visual = Visuals::dark();
    visual.panel_fill = VECTR_BACKGROUND;
    visual.window_fill = VECTR_BACKGROUND;
    visual.extreme_bg_color = VECTR_CARD;
    visual.faint_bg_color = VECTR_MUTED;
    visual.code_bg_color = VECTR_CARD;
    visual.hyperlink_color = VECTR_PRIMARY;
    visual.error_fg_color = VECTR_DESTRUCTIVE;
    visual.selection.bg_fill = VECTR_PRIMARY;
    visual.selection.stroke = Stroke::new(1.5, VECTR_PRIMARY_FOREGROUND);
    visual.window_corner_radius = CornerRadius::same(VECTR_RADIUS_SURFACE);
    visual.menu_corner_radius = CornerRadius::same(VECTR_RADIUS_SURFACE);
    visual.widgets.noninteractive.bg_fill = VECTR_CARD;
    visual.widgets.noninteractive.weak_bg_fill = VECTR_BACKGROUND;
    visual.widgets.noninteractive.bg_stroke = Stroke::new(1.0, VECTR_BORDER);
    visual.widgets.noninteractive.fg_stroke = Stroke::new(1.0, VECTR_FOREGROUND);
    visual.widgets.noninteractive.corner_radius = CornerRadius::same(VECTR_RADIUS_CONTROL);
    visual.widgets.inactive.bg_fill = VECTR_MUTED;
    visual.widgets.inactive.weak_bg_fill = VECTR_CARD;
    visual.widgets.inactive.bg_stroke = Stroke::new(1.0, VECTR_BORDER);
    visual.widgets.inactive.fg_stroke = Stroke::new(1.0, VECTR_FOREGROUND);
    visual.widgets.inactive.corner_radius = CornerRadius::same(VECTR_RADIUS_CONTROL);
    visual.widgets.hovered.bg_fill = VECTR_MUTED_HOVER;
    visual.widgets.hovered.weak_bg_fill = VECTR_MUTED;
    visual.widgets.hovered.bg_stroke = Stroke::new(1.0, VECTR_PRIMARY);
    visual.widgets.hovered.fg_stroke = Stroke::new(1.0, VECTR_FOREGROUND);
    visual.widgets.hovered.corner_radius = CornerRadius::same(VECTR_RADIUS_CONTROL);
    visual.widgets.active.bg_fill = VECTR_PRIMARY;
    visual.widgets.active.weak_bg_fill = VECTR_PRIMARY;
    visual.widgets.active.bg_stroke = Stroke::new(1.0, VECTR_PRIMARY);
    visual.widgets.active.fg_stroke = Stroke::new(1.0, VECTR_PRIMARY_FOREGROUND);
    visual.widgets.active.corner_radius = CornerRadius::same(VECTR_RADIUS_CONTROL);
    visual.widgets.open.bg_fill = VECTR_PRIMARY;
    visual.widgets.open.weak_bg_fill = VECTR_MUTED;
    visual.widgets.open.bg_stroke = Stroke::new(1.0, VECTR_PRIMARY);
    visual.widgets.open.fg_stroke = Stroke::new(1.0, VECTR_PRIMARY_FOREGROUND);
    visual.widgets.open.corner_radius = CornerRadius::same(VECTR_RADIUS_CONTROL);
    visual.weak_text_color = Some(VECTR_MUTED_FOREGROUND);
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

/// Bundle the same sans family as Vectr, with egui's defaults retained for glyph fallback.
pub fn install_fonts(context: &eframe::egui::Context) {
    use eframe::egui::{FontData, FontDefinitions, FontFamily};
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "Geist".into(),
        std::sync::Arc::new(FontData::from_static(include_bytes!(
            "../../../assets/fonts/Geist-Regular.ttf"
        ))),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "Geist".into());
    // egui does not synthesise weights, so titles and labels need their own files.
    let fallback = fonts.families[&FontFamily::Proportional].clone();
    for (name, bytes) in [
        (
            "Geist Medium",
            include_bytes!("../../../assets/fonts/Geist-Medium.ttf").as_slice(),
        ),
        (
            "Geist SemiBold",
            include_bytes!("../../../assets/fonts/Geist-SemiBold.ttf").as_slice(),
        ),
    ] {
        fonts.font_data.insert(
            name.into(),
            std::sync::Arc::new(FontData::from_static(bytes)),
        );
        let mut family = vec![name.to_string()];
        family.extend(fallback.iter().filter(|f| f.as_str() != "Geist").cloned());
        fonts.families.insert(FontFamily::Name(name.into()), family);
    }
    context.set_fonts(fonts);
}

/// Mockup colors that `Visuals` has no slot for. Under a Windows contrast theme
/// (`override_text_color` set) each falls back to a system-derived color.
pub fn success(visuals: &Visuals) -> Color32 {
    visuals.override_text_color.unwrap_or(VECTR_SUCCESS)
}
pub fn warning(visuals: &Visuals) -> Color32 {
    visuals.override_text_color.unwrap_or(VECTR_WARNING)
}
pub fn thumbnail_fill(visuals: &Visuals) -> Color32 {
    if visuals.override_text_color.is_some() {
        visuals.extreme_bg_color
    } else {
        VECTR_THUMBNAIL
    }
}
/// Border of a toggled-on control (muted fill, slightly brighter edge).
pub fn toggled_border(visuals: &Visuals) -> Color32 {
    visuals.override_text_color.unwrap_or(VECTR_TOGGLED_BORDER)
}
/// Border of a hovered gallery card.
pub fn card_hover_border(visuals: &Visuals) -> Color32 {
    visuals
        .override_text_color
        .unwrap_or(VECTR_CARD_HOVER_BORDER)
}
