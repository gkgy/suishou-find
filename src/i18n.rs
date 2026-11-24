use std::sync::atomic::{AtomicBool, Ordering};
static ENGLISH: AtomicBool = AtomicBool::new(false);
pub fn set_english(value: bool) {
    ENGLISH.store(value, Ordering::Relaxed);
}
pub fn english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}
pub fn tr(text: &str) -> &str {
    if !english() {
        return text;
    }
    translate(text)
}
fn translate(text: &str) -> &str {
    match text {
        "随手找" => "Everywhere",
        "请选择一个文件夹建立索引" => "Choose a folder to create an index",
        "已载入 {count} 个文件和文件夹" => "Loaded {count} files and folders",
        "索引初始化失败：{error}" => "Could not initialize index: {error}",
        "快捷键设置" => "Keyboard Shortcut",
        "用一组按键，随时唤起或隐藏随手找。" => {
            "Show or hide Everywhere with a keyboard shortcut."
        }
        "当前组合" => "Current shortcut",
        "组合键" => "Modifiers",
        "主按键" => "Key",
        "选择至少一个 Command、Option 或 Control。功能键可能需要配合 Fn。" => {
            "Use Command, Option or Control. F keys may need Fn."
        }
        "保存并应用" => "Save and Apply",
        "快捷键已设置为 {}" => "Shortcut set to {}",
        "{e}；当前快捷键保留" => "{e}; previous shortcut retained",
        "取消" => "Cancel",
        "恢复默认" => "Restore Default",
        "我的搜索范围" => "My Search Locations",
        "勾选要查找的目录，应用后仅搜索这些位置。" => {
            "Select folders to search, then apply your selection."
        }
        "已选择 {} 个目录" => "{} folders selected",
        "全不选" => "Deselect All",
        "全选" => "Select All",
        "桌面" => "Desktop",
        "文稿" => "Documents",
        "下载" => "Downloads",
        "图片" => "Images",
        "影片" => "Movies",
        "音乐" => "Music",
        "添加其他目录…" => "Add Other Folder…",
        "高级：排除规则" => "Advanced: Exclusions",
        "每行一个目录名。.* 排除隐藏目录，*.app 排除应用包。" => {
            "One folder name per line. .* excludes hidden folders; *.app excludes app bundles."
        }
        "只筛选已经建立索引的位置。取消或关闭不会改变当前搜索范围。" => {
            "Only indexed locations are searched. Cancel or close to keep your current selection."
        }
        "应用范围" => "Apply Locations",
        "管理常用筛选" => "Manage Saved Filters",
        "删除已保存的筛选，不会删除文件。" => {
            "Removing a saved filter does not delete files."
        }
        "还没有保存的筛选。" => "No saved filters yet.",
        "删除筛选" => "Remove Filter",
        "新增常用筛选" => "New Saved Filter",
        "组合你常用的条件，保存后可一键搜索。" => {
            "Combine your usual search criteria and save them for quick access."
        }
        "名称" => "Name",
        "例如：本周报告、项目图片" => "Example: Weekly reports, Project images",
        "关键词" => "Keywords",
        "可留空；例如 报告、.pdf、/output" => "Optional; e.g. report, .pdf, /output",
        "文件类型" => "File Type",
        "修改日期" => "Date Modified",
        "自定义日期…" => "Custom Dates…",
        "日期起止" => "Date Range",
        "至" => "to",
        "搜索范围" => "Search Locations",
        "全部位置" => "All Locations",
        "选择目录…" => "Choose Folders…",
        "排序" => "Sort Order",
        "同时匹配路径" => "Match Paths",
        "显示缩略图" => "Show Thumbnails",
        "仅可预览图片" => "Previewable Images Only",
        "普通关键词匹配文件名，/目录名可定位项目或输出目录。" => {
            "Plain keywords match names; /folder matches a project or output folder."
        }
        "保存并使用" => "Save and Use",
        "关闭" => "Close",
        "日期缓存保存失败：{error}" => "Could not save date cache: {error}",
        "修改日期补充失败，请重新打开重试" => {
            "Could not load modification dates. Reopen the app to retry."
        }
        "没有可索引的位置" => "No locations to index",
        "正在建立全盘索引…" => "Indexing local disks…",
        "正在并行扫描 {} 个位置…" => "Scanning {} locations in parallel…",
        "正在建立索引：已发现 {count} 项…" => "Indexing: {count} items found…",
        "索引完成：{} 项，用时 {:.1} 秒" => "Indexed {} items in {:.1} seconds",
        "索引可搜索，但启动文件变化监听失败" => {
            "Search is available, but file monitoring could not start"
        }
        "索引可搜索，但无法监听 {}：{error}" => {
            "Search is available, but cannot monitor {}: {error}"
        }
        "文件变更历史不完整，如需核对全部日期，请重新建立索引" => {
            "File history is incomplete. Rebuild the index to verify all dates."
        }
        "文件变化监听异常：{error}" => "File monitoring error: {error}",
        "增量索引更新失败：{error}" => "Index update failed: {error}",
        "后台索引更新意外停止" => "Background index updates stopped unexpectedly",
        "正在后台同步文件变化…" => "Syncing file changes in the background…",
        "索引已自动更新，当前共 {} 项" => "Index updated: {} items",
        "选择文件查看预览" => "Select a file to preview",
        "打开" => "Open",
        "在访达中显示" => "Show in Finder",
        "正在载入预览…" => "Loading preview…",
        "此图片暂不能预览" => "Preview unavailable for this image",
        "可直接打开查看原文件。" => "Open the original file to view it.",
        "打开文件，或在访达中查看。" => "Open the file or show it in Finder.",
        "修改日期  {}" => "Modified  {}",
        "大小  {:.1} MB" => "Size  {:.1} MB",
        "尺寸  {} × {}" => "Dimensions  {} × {}",
        "无法调用系统打开功能：{error}" => "Could not open file: {error}",
        "无法确认打开结果：{error}" => "Could not confirm open result: {error}",
        "系统无法打开这个项目（状态码：{status}）" => {
            "System could not open this item (status: {status})"
        }
        "暂无日期" => "Unknown date",
        "自定义修改日期范围" => "Custom Modification Dates",
        "输入起止日期（YYYY-MM-DD），包含结束日期当天。" => {
            "Enter dates (YYYY-MM-DD). The end date is inclusive."
        }
        "开始日期" => "Start Date",
        "结束日期" => "End Date",
        "例如 2025-01-01 至 2026-12-31；使用本地时区。" => {
            "Example: 2025-01-01 to 2026-12-31, in your local time zone."
        }
        "应用日期范围" => "Apply Dates",
        "随手找 v{}" => "Everywhere v{}",
        "索引" => "Index",
        "全盘索引" => "Index Local Disks",
        "选择位置…" => "Choose Locations…",
        "添加位置…" => "Add Locations…",
        "重新建立索引" => "Rebuild Index",
        "开启后，父目录名称也参与匹配，例如用项目名查找项目内文件" => {
            "Include parent folder names, e.g. search a project name to find its files"
        }
        "{} {} · 双击打开" => "{} {} · Double-click to open",
        "唤起" => "to show",
        "未生效" => "inactive",
        "搜索名称和路径，空格组合关键词" => {
            "Search names and paths; separate keywords with spaces"
        }
        "搜索文件或文件夹名称，空格组合关键词" => {
            "Search file or folder names; separate keywords with spaces"
        }
        "我的搜索范围…" => "My Search Locations…",
        "不限时间" => "Any Time",
        "按修改时间筛选；一个月为30天，一年为365天；不是最近打开时间" => {
            "Filter by modification date; month = 30 days, year = 365 days, not last opened"
        }
        "常用筛选" => "Quick Filters",
        "快捷搜索" => "Quick Searches",
        "我的常用筛选" => "My Saved Filters",
        "还没有自定义筛选" => "No custom filters yet",
        "新增常用筛选…" => "New Saved Filter…",
        "保存当前筛选…" => "Save Current Filter…",
        "管理筛选…" => "Manage Filters…",
        "设置" => "Settings",
        "显示右侧预览" => "Show Preview Panel",
        "仅PNG / JPEG / WebP / GIF" => "PNG / JPEG / WebP / GIF only",
        "快捷键设置…" => "Keyboard Shortcut…",
        "缩略图" => "Thumbnails",
        "列表" => "List",
        "格式筛选：仅可预览图片（可在‘设置’中取消）" => {
            "Format filter: previewable images only (change in Settings)"
        }
        "我的搜索范围 · {} 个目录（可在‘范围’中查看）" => {
            "My Search Locations · {} folders (see location settings)"
        }
        "修改日期：{start} 至 {end}（包含结束当天）" => {
            "Modified: {start} to {end} (inclusive)"
        }
        "正在核对修改日期" => "Checking modification dates",
        "{} 个结果 · {:.2} ms" => "{} results · {:.2} ms",
        "当前 {}–{} · 滚动加载" => "Showing {}–{} · Scroll to load",
        "分类" => "Categories",
        "名称 ↑" => "Name ↑",
        "修改日期 ↓" => "Date Modified ↓",
        "修改日期 ↑" => "Date Modified ↑",
        "请选择位置建立文件名索引" => "Choose a location to index file names",
        "输入名称，开始找文件" => "Type a name to find files",
        "没有找到符合条件的文件" => "No matching files",
        "试试减少关键词，或放宽位置与日期范围。" => {
            "Try fewer keywords or broader locations and dates."
        }
        "正在加载…" => "Loading…",
        "复制完整路径" => "Copy Full Path",
        "全部" => "All",
        "文件夹" => "Folders",
        "文档" => "Documents",
        "音频" => "Audio",
        "视频" => "Video",
        "应用" => "Applications",
        "压缩包" => "Archives",
        "代码" => "Code",
        "其他" => "Other",
        "相关性优先" => "Relevance",
        "最新修改优先" => "Newest First",
        "最早修改优先" => "Oldest First",
        "名称升序" => "Name A–Z",
        "最近一周的文档" => "Documents This Week",
        "最近下载的文件" => "Recent Downloads",
        "最近一周的图片" => "Images This Week",
        "PDF 文档" => "PDF Documents",
        "最近修改的代码" => "Recently Modified Code",
        "最近生成的图片" => "Recently Generated Images",
        "最近 3 天" => "Past 3 Days",
        "最近一周" => "Past Week",
        "最近一个月" => "Past Month",
        "最近一年" => "Past Year",
        "开始日期不能晚于结束日期" => "Start date must not be after end date",
        "请输入有效日期，例如 2026-10-12（年份 1900–9999）" => {
            "Enter a valid date, e.g. 2026-10-12 (year 1900–9999)"
        }
        "至少选择 ⌘、⌥、⌃ 中的一个，避免影响普通输入" => {
            "Select at least one of ⌘, ⌥ or ⌃ to preserve normal typing"
        }
        "请选择空格、字母、数字或 F1–F12" => {
            "Select Space, a letter, a digit or F1–F12"
        }
        "快捷键无效：{e}" => "Invalid shortcut: {e}",
        "无法注册，可能被其他应用或系统占用：{e}；请换一个组合" => {
            "Could not register shortcut (possibly already in use): {e}; choose another combination"
        }
        "旧快捷键释放失败：{error}；新快捷键未保存{}" => {
            "Could not release old shortcut: {error}; new shortcut not saved{}"
        }
        "，请重启应用后重试" => "; restart the app to retry",
        "快捷键初始化失败：{e}" => "Could not initialize shortcut: {e}",
        _ => text,
    }
}
macro_rules! localized_format {
    ($zh:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if crate::i18n::english() { format!($en $(, $arg)*) }
        else { format!($zh $(, $arg)*) }
    };
}
pub(crate) use localized_format;
#[cfg(test)]
mod tests {
    #[test]
    fn labels_translate_and_user_text_is_preserved() {
        assert_eq!(super::translate("随手找"), "Everywhere");
        assert_eq!(super::translate("全部位置"), "All Locations");
        assert_eq!(super::translate("我的搜索范围"), "My Search Locations");
        assert_eq!(
            super::translate("my project 随手找.txt"),
            "my project 随手找.txt"
        );
    }
}
