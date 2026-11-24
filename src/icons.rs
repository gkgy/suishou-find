use eframe::egui::{self, Color32, ColorImage, TextureHandle};
use std::collections::HashMap;
pub struct Icons(HashMap<&'static str, TextureHandle>);
impl Icons {
    pub fn new(context: &egui::Context) -> Self {
        let mut icons = HashMap::new();
        macro_rules! load {
            ($name:literal) => {
                let bytes = include_bytes!(concat!("../assets/icons/", $name, ".png"));
                let image = image::load_from_memory(bytes)
                    .expect("bundled icon")
                    .thumbnail(48, 48)
                    .to_rgba8();
                let color = ColorImage::from_rgba_unmultiplied(
                    [image.width() as usize, image.height() as usize],
                    image.as_raw(),
                );
                icons.insert(
                    $name,
                    context.load_texture($name, color, egui::TextureOptions::LINEAR),
                );
            };
        }
        load!("layout-grid");
        load!("folder");
        load!("file-text");
        load!("photo");
        load!("music");
        load!("video");
        load!("apps");
        load!("file-zip");
        load!("code");
        load!("dots");
        load!("search");
        load!("keyboard");
        load!("settings");
        load!("list");
        load!("clock");
        load!("external-link");
        load!("x");
        Self(icons)
    }
    pub fn image(&self, name: &str, size: f32, color: Color32) -> egui::Image<'_> {
        egui::Image::new(self.0.get(name).expect("known icon"))
            .fit_to_exact_size(egui::vec2(size, size))
            .tint(color)
    }
}
