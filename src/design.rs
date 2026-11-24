//! Shared typography and spacing for the search window and preference sheets.
use eframe::egui::{Color32, FontFamily, RichText};
pub const BODY: f32 = 15.0;
pub const CAPTION: f32 = 13.0;
pub const TITLE: f32 = 22.0;
pub const SECTION: f32 = 17.0;
pub const CONTROL_HEIGHT: f32 = 36.0;
pub const TEXT: Color32 = Color32::from_rgb(29, 29, 31);
pub const SECONDARY: Color32 = Color32::from_rgb(102, 109, 118);
pub const SURFACE: Color32 = Color32::from_rgb(247, 248, 250);
pub const ACCENT: Color32 = Color32::from_rgb(0, 122, 138);
pub const ACCENT_TEXT: Color32 = Color32::from_rgb(11, 111, 128);
pub const BORDER: Color32 = Color32::from_rgb(224, 227, 232);
pub fn medium(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text)
        .size(size)
        .family(FontFamily::Name("InterfaceMedium".into()))
}
pub fn semibold(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text)
        .size(size)
        .family(FontFamily::Name("InterfaceSemibold".into()))
}
pub fn checkbox(
    ui: &mut eframe::egui::Ui,
    checked: &mut bool,
    text: impl Into<eframe::egui::WidgetText>,
) -> eframe::egui::Response {
    ui.scope(|ui| {
        let visuals = ui.visuals_mut();
        for widget in [
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
        ] {
            widget.corner_radius = eframe::egui::CornerRadius::same(3);
        }
        ui.checkbox(checked, text)
    })
    .inner
}
