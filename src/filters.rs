use miao_find::index::{Category, SortOrder};
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SavedFilter {
    pub name: String,
    pub query: String,
    pub category: Category,
    pub order: SortOrder,
    pub paths: bool,
    pub personal: bool,
    pub recent: bool,
    pub thumbnails: bool,
    #[serde(default)]
    pub raster: bool,
}
impl SavedFilter {
    pub fn recent_images() -> Self {
        Self {
            name: "最近生成的图片".into(),
            query: "/output".into(),
            category: Category::Image,
            order: SortOrder::Latest,
            paths: false,
            personal: true,
            recent: true,
            thumbnails: true,
            raster: true,
        }
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Preferences {
    pub personal_roots: Vec<PathBuf>,
    pub saved: Vec<SavedFilter>,
    #[serde(default = "miao_find::index::default_personal_exclusions")]
    pub exclusions: Vec<String>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            personal_roots: default_roots(),
            saved: Vec::new(),
            exclusions: miao_find::index::default_personal_exclusions(),
        }
    }
}
pub fn default_roots() -> Vec<PathBuf> {
    let home = directories::BaseDirs::new()
        .map(|d| d.home_dir().to_owned())
        .unwrap_or_default();
    [
        "Desktop",
        "Documents",
        "Downloads",
        "Pictures",
        "Movies",
        "Music",
        "AI",
    ]
    .into_iter()
    .map(|name| home.join(name))
    .collect()
}
pub fn seven_days_ago() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
        - 7 * 86400 * 1000
}

#[cfg(test)]
mod tests {
    #[derive(Default)]
    struct Store(std::collections::HashMap<String, String>);
    impl eframe::Storage for Store {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.into(), value);
        }
        fn remove_string(&mut self, key: &str) {
            self.0.remove(key);
        }
        fn flush(&mut self) {}
    }
    #[test]
    fn seven_days_uses_index_milliseconds() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let elapsed = now - super::seven_days_ago();
        assert!((7 * 86400 * 1000 - 1000..=7 * 86400 * 1000 + 1000).contains(&elapsed));
    }
    #[test]
    fn saved_filter_restores_all_settings_and_relative_date() {
        let mut storage = Store::default();
        let mut preferences = super::Preferences::default();
        let mut filter = super::SavedFilter::recent_images();
        filter.name = "my outputs".into();
        filter.query = "/output baby".into();
        preferences.saved.push(filter);
        eframe::set_value(&mut storage, "prefs", &preferences);
        let restored: super::Preferences = eframe::get_value(&storage, "prefs").unwrap();
        assert_eq!(restored.personal_roots, preferences.personal_roots);
        let f = &restored.saved[0];
        assert_eq!(f.query, "/output baby");
        assert!(f.personal && f.recent && f.thumbnails && f.raster && !f.paths);
        assert_eq!(f.category, miao_find::index::Category::Image);
        assert_eq!(f.order, miao_find::index::SortOrder::Latest);
        assert_eq!(restored.exclusions, preferences.exclusions);
    }
}
