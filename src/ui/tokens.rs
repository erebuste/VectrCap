//! Layout and type values from the main-window design, in egui
//! points at 100% scale. Colors live in `platform::windows::appearance`.
use eframe::egui::{FontFamily, FontId};

// Sidebar
pub const SIDEBAR_WIDTH: f32 = 208.0;
pub const SIDEBAR_COLLAPSED: f32 = 56.0;
pub const SIDEBAR_PAD_X: f32 = 8.0;
pub const SIDEBAR_PAD_Y: f32 = 10.0;
pub const BRAND_HEIGHT: f32 = 36.0;
pub const NAV_ITEM_HEIGHT: f32 = 34.0;
pub const NAV_GAP: f32 = 10.0;
pub const ICON_SIZE: f32 = 15.0;

// Breakpoints, by content width
pub const COLLAPSE_BELOW: f32 = 960.0;
pub const STACK_BELOW: f32 = 620.0;

// Page
pub const PAGE_PAD_TOP: f32 = 22.0;
pub const PAGE_PAD_X: f32 = 26.0;
pub const PAGE_PAD_BOTTOM: f32 = 18.0;

// Type
pub const TITLE_SIZE: f32 = 20.0;
pub const BODY_SIZE: f32 = 13.0;
pub const SMALL_SIZE: f32 = 12.0;
pub const META_SIZE: f32 = 11.0;

// Controls
pub const BUTTON_HEIGHT: f32 = 30.0;
pub const BUTTON_HEIGHT_SMALL: f32 = 26.0;
pub const BUTTON_PAD: f32 = 11.0;
pub const BUTTON_PAD_SMALL: f32 = 9.0;
pub const RADIUS_CONTROL: u8 = 6;
pub const RADIUS_SURFACE: u8 = 10;
pub const SWITCH_WIDTH: f32 = 36.0;
pub const SWITCH_HEIGHT: f32 = 20.0;
pub const PILL_HEIGHT: f32 = 20.0;

// In progress
pub const GLYPH_BOX: f32 = 26.0;
pub const PROGRESS_HEIGHT: f32 = 6.0;
pub const IN_PROGRESS_MAX_ROWS: usize = 5;

// History
pub const GALLERY_GAP: f32 = 10.0;
pub const THUMB_HEIGHT: f32 = 104.0;
pub const INSPECTOR_WIDTH: f32 = 288.0;
pub const INSPECTOR_GAP: f32 = 18.0;
pub const PREVIEW_HEIGHT: f32 = 160.0;

// Settings
pub const SECTION_LIST_WIDTH: f32 = 168.0;
pub const SECTION_GAP: f32 = 28.0;
pub const GROUPS_MAX_WIDTH: f32 = 760.0;
pub const GROUP_GAP: f32 = 26.0;
pub const ROW_PAD_X: f32 = 16.0;
pub const ROW_PAD_Y: f32 = 13.0;
pub const CONTROL_COLUMN: f32 = 240.0;

pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("Geist Medium".into()))
}
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("Geist SemiBold".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Context, FontFamily, RichText};

    #[test]
    fn semibold_font_is_registered() {
        let ctx = Context::default();
        crate::platform::windows::appearance::install_fonts(&ctx);
        // Fonts set this frame apply from the next one; an unknown family panics there.
        for _ in 0..2 {
            let mut output = ctx.run_ui(Default::default(), |ui| {
                ui.label(RichText::new("Settings").font(semibold(20.0)));
                ui.label(RichText::new("History").font(medium(13.0)));
            });
            output.textures_delta.clear();
        }
        assert_eq!(
            semibold(20.0).family,
            FontFamily::Name("Geist SemiBold".into())
        );
        assert_eq!(medium(13.0).family, FontFamily::Name("Geist Medium".into()));
    }
}
