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
    #[serde(default)]
    pub recent: bool,
    #[serde(default)]
    pub time_range: TimeRange,
    pub thumbnails: bool,
    #[serde(default)]
    pub raster: bool,
}
impl SavedFilter {
    pub fn empty() -> Self {
        Self {
            name: String::new(),
            query: String::new(),
            category: Category::All,
            order: SortOrder::Relevance,
            paths: false,
            personal: false,
            recent: false,
            time_range: TimeRange::Any,
            thumbnails: false,
            raster: false,
        }
    }
    pub fn builtins() -> Vec<Self> {
        let mut presets = Vec::new();
        for (name, query, category, time_range, thumbnails, raster) in [
            (
                "最近一周的文档",
                "",
                Category::Document,
                TimeRange::Week,
                false,
                false,
            ),
            (
                "最近下载的文件",
                "/Downloads",
                Category::All,
                TimeRange::Week,
                false,
                false,
            ),
            (
                "最近一周的图片",
                "",
                Category::Image,
                TimeRange::Week,
                true,
                true,
            ),
            (
                "PDF 文档",
                ".pdf",
                Category::Document,
                TimeRange::Any,
                false,
                false,
            ),
            (
                "压缩包",
                "",
                Category::Archive,
                TimeRange::Any,
                false,
                false,
            ),
            (
                "最近修改的代码",
                "",
                Category::Code,
                TimeRange::Week,
                false,
                false,
            ),
            (
                "最近生成的图片",
                "/output",
                Category::Image,
                TimeRange::Week,
                true,
                true,
            ),
        ] {
            let mut filter = Self::empty();
            filter.name = name.into();
            filter.query = query.into();
            filter.category = category;
            filter.time_range = time_range;
            filter.order = SortOrder::Latest;
            filter.thumbnails = thumbnails;
            filter.raster = raster;
            presets.push(filter);
        }
        presets
    }

    #[cfg(test)]
    pub fn recent_images() -> Self {
        Self {
            name: crate::i18n::tr("最近生成的图片").into(),
            query: "/output".into(),
            category: Category::Image,
            order: SortOrder::Latest,
            paths: false,
            personal: true,
            recent: true,
            time_range: TimeRange::Week,
            thumbnails: true,
            raster: true,
        }
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Preferences {
    #[serde(default)]
    pub english: bool,
    #[serde(default = "default_preview_visible")]
    pub preview_visible: bool,
    #[serde(default)]
    pub shortcut: crate::shortcut::Shortcut,
    pub personal_roots: Vec<PathBuf>,
    #[serde(default)]
    pub scope_locations: Vec<PathBuf>,
    pub saved: Vec<SavedFilter>,
    #[serde(default = "miao_find::index::default_personal_exclusions")]
    pub exclusions: Vec<String>,
}
fn default_preview_visible() -> bool {
    true
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            english: false,
            preview_visible: true,
            shortcut: crate::shortcut::Shortcut::default(),
            personal_roots: default_roots(),
            scope_locations: default_roots(),
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

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TimeRange {
    #[default]
    Any,
    Days3,
    Week,
    Month,
    Year,
    Custom {
        start: String,
        end: String,
    },
}
impl TimeRange {
    pub fn label(&self) -> &str {
        match self {
            Self::Any => crate::i18n::tr("不限时间"),
            Self::Days3 => crate::i18n::tr("最近 3 天"),
            Self::Week => crate::i18n::tr("最近一周"),
            Self::Month => crate::i18n::tr("最近一个月"),
            Self::Year => crate::i18n::tr("最近一年"),
            Self::Custom { .. } => crate::i18n::tr("自定义日期…"),
        }
    }
    pub fn relative(&self) -> bool {
        matches!(self, Self::Days3 | Self::Week | Self::Month | Self::Year)
    }
    pub fn bounds(&self) -> Result<(Option<i64>, Option<i64>), String> {
        self.bounds_at(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
        )
    }
    fn bounds_at(&self, now: i64) -> Result<(Option<i64>, Option<i64>), String> {
        let days = match self {
            Self::Any => return Ok((None, None)),
            Self::Days3 => 3,
            Self::Week => 7,
            Self::Month => 30,
            Self::Year => 365,
            Self::Custom { start, end } => {
                let start = local_date(start, false)?;
                let end = local_date(end, true)?;
                if start >= end {
                    return Err(crate::i18n::tr("开始日期不能晚于结束日期").into());
                }
                return Ok((Some(start), Some(end)));
            }
        };
        Ok((Some(now - days * 86_400_000), Some(now + 1)))
    }
}
/// Local calendar boundaries, including DST changes. Never silently normalize invalid dates.
fn local_date(text: &str, next_day: bool) -> Result<i64, String> {
    let invalid =
        || crate::i18n::tr("请输入有效日期，例如 2026-10-12（年份 1900–9999）").to_string();
    let parts: Vec<i32> = text
        .trim()
        .split('-')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|_| invalid())?;
    if parts.len() != 3
        || !(1900..=9999).contains(&parts[0])
        || !(1..=12).contains(&parts[1])
        || !(1..=31).contains(&parts[2])
    {
        return Err(invalid());
    }
    let mut date: libc::tm = unsafe { std::mem::zeroed() };
    date.tm_year = parts[0] - 1900;
    date.tm_mon = parts[1] - 1;
    date.tm_mday = parts[2];
    date.tm_isdst = -1;
    let value = unsafe { libc::mktime(&mut date) };
    if date.tm_year != parts[0] - 1900 || date.tm_mon != parts[1] - 1 || date.tm_mday != parts[2] {
        return Err(invalid());
    }
    if next_day {
        date.tm_mday += 1;
        date.tm_isdst = -1;
        Ok(unsafe { libc::mktime(&mut date) } as i64 * 1000)
    } else {
        Ok(value as i64 * 1000)
    }
}
pub fn today() -> String {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut date: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut date) };
    format!(
        "{:04}-{:02}-{:02}",
        date.tm_year + 1900,
        date.tm_mon + 1,
        date.tm_mday
    )
}
impl SavedFilter {
    pub fn effective_time_range(&self) -> TimeRange {
        if self.time_range == TimeRange::Any && self.recent {
            TimeRange::Week
        } else {
            self.time_range.clone()
        }
    }
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
    fn ranges_use_milliseconds_and_validate_calendar() {
        use super::TimeRange;
        for (range, days) in [
            (TimeRange::Days3, 3),
            (TimeRange::Week, 7),
            (TimeRange::Month, 30),
            (TimeRange::Year, 365),
        ] {
            let (start, end) = range.bounds_at(2_000_000_000_000).unwrap();
            assert_eq!(start, Some(2_000_000_000_000 - days * 86_400_000));
            assert_eq!(end, Some(2_000_000_000_001));
        }
        assert!(super::local_date("2025-02-29", false).is_err());
        assert!(super::local_date("2024-02-29", false).is_ok());
        assert!(super::local_date("2026-04-31", false).is_err());
        let range = TimeRange::Custom {
            start: "2026-10-12".into(),
            end: "2026-10-12".into(),
        };
        let (start, end) = range.bounds().unwrap();
        assert!(end.unwrap() > start.unwrap());
        assert_eq!(end, Some(super::local_date("2026-10-13", false).unwrap()));
        assert!(
            TimeRange::Custom {
                start: "2026-10-13".into(),
                end: "2026-10-12".into()
            }
            .bounds()
            .is_err()
        );
        let mut legacy = super::SavedFilter::recent_images();
        legacy.time_range = TimeRange::Any;
        assert_eq!(legacy.effective_time_range(), TimeRange::Week);
        legacy.recent = false;
        assert_eq!(legacy.effective_time_range(), TimeRange::Any);
    }
    #[test]
    fn saved_filter_restores_all_settings_and_relative_date() {
        let mut storage = Store::default();
        let mut preferences = super::Preferences::default();
        let mut filter = super::SavedFilter::recent_images();
        filter.name = "my outputs".into();
        filter.query = "/output baby".into();
        filter.time_range = super::TimeRange::Custom {
            start: "2024-01-01".into(),
            end: "2026-12-31".into(),
        };
        let expected = filter.time_range.clone();
        preferences.saved.push(filter);
        preferences.shortcut.shift = true;
        preferences.english = true;
        eframe::set_value(&mut storage, "prefs", &preferences);
        let restored: super::Preferences = eframe::get_value(&storage, "prefs").unwrap();
        assert!(restored.english);
        assert_eq!(restored.personal_roots, preferences.personal_roots);
        let f = &restored.saved[0];
        assert_eq!(f.time_range, expected);
        assert_eq!(f.query, "/output baby");
        assert!(f.personal && f.recent && f.thumbnails && f.raster && !f.paths);
        assert_eq!(f.category, miao_find::index::Category::Image);
        assert_eq!(f.order, miao_find::index::SortOrder::Latest);
        assert_eq!(restored.exclusions, preferences.exclusions);
        assert_eq!(restored.shortcut, preferences.shortcut);
    }
}

