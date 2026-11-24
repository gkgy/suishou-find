use eframe::egui::{self, ColorImage, TextureHandle};
use std::{
    collections::{HashMap, HashSet},
    sync::mpsc::{self, Receiver, SyncSender},
};

type Key = (String, Option<i64>);
struct Decoded {
    image: ColorImage,
    dimensions: [u32; 2],
    bytes: u64,
}
#[derive(Clone)]
pub struct PreviewImage {
    pub texture: TextureHandle,
    pub dimensions: [u32; 2],
    pub bytes: u64,
}
pub struct Preview {
    sender: SyncSender<Key>,
    receiver: Receiver<(Key, Option<Decoded>)>,
    cache: HashMap<Key, Option<PreviewImage>>,
    pending: HashSet<Key>,
}
impl Preview {
    pub fn new(context: egui::Context) -> Self {
        // Independent from search and small thumbnails; at most two requests in flight.
        let (sender, requests) = mpsc::sync_channel::<Key>(1);
        let (replies, receiver) = mpsc::sync_channel(2);
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
            pending: HashSet::new(),
        }
    }
    pub fn receive(&mut self, context: &egui::Context) {
        for _ in 0..2 {
            let Ok((key, image)) = self.receiver.try_recv() else {
                break;
            };
            self.pending.remove(&key);
            if self.cache.len() >= 8 {
                self.cache.clear();
            }
            self.cache.insert(
                key,
                image.map(|im| PreviewImage {
                    texture: context.load_texture(
                        "selected file preview",
                        im.image,
                        egui::TextureOptions::LINEAR,
                    ),
                    dimensions: im.dimensions,
                    bytes: im.bytes,
                }),
            );
        }
    }
    pub fn get(&mut self, path: &str, modified: Option<i64>) -> Option<PreviewImage> {
        let key = (path.to_owned(), modified);
        if let Some(image) = self.cache.get(&key) {
            return image.clone();
        }
        if !self.pending.contains(&key) && self.sender.try_send(key.clone()).is_ok() {
            self.pending.insert(key);
        }
        None
    }
    pub fn finished(&self, path: &str, modified: Option<i64>) -> bool {
        self.cache.contains_key(&(path.to_owned(), modified))
    }
}
fn decode(path: &str) -> Option<Decoded> {
    let bytes = std::fs::metadata(path).ok()?.len();
    if bytes > 32 * 1024 * 1024 {
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
    let decoded = reader.decode().ok()?;
    let dimensions = [decoded.width(), decoded.height()];
    let image = decoded.thumbnail(720, 720).to_rgba8();
    Some(Decoded {
        dimensions,
        bytes,
        image: ColorImage::from_rgba_unmultiplied(
            [image.width() as usize, image.height() as usize],
            image.as_raw(),
        ),
    })
}
#[cfg(test)]
mod tests {
    #[test]
    fn preview_retains_source_dimensions_and_bounds_texture() {
        let path = std::env::temp_dir().join(format!("suishou-preview-{}.png", std::process::id()));
        image::RgbaImage::from_pixel(1440, 360, image::Rgba([20, 80, 200, 255]))
            .save(&path)
            .unwrap();
        let preview = super::decode(path.to_str().unwrap()).unwrap();
        assert_eq!(preview.dimensions, [1440, 360]);
        assert_eq!(preview.image.size, [720, 180]);
        std::fs::write(&path, b"invalid").unwrap();
        assert!(super::decode(path.to_str().unwrap()).is_none());
        std::fs::remove_file(path).unwrap();
    }
}
