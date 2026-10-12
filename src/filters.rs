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
    pub fn recent_images() -> Self {
        Self {
            name: "最近生成的图片".into(),
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
    pub shortcut: crate::shortcut::Shortcut,
    pub personal_roots: Vec<PathBuf>,
    pub saved: Vec<SavedFilter>,
    #[serde(default = "miao_find::index::default_personal_exclusions")]
    pub exclusions: Vec<String>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            shortcut: crate::shortcut::Shortcut::default(),
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
            Self::Any => "不限时间",
            Self::Days3 => "最近 3 天",
            Self::Week => "最近一周",
            Self::Month => "最近一个月",
            Self::Year => "最近一年",
            Self::Custom { .. } => "自定义日期…",
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
                    return Err("开始日期不能晚于结束日期".into());
                }
                return Ok((Some(start), Some(end)));
            }
        };
        Ok((Some(now - days * 86_400_000), Some(now + 1)))
    }
}
/// Local calendar boundaries, including DST changes. Never silently normalize invalid dates.
fn local_date(text: &str, next_day: bool) -> Result<i64, String> {
    let invalid = || "请输入有效日期，例如 2026-10-12（年份 1900–9999）".to_string();
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
        eframe::set_value(&mut storage, "prefs", &preferences);
        let restored: super::Preferences = eframe::get_value(&storage, "prefs").unwrap();
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