#[cfg(test)]
mod workflow_tests {
    use super::*;
    #[test]
    fn quick_searches_work_without_a_personal_scope() {
        let presets = SavedFilter::builtins();
        assert_eq!(presets.len(), 7);
        for preset in &presets {
            assert!(
                !preset.personal,
                "built-ins must search all indexed locations by default"
            );
            assert!(preset.time_range.bounds().is_ok());
            assert!(!preset.raster || preset.category == Category::Image);
        }
        assert!(presets.iter().any(|p| p.query == "/Downloads"));
        assert!(presets.iter().any(|p| p.query == ".pdf"));
        let empty = SavedFilter::empty();
        assert!(!empty.personal && empty.time_range == TimeRange::Any && empty.query.is_empty());
    }
    #[test]
    fn saved_search_and_unselected_custom_locations_survive_restart() {
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
        let mut filter = SavedFilter::empty();
        filter.name = "本周项目 PDF".into();
        filter.query = ".pdf".into();
        filter.category = Category::Document;
        filter.time_range = TimeRange::Week;
        filter.personal = true;
        let selected = PathBuf::from("/tmp/selected-project");
        let unselected = PathBuf::from("/tmp/other-project");
        let preferences = Preferences {
            personal_roots: vec![selected.clone()],
            scope_locations: vec![selected.clone(), unselected.clone()],
            saved: vec![filter],
            ..Default::default()
        };
        let mut store = Store::default();
        eframe::set_value(&mut store, "preferences", &preferences);
        let loaded: Preferences = eframe::get_value(&store, "preferences").unwrap();
        assert_eq!(loaded.personal_roots, vec![selected.clone()]);
        assert_eq!(loaded.scope_locations, vec![selected, unselected]);
        assert_eq!(loaded.saved[0].query, ".pdf");
        assert!(loaded.saved[0].personal);
        assert_eq!(loaded.saved[0].effective_time_range(), TimeRange::Week);
    }
}
