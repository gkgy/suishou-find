use eframe::egui::{self, ColorImage, TextureHandle};
use std::{
    collections::HashMap,
    sync::mpsc::{self, Receiver, SyncSender},
};
type Key = (String, Option<i64>);
type Reply = (Key, Option<ColorImage>);
pub struct Thumbnails {
    sender: SyncSender<Key>,
    receiver: Receiver<Reply>,
    cache: HashMap<Key, Option<TextureHandle>>,
    pending: std::collections::HashSet<Key>,
}
impl Thumbnails {
    pub fn new(context: egui::Context) -> Self {
        let (sender, requests) = mpsc::sync_channel::<Key>(8);
        let (replies, receiver) = mpsc::sync_channel::<Reply>(8);
        std::thread::spawn(move || {
            while let Ok(key) = requests.recv() {
                let decoded = decode(&key.0);
                if replies.send((key, decoded)).is_err() {
                    break;
                }
                context.request_repaint();
            }
        });
        Self {
            sender,
            receiver,
            cache: HashMap::new(),
            pending: Default::default(),
        }
    }
    pub fn receive(&mut self, context: &egui::Context) {
        for _ in 0..2 {
            let Ok((key, image)) = self.receiver.try_recv() else {
                break;
            };
            self.pending.remove(&key);
            // Bound GPU/cache use; at most 128 thumbnails of 120px each.
            if self.cache.len() >= 128 {
                self.cache.clear();
            }
            let texture = image
                .map(|im| context.load_texture("file thumbnail", im, egui::TextureOptions::LINEAR));
            self.cache.insert(key, texture);
            context.request_repaint();
        }
    }
    pub fn get(
        &mut self,
        _context: &egui::Context,
        path: &str,
        modified: Option<i64>,
    ) -> Option<TextureHandle> {
        let key = (path.to_owned(), modified);
        if let Some(texture) = self.cache.get(&key) {
            return texture.clone();
        }
        if !self.pending.contains(&key) && self.sender.try_send(key.clone()).is_ok() {
            self.pending.insert(key);
        }
        None
    }
}
fn decode(path: &str) -> Option<ColorImage> {
    if std::fs::metadata(path).ok()?.len() > 32 * 1024 * 1024 {
        return None;
    }
    let mut reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().ok()?.thumbnail(120, 120).to_rgba8();
    Some(ColorImage::from_rgba_unmultiplied(
        [image.width() as usize, image.height() as usize],
        image.as_raw(),
    ))
}
#[cfg(test)]
mod tests {
    #[test]
    fn thumbnails_resize_and_reject_invalid() {
        let path = std::env::temp_dir().join(format!("suishou-thumb-{}.png", std::process::id()));
        image::RgbaImage::from_pixel(240, 60, image::Rgba([20, 80, 200, 255]))
            .save(&path)
            .unwrap();
        assert_eq!(
            super::decode(path.to_str().unwrap()).unwrap().size,
            [120, 30]
        );
        std::fs::write(&path, b"invalid").unwrap();
        assert!(super::decode(path.to_str().unwrap()).is_none());
        std::fs::remove_file(path).unwrap();
    }
}
