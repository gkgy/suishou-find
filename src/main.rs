#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    path::PathBuf,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::Instant,
};

use eframe::egui::{self, Color32, RichText};
use miao_find::index::{self, Category, CompactIndex, IndexDelta, ScanMessage, SortOrder};
#[cfg(target_os = "macos")]
mod date_history;
mod design;
mod filters;
mod fonts;
mod i18n;
mod icons;
mod paging;
mod preview;
mod shortcut;
mod thumbnails;
use filters::{Preferences, SavedFilter, TimeRange};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use thumbnails::Thumbnails;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title(crate::i18n::localized_format!(
                "随手找 v{}",
                "Everywhere v{}",
                env!("CARGO_PKG_VERSION")
            )),
        ..Default::default()
    };

    eframe::run_native(
        "随手找",
        options,
        Box::new(|context| Ok(Box::new(MiaoFindApp::new(context)))),
    )
}

struct MiaoFindApp {
    preferences: Preferences,
    personal_mask: Vec<bool>,
    raster: bool,
    exclusion_text: String,
    personal: bool,
    time_range: TimeRange,
    date_editor: bool,
    date_start: String,
    date_end: String,
    date_feedback: String,
    thumbnails_enabled: bool,
    thumbnails: Thumbnails,
    preview: preview::Preview,
    icons: icons::Icons,
    filter_signature: String,
    scope_editor: bool,
    scope_draft: Vec<PathBuf>,
    filter_draft: SavedFilter,
    save_editor: bool,
    manage_filters: bool,
    bookmark_name: String,
    index: CompactIndex,
    results: Vec<usize>,
    paging: paging::Paging,
    selected_result: Option<usize>,
    roots: Vec<PathBuf>,
    query: String,
    last_query: String,
    category: Category,
    last_category: Category,
    include_paths: bool,
    last_include_paths: bool,
    sort_order: SortOrder,
    last_sort_order: SortOrder,
    #[cfg(target_os = "macos")]
    history: Option<date_history::History>,
    date_cursor: usize,
    date_receiver: Option<Receiver<Vec<(usize, i64)>>>,
    open_receiver: Option<Receiver<Result<(), String>>>,
    status: String,
    scanning: bool,
    scan_receiver: Option<Receiver<ScanMessage>>,
    watcher: Option<RecommendedWatcher>,
    watch_receiver: Option<Receiver<notify::Result<notify::Event>>>,
    update_receiver: Option<Receiver<Result<IndexDelta, String>>>,
    pending_paths: Vec<PathBuf>,
    last_watch_event: Option<Instant>,
    watcher_started: Option<Instant>,
    query_ms: f64,
    hotkey: Option<shortcut::Controller>,
    shortcut_editor: bool,
    shortcut_draft: shortcut::Shortcut,
    shortcut_feedback: String,
    hotkey_requested: Arc<AtomicBool>,
    focus_search: bool,
    app_context: egui::Context,
}

impl MiaoFindApp {
    fn new(context: &eframe::CreationContext<'_>) -> Self {
        install_visual_style(&context.egui_ctx);
        install_chinese_font(&context.egui_ctx);
        index::warm_up_parallel_search();
        let hotkey_requested = Arc::new(AtomicBool::new(false));
        let mut app = match index::load_cached() {
            Ok(index) => {
                let count = index.len();
                let roots = index.roots().to_vec();
                Self {
                    preferences: Preferences::default(),
                    personal_mask: Vec::new(),
                    raster: false,
                    exclusion_text: String::new(),
                    personal: false,
                    time_range: TimeRange::Any,
                    date_editor: false,
                    date_start: filters::today(),
                    date_end: filters::today(),
                    date_feedback: String::new(),
                    thumbnails_enabled: false,
                    thumbnails: Thumbnails::new(context.egui_ctx.clone()),
                    preview: preview::Preview::new(context.egui_ctx.clone()),
                    icons: icons::Icons::new(&context.egui_ctx),
                    filter_signature: String::new(),
                    scope_editor: false,
                    scope_draft: Vec::new(),
                    filter_draft: SavedFilter::empty(),
                    save_editor: false,
                    manage_filters: false,
                    bookmark_name: String::new(),
                    index,
                    results: Vec::new(),
                    paging: paging::Paging::new(context.egui_ctx.clone()),
                    selected_result: None,
                    roots,
                    query: String::new(),
                    last_query: String::new(),
                    category: Category::All,
                    last_category: Category::All,
                    include_paths: false,
                    last_include_paths: false,
                    sort_order: SortOrder::Relevance,
                    last_sort_order: SortOrder::Relevance,
                    #[cfg(target_os = "macos")]
                    history: None,
                    date_cursor: 0,
                    date_receiver: None,
                    open_receiver: None,
                    status: if count == 0 {
                        crate::i18n::tr("请选择一个文件夹建立索引").to_owned()
                    } else {
                        crate::i18n::localized_format!(
                            "已载入 {count} 个文件和文件夹",
                            "Loaded {count} files and folders"
                        )
                    },
                    scanning: false,
                    scan_receiver: None,
                    watcher: None,
                    watch_receiver: None,
                    update_receiver: None,
                    pending_paths: Vec::new(),
                    last_watch_event: None,
                    watcher_started: None,
                    query_ms: 0.0,
                    hotkey: None,
                    shortcut_editor: false,
                    shortcut_draft: shortcut::Shortcut::default(),
                    shortcut_feedback: String::new(),
                    hotkey_requested,
                    focus_search: false,
                    app_context: context.egui_ctx.clone(),
                }
            }
            Err(error) => Self {
                preferences: Preferences::default(),
                personal_mask: Vec::new(),
                raster: false,
                exclusion_text: String::new(),
                personal: false,
                time_range: TimeRange::Any,
                date_editor: false,
                date_start: filters::today(),
                date_end: filters::today(),
                date_feedback: String::new(),
                thumbnails_enabled: false,
                thumbnails: Thumbnails::new(context.egui_ctx.clone()),
                preview: preview::Preview::new(context.egui_ctx.clone()),
                icons: icons::Icons::new(&context.egui_ctx),
                filter_signature: String::new(),
                scope_editor: false,
                scope_draft: Vec::new(),
                filter_draft: SavedFilter::empty(),
                save_editor: false,
                manage_filters: false,
                bookmark_name: String::new(),
                index: CompactIndex::default(),
                results: Vec::new(),
                paging: paging::Paging::new(context.egui_ctx.clone()),
                selected_result: None,
                roots: Vec::new(),
                query: String::new(),
                last_query: String::new(),
                category: Category::All,
                last_category: Category::All,
                include_paths: false,
                last_include_paths: false,
                sort_order: SortOrder::Relevance,
                last_sort_order: SortOrder::Relevance,
                #[cfg(target_os = "macos")]
                history: None,
                date_cursor: 0,
                date_receiver: None,
                open_receiver: None,
                status: crate::i18n::localized_format!(
                    "索引初始化失败：{error}",
                    "Could not initialize index: {error}"
                ),
                scanning: false,
                scan_receiver: None,
                watcher: None,
                watch_receiver: None,
                update_receiver: None,
                pending_paths: Vec::new(),
                last_watch_event: None,
                watcher_started: None,
                query_ms: 0.0,
                hotkey: None,
                shortcut_editor: false,
                shortcut_draft: shortcut::Shortcut::default(),
                shortcut_feedback: String::new(),
                hotkey_requested,
                focus_search: false,
                app_context: context.egui_ctx.clone(),
            },
        };
        app.preferences = context
            .storage
            .and_then(|s| eframe::get_value(s, "search_preferences_v1"))
            .unwrap_or_default();
        i18n::set_english(app.preferences.english);
        if app.preferences.english {
            app.status = format!("Index ready: {} items", app.index.len());
        }
        context
            .egui_ctx
            .send_viewport_cmd(egui::ViewportCommand::Title(
                crate::i18n::localized_format!(
                    "随手找 v{}",
                    "Everywhere v{}",
                    env!("CARGO_PKG_VERSION")
                ),
            ));
        for root in filters::default_roots()
            .into_iter()
            .chain(app.preferences.personal_roots.clone())
        {
            if !app.preferences.scope_locations.contains(&root) {
                app.preferences.scope_locations.push(root);
            }
        }
        app.personal_mask = app.index.personal_mask_with_exclusions(
            &app.preferences.personal_roots,
            &app.preferences.exclusions,
        );
        let (controller, hotkey_status) = shortcut::Controller::new(
            &context.egui_ctx,
            Arc::clone(&app.hotkey_requested),
            &app.preferences.shortcut,
        );
        app.hotkey = Some(controller);
        if let Some(error) = hotkey_status {
            app.shortcut_feedback = error.clone();
            app.status = error;
        }
        #[cfg(target_os = "macos")]
        {
            app.history = date_history::History::start(&app.index, app.app_context.clone());
        }
        app.start_watcher();
        app.start_dates();
        let (sender, receiver) = mpsc::channel();
        app.update_receiver = Some(receiver);
        let repaint = app.app_context.clone();
        std::thread::spawn(move || {
            let _ = sender.send(index::pending_journal_delta());
            repaint.request_repaint();
        });
        app
    }

    fn filter_windows(&mut self, context: &egui::Context) {
        if self.shortcut_editor {
            let mut close = false;
            settings_window(crate::i18n::tr("快捷键设置"), 480.0).show(context, |ui| {
                close |= self.dialog_header(
                    ui,
                    crate::i18n::tr("快捷键设置"),
                    crate::i18n::tr("用一组按键，随时唤起或隐藏随手找。"),
                    "keyboard",
                );
                ui.add_space(18.0);
                egui::Frame::new()
                    .fill(Color32::from_rgb(241, 248, 249))
                    .inner_margin(18)
                    .show(ui, |ui| {
                        ui.set_min_width(420.0);
                        ui.set_max_width(ui.available_width());
                        ui.vertical_centered(|ui| {
                            ui.label(
                                RichText::new(crate::i18n::tr("当前组合"))
                                    .size(design::CAPTION)
                                    .color(design::SECONDARY),
                            );
                            ui.add_space(8.0);
                            ui.label(
                                design::medium(self.shortcut_draft.label(), 28.0)
                                    .color(design::ACCENT_TEXT),
                            );
                        });
                    });
                ui.add_space(16.0);
                ui.label(design::medium(crate::i18n::tr("组合键"), design::BODY));
                ui.horizontal(|ui| {
                    let column_width = (ui.available_width() - 12.0) / 2.0;
                    for (flag, label) in [
                        (&mut self.shortcut_draft.command, "⌘ Command"),
                        (&mut self.shortcut_draft.option, "⌥ Option"),
                    ] {
                        if ui
                            .add_sized(
                                [column_width, design::CONTROL_HEIGHT],
                                egui::Button::new(label).selected(*flag),
                            )
                            .clicked()
                        {
                            *flag = !*flag;
                        }
                    }
                });
                ui.horizontal(|ui| {
                    let column_width = (ui.available_width() - 12.0) / 2.0;
                    for (flag, label) in [
                        (&mut self.shortcut_draft.control, "⌃ Control"),
                        (&mut self.shortcut_draft.shift, "⇧ Shift"),
                    ] {
                        if ui
                            .add_sized(
                                [column_width, design::CONTROL_HEIGHT],
                                egui::Button::new(label).selected(*flag),
                            )
                            .clicked()
                        {
                            *flag = !*flag;
                        }
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(80.0, 36.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.label(design::medium(crate::i18n::tr("主按键"), design::BODY));
                        },
                    );
                    egui::ComboBox::from_id_salt("shortcut_key")
                        .width(ui.available_width())
                        .selected_text(&self.shortcut_draft.key)
                        .show_ui(ui, |ui| {
                            for key in shortcut::Shortcut::keys() {
                                if ui
                                    .selectable_value(
                                        &mut self.shortcut_draft.key,
                                        key.clone(),
                                        key,
                                    )
                                    .clicked()
                                {
                                    ui.close();
                                }
                            }
                        });
                });
                ui.add_space(8.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(crate::i18n::tr(
                            "选择至少一个 Command、Option 或 Control。功能键可能需要配合 Fn。",
                        ))
                        .size(design::CAPTION)
                        .color(design::SECONDARY),
                    )
                    .wrap(),
                );
                if !self.shortcut_feedback.is_empty() {
                    ui.add_space(6.0);
                    ui.add(egui::Label::new(&self.shortcut_feedback).wrap());
                }
                ui.add_space(14.0);
                ui.separator();
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            self.shortcut_draft.parse().is_ok(),
                            primary_button(crate::i18n::tr("保存并应用"))
                                .min_size(egui::vec2(140.0, 36.0)),
                        )
                        .clicked()
                    {
                        match self.hotkey.as_mut().unwrap().apply(&self.shortcut_draft) {
                            Ok(()) => {
                                self.preferences.shortcut = self.shortcut_draft.clone();
                                self.status = crate::i18n::localized_format!(
                                    "快捷键已设置为 {}",
                                    "Shortcut set to {}",
                                    self.preferences.shortcut.label()
                                );
                                close = true;
                            }
                            Err(e) => {
                                self.shortcut_feedback = crate::i18n::localized_format!(
                                    "{e}；当前快捷键保留",
                                    "{e}; previous shortcut retained"
                                )
                            }
                        }
                    }
                    if ui
                        .add_sized([90.0, 36.0], egui::Button::new(crate::i18n::tr("取消")))
                        .clicked()
                    {
                        close = true;
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(crate::i18n::tr("恢复默认")).frame(false))
                            .clicked()
                        {
                            self.shortcut_draft = shortcut::Shortcut::default();
                            self.shortcut_feedback.clear();
                        }
                    });
                });
            });
            if close
                || context.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            {
                self.shortcut_editor = false;
            }
        }
        if self.scope_editor {
            let mut close = false;
            let mut apply = false;
            settings_window(crate::i18n::tr("我的搜索范围"), 560.0).show(context, |ui| {
                close |= self.dialog_header(
                    ui,
                    crate::i18n::tr("我的搜索范围"),
                    crate::i18n::tr("勾选要查找的目录，应用后仅搜索这些位置。"),
                    "folder",
                );
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    ui.label(design::medium(
                        crate::i18n::localized_format!(
                            "已选择 {} 个目录",
                            "{} folders selected",
                            self.scope_draft.len()
                        ),
                        design::BODY,
                    ));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(crate::i18n::tr("全不选")).clicked() {
                            self.scope_draft.clear();
                        }
                        if ui.button(crate::i18n::tr("全选")).clicked() {
                            self.scope_draft = self.preferences.scope_locations.clone();
                        }
                    });
                });
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .id_salt("search_scope_choices_v2")
                    .max_height((context.content_rect().height() - 350.0).clamp(120.0, 248.0))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for p in &self.preferences.scope_locations {
                            ui.horizontal(|ui| {
                                let mut selected = self.scope_draft.contains(p);
                                ui.add(self.icons.image("folder", 20.0, design::SECONDARY));
                                let short = short_path(&p.to_string_lossy());
                                let label = p
                                    .file_name()
                                    .map(|v| v.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| short.clone());
                                ui.allocate_ui_with_layout(
                                    egui::vec2(ui.available_width(), 44.0),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        ui.spacing_mut().item_spacing.y = 4.0;
                                        let display = match label.as_str() {
                                            "Desktop" => crate::i18n::tr("桌面"),
                                            "Documents" => crate::i18n::tr("文稿"),
                                            "Downloads" => crate::i18n::tr("下载"),
                                            "Pictures" => crate::i18n::tr("图片"),
                                            "Movies" => crate::i18n::tr("影片"),
                                            "Music" => crate::i18n::tr("音乐"),
                                            _ => &label,
                                        };
                                        ui.spacing_mut().interact_size.y = 20.0;
                                        if design::checkbox(
                                            ui,
                                            &mut selected,
                                            design::medium(display, design::BODY),
                                        )
                                        .changed()
                                        {
                                            if selected {
                                                self.scope_draft.push(p.clone());
                                            } else {
                                                self.scope_draft.retain(|root| root != p);
                                            }
                                        }
                                        ui.horizontal(|ui| {
                                            ui.add_space(24.0);
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(&short)
                                                        .size(design::CAPTION)
                                                        .color(design::SECONDARY),
                                                )
                                                .truncate(),
                                            )
                                            .on_hover_text(p.to_string_lossy());
                                        });
                                    },
                                );
                            });
                            ui.separator();
                            ui.add_space(12.0);
                        }
                    });
                ui.add_space(12.0);
                if ui
                    .add_sized(
                        [140.0, design::CONTROL_HEIGHT],
                        egui::Button::new(crate::i18n::tr("添加其他目录…")),
                    )
                    .clicked()
                {
                    if let Some(paths) = rfd::FileDialog::new().pick_folders() {
                        for path in paths {
                            if !self.preferences.scope_locations.contains(&path) {
                                self.preferences.scope_locations.push(path.clone());
                            }
                            if !self.scope_draft.contains(&path) {
                                self.scope_draft.push(path);
                            }
                        }
                    }
                }
                ui.add_space(8.0);
                ui.collapsing(crate::i18n::tr("高级：排除规则"), |ui| {
                    ui.label(
                        RichText::new(crate::i18n::tr(
                            "每行一个目录名。.* 排除隐藏目录，*.app 排除应用包。",
                        ))
                        .size(design::CAPTION)
                        .color(design::SECONDARY),
                    );
                    ui.add(
                        egui::TextEdit::multiline(&mut self.exclusion_text)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY),
                    );
                });
                ui.add_space(8.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(crate::i18n::tr(
                            "只筛选已经建立索引的位置。取消或关闭不会改变当前搜索范围。",
                        ))
                        .size(design::CAPTION)
                        .color(design::SECONDARY),
                    )
                    .wrap(),
                );
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(6.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            !self.scope_draft.is_empty(),
                            primary_button(crate::i18n::tr("应用范围"))
                                .min_size(egui::vec2(110.0, design::CONTROL_HEIGHT)),
                        )
                        .clicked()
                    {
                        apply = true;
                        close = true;
                    }
                    if ui
                        .add_sized(
                            [90.0, design::CONTROL_HEIGHT],
                            egui::Button::new(crate::i18n::tr("取消")),
                        )
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
            if apply {
                self.preferences.personal_roots = self.scope_draft.clone();
                self.preferences.exclusions = self
                    .exclusion_text
                    .lines()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect();
                self.personal_mask = self.index.personal_mask_with_exclusions(
                    &self.preferences.personal_roots,
                    &self.preferences.exclusions,
                );
                self.personal = true;
                if self.save_editor {
                    self.filter_draft.personal = true;
                }
                self.filter_signature.clear();
            }
            if close
                || context.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            {
                self.scope_editor = false;
            }
        }
        if self.manage_filters {
            let mut open = true;
            egui::Window::new(crate::i18n::tr("管理常用筛选"))
                .open(&mut open)
                .default_width(420.0)
                .show(context, |ui| {
                    ui.label(crate::i18n::tr("删除已保存的筛选，不会删除文件。"));
                    if self.preferences.saved.is_empty() {
                        ui.label(crate::i18n::tr("还没有保存的筛选。"));
                    }
                    let mut remove = None;
                    for (i, filter) in self.preferences.saved.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(&filter.name);
                            if ui.button(crate::i18n::tr("删除筛选")).clicked() {
                                remove = Some(i);
                            }
                        });
                    }
                    if let Some(i) = remove {
                        self.preferences.saved.remove(i);
                    }
                });
            self.manage_filters = open;
        }
        if self.save_editor && !self.scope_editor {
            let mut close = false;
            let mut save = false;
            settings_window(crate::i18n::tr("新增常用筛选"), 560.0).show(context, |ui| {
                close |= self.dialog_header(
                    ui,
                    crate::i18n::tr("新增常用筛选"),
                    crate::i18n::tr("组合你常用的条件，保存后可一键搜索。"),
                    "settings",
                );
                ui.add_space(16.0);
                egui::Grid::new("filter_builder_fields")
                    .num_columns(2)
                    .spacing([16.0, 12.0])
                    .show(ui, |ui| {
                        ui.label(design::medium(crate::i18n::tr("名称"), design::BODY));
                        ui.add_sized(
                            [400.0, 34.0],
                            egui::TextEdit::singleline(&mut self.filter_draft.name)
                                .vertical_align(egui::Align::Center)
                                .hint_text(crate::i18n::tr("例如：本周报告、项目图片")),
                        );
                        ui.end_row();
                        ui.label(design::medium(crate::i18n::tr("关键词"), design::BODY));
                        ui.add_sized(
                            [400.0, 34.0],
                            egui::TextEdit::singleline(&mut self.filter_draft.query)
                                .vertical_align(egui::Align::Center)
                                .hint_text(crate::i18n::tr("可留空；例如 报告、.pdf、/output")),
                        );
                        ui.end_row();
                        ui.label(design::medium(crate::i18n::tr("文件类型"), design::BODY));
                        egui::ComboBox::from_id_salt("builder_category")
                            .width(400.0)
                            .selected_text(crate::i18n::tr(self.filter_draft.category.label()))
                            .show_ui(ui, |ui| {
                                for category in Category::ALL {
                                    ui.selectable_value(
                                        &mut self.filter_draft.category,
                                        category,
                                        crate::i18n::tr(category.label()),
                                    );
                                }
                            });
                        ui.end_row();
                        ui.label(design::medium(crate::i18n::tr("修改日期"), design::BODY));
                        egui::ComboBox::from_id_salt("builder_time")
                            .width(400.0)
                            .selected_text(self.filter_draft.time_range.label())
                            .show_ui(ui, |ui| {
                                for range in [
                                    TimeRange::Any,
                                    TimeRange::Days3,
                                    TimeRange::Week,
                                    TimeRange::Month,
                                    TimeRange::Year,
                                ] {
                                    let label = range.label().to_owned();
                                    ui.selectable_value(
                                        &mut self.filter_draft.time_range,
                                        range,
                                        label,
                                    );
                                }
                                if ui
                                    .selectable_label(
                                        matches!(
                                            self.filter_draft.time_range,
                                            TimeRange::Custom { .. }
                                        ),
                                        crate::i18n::tr("自定义日期…"),
                                    )
                                    .clicked()
                                {
                                    self.filter_draft.time_range = TimeRange::Custom {
                                        start: filters::today(),
                                        end: filters::today(),
                                    };
                                }
                            });
                        ui.end_row();
                        if let TimeRange::Custom { start, end } = &mut self.filter_draft.time_range
                        {
                            ui.label(crate::i18n::tr("日期起止"));
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(start)
                                        .vertical_align(egui::Align::Center)
                                        .desired_width(160.0),
                                );
                                ui.label(crate::i18n::tr("至"));
                                ui.add(
                                    egui::TextEdit::singleline(end)
                                        .vertical_align(egui::Align::Center)
                                        .desired_width(160.0),
                                );
                            });
                            ui.end_row();
                        }
                        ui.label(design::medium(crate::i18n::tr("搜索范围"), design::BODY));
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut self.filter_draft.personal,
                                false,
                                crate::i18n::tr("全部位置"),
                            );
                            ui.add_enabled_ui(!self.preferences.personal_roots.is_empty(), |ui| {
                                ui.selectable_value(
                                    &mut self.filter_draft.personal,
                                    true,
                                    crate::i18n::tr("我的搜索范围"),
                                );
                            });
                            if ui.button(crate::i18n::tr("选择目录…")).clicked() {
                                self.open_scope_editor();
                            }
                        });
                        ui.end_row();
                        ui.label(design::medium(crate::i18n::tr("排序"), design::BODY));
                        egui::ComboBox::from_id_salt("builder_order")
                            .width(400.0)
                            .selected_text(crate::i18n::tr(self.filter_draft.order.label()))
                            .show_ui(ui, |ui| {
                                for order in [
                                    SortOrder::Relevance,
                                    SortOrder::Latest,
                                    SortOrder::Oldest,
                                    SortOrder::Name,
                                ] {
                                    ui.selectable_value(
                                        &mut self.filter_draft.order,
                                        order,
                                        crate::i18n::tr(order.label()),
                                    );
                                }
                            });
                        ui.end_row();
                    });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    design::checkbox(
                        ui,
                        &mut self.filter_draft.paths,
                        crate::i18n::tr("同时匹配路径"),
                    );
                    design::checkbox(
                        ui,
                        &mut self.filter_draft.thumbnails,
                        crate::i18n::tr("显示缩略图"),
                    );
                    ui.add_enabled_ui(self.filter_draft.category == Category::Image, |ui| {
                        design::checkbox(
                            ui,
                            &mut self.filter_draft.raster,
                            crate::i18n::tr("仅可预览图片"),
                        );
                    });
                });
                if self.filter_draft.category != Category::Image {
                    self.filter_draft.raster = false;
                }
                ui.add_space(8.0);
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "普通关键词匹配文件名，/目录名可定位项目或输出目录。",
                    ))
                    .size(design::CAPTION)
                    .color(design::SECONDARY),
                );
                if let Err(error) = self.filter_draft.time_range.bounds() {
                    ui.colored_label(Color32::from_rgb(180, 60, 50), error);
                }
                ui.add_space(16.0);
                ui.separator();
                ui.add_space(6.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let valid = !self.filter_draft.name.trim().is_empty()
                        && self.filter_draft.time_range.bounds().is_ok()
                        && (!self.filter_draft.personal
                            || !self.preferences.personal_roots.is_empty());
                    if ui
                        .add_enabled(
                            valid,
                            primary_button(crate::i18n::tr("保存并使用"))
                                .min_size(egui::vec2(130.0, design::CONTROL_HEIGHT)),
                        )
                        .clicked()
                    {
                        save = true;
                        close = true;
                    }
                    if ui
                        .add_sized(
                            [90.0, design::CONTROL_HEIGHT],
                            egui::Button::new(crate::i18n::tr("取消")),
                        )
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
            if save {
                let mut filter = self.filter_draft.clone();
                filter.name = filter.name.trim().to_owned();
                filter.recent = false;
                if let Some(existing) = self
                    .preferences
                    .saved
                    .iter_mut()
                    .find(|f| f.name == filter.name)
                {
                    *existing = filter.clone();
                } else {
                    self.preferences.saved.push(filter.clone());
                }
                self.apply_filter(filter);
            }
            if close
                || context.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            {
                self.save_editor = false;
            }
        }
    }

    fn open_scope_editor(&mut self) {
        self.scope_draft = self.preferences.personal_roots.clone();
        self.exclusion_text = self.preferences.exclusions.join("\n");
        self.scope_editor = true;
    }
    fn open_filter_editor(&mut self, current: bool) {
        self.filter_draft = if current {
            self.current_filter()
        } else {
            SavedFilter::empty()
        };
        self.filter_draft.name.clear();
        self.save_editor = true;
    }

    fn dialog_header(&self, ui: &mut egui::Ui, title: &str, subtitle: &str, icon: &str) -> bool {
        let close = ui
            .horizontal(|ui| {
                ui.add(self.icons.image(icon, 24.0, design::ACCENT_TEXT));
                ui.label(design::semibold(title, design::TITLE));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(
                        egui::Button::image_and_text(
                            self.icons.image("x", 18.0, design::SECONDARY),
                            crate::i18n::tr("关闭"),
                        )
                        .frame(false),
                    )
                    .on_hover_text(crate::i18n::tr("关闭"))
                    .clicked()
                })
                .inner
            })
            .inner;
        ui.add_space(8.0);
        ui.add(
            egui::Label::new(RichText::new(subtitle).size(14.0).color(design::SECONDARY)).wrap(),
        );
        close
    }

    fn start_dates(&mut self) {
        if self.index.is_empty() || self.index.dates_ready() {
            return;
        }
        let paths = self.index.missing_date_batch(&mut self.date_cursor);
        if paths.is_empty() {
            if let Err(error) = self.index.finish_date_repair() {
                self.status = crate::i18n::localized_format!(
                    "日期缓存保存失败：{error}",
                    "Could not save date cache: {error}"
                );
            }
            self.last_query.clear();
            self.filter_signature.clear();
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.date_receiver = Some(receiver);
        let context = self.app_context.clone();
        std::thread::spawn(move || {
            let _ = sender.send(index::read_date_batch(paths));
            context.request_repaint();
        });
    }
    fn receive_dates(&mut self, context: &egui::Context) {
        if let Some(receiver) = self.date_receiver.take() {
            match receiver.try_recv() {
                Ok(dates) => {
                    self.index.apply_date_batch(dates);
                    self.start_dates();
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.date_receiver = Some(receiver);
                    context.request_repaint_after(std::time::Duration::from_millis(100));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.status = crate::i18n::tr("修改日期补充失败，请重新打开重试").into()
                }
            }
        }
    }

    fn choose_roots(&mut self, replace: bool) {
        if self.scanning {
            return;
        }
        let mut dialog = rfd::FileDialog::new();
        if let Some(root) = self.roots.first() {
            dialog = dialog.set_directory(root);
        }
        if let Some(folders) = dialog.pick_folders() {
            let mut roots = if replace {
                Vec::new()
            } else {
                self.roots.clone()
            };
            roots.extend(folders);
            self.start_scan(roots);
        }
    }

    fn start_scan(&mut self, roots: Vec<PathBuf>) {
        let roots = index::normalize_roots(roots);
        if roots.is_empty() {
            self.status = crate::i18n::tr("没有可索引的位置").to_owned();
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.scanning = true;
        self.status = if roots == index::full_disk_roots() {
            crate::i18n::tr("正在建立全盘索引…").to_owned()
        } else {
            crate::i18n::localized_format!(
                "正在并行扫描 {} 个位置…",
                "Scanning {} locations in parallel…",
                roots.len()
            )
        };
        self.scan_receiver = Some(receiver);
        index::scan(roots, sender);
    }

    fn receive_scan_updates(&mut self, context: &egui::Context) {
        let Some(receiver) = self.scan_receiver.take() else {
            return;
        };
        let mut keep_receiver = true;
        while let Ok(message) = receiver.try_recv() {
            match message {
                ScanMessage::Progress(count) => {
                    self.status = crate::i18n::localized_format!(
                        "正在建立索引：已发现 {count} 项…",
                        "Indexing: {count} items found…"
                    );
                    context.request_repaint();
                }
                ScanMessage::Finished { index, elapsed_ms } => {
                    self.roots = index.roots().to_vec();
                    self.index = index;
                    #[cfg(target_os = "macos")]
                    {
                        self.history = date_history::History::after_rebuild(&self.index);
                    }
                    self.date_receiver = None;
                    self.pending_paths.clear();
                    self.update_receiver = None;
                    self.scanning = false;
                    self.last_query.clear();
                    self.filter_signature.clear();
                    self.personal_mask = self.index.personal_mask_with_exclusions(
                        &self.preferences.personal_roots,
                        &self.preferences.exclusions,
                    );
                    self.status = crate::i18n::localized_format!(
                        "索引完成：{} 项，用时 {:.1} 秒",
                        "Indexed {} items in {:.1} seconds",
                        self.index.len(),
                        elapsed_ms as f64 / 1000.0
                    );
                    self.start_watcher();
                    keep_receiver = false;
                }
                ScanMessage::Failed(error) => {
                    self.status = error;
                    self.scanning = false;
                    keep_receiver = false;
                }
            }
        }
        if keep_receiver {
            self.scan_receiver = Some(receiver);
        }
    }

    fn start_watcher(&mut self) {
        self.watcher = None;
        self.watch_receiver = None;
        if self.roots.is_empty() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let repaint = self.app_context.clone();
        let watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
                Ok(event) if is_index_relevant(&event.kind) => {
                    let _ = sender.send(Ok(event));
                    repaint.request_repaint();
                }
                Err(error) => {
                    let _ = sender.send(Err(error));
                    repaint.request_repaint();
                }
                _ => {}
            });
        let Ok(mut watcher) = watcher else {
            self.status = crate::i18n::tr("索引可搜索，但启动文件变化监听失败").to_owned();
            return;
        };
        for root in &self.roots {
            if let Err(error) = watcher.watch(root, RecursiveMode::Recursive) {
                self.status = crate::i18n::localized_format!(
                    "索引可搜索，但无法监听 {}：{error}",
                    "Search is available, but cannot monitor {}: {error}",
                    root.display()
                );
                return;
            }
        }
        self.watcher = Some(watcher);
        self.watch_receiver = Some(receiver);
        self.watcher_started = Some(Instant::now());
    }

    fn receive_file_updates(&mut self, context: &egui::Context) {
        let Some(receiver) = self.watch_receiver.take() else {
            return;
        };
        while let Ok(event) = receiver.try_recv() {
            match event {
                Ok(event) => {
                    if self.watcher_started.is_none_or(|started| {
                        started.elapsed() >= std::time::Duration::from_secs(2)
                    }) {
                        self.pending_paths.extend(event.paths);
                        self.last_watch_event = Some(Instant::now());
                    }
                }
                Err(error) => {
                    self.status = crate::i18n::localized_format!(
                        "文件变化监听异常：{error}",
                        "File monitoring error: {error}"
                    )
                }
            }
        }
        self.watch_receiver = Some(receiver);

        if let Some(update_receiver) = self.update_receiver.take() {
            match update_receiver.try_recv() {
                Ok(Ok(delta)) => self.apply_delta(delta),
                Ok(Err(error)) => {
                    self.status = crate::i18n::localized_format!(
                        "增量索引更新失败：{error}",
                        "Index update failed: {error}"
                    )
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.update_receiver = Some(update_receiver);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.status = crate::i18n::tr("后台索引更新意外停止").to_owned();
                }
            }
        }

        if self.scanning || self.update_receiver.is_some() || self.pending_paths.is_empty() {
            if self.update_receiver.is_some() || !self.pending_paths.is_empty() {
                context.request_repaint_after(std::time::Duration::from_millis(100));
            }
            return;
        }
        if self
            .last_watch_event
            .is_some_and(|last| last.elapsed() < std::time::Duration::from_millis(250))
        {
            context.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }

        let paths = coalesce_paths(std::mem::take(&mut self.pending_paths), &self.roots);
        if !paths.is_empty() {
            let (sender, receiver) = mpsc::channel();
            index::update_paths(paths, sender);
            self.update_receiver = Some(receiver);
            self.status = crate::i18n::tr("正在后台同步文件变化…").to_owned();
            context.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn apply_delta(&mut self, delta: IndexDelta) {
        if delta.changed() == 0 {
            return;
        }
        match self.index.apply_delta(delta) {
            Ok(()) => {
                self.last_query.clear();
                self.filter_signature.clear();
                self.personal_mask = self.index.personal_mask_with_exclusions(
                    &self.preferences.personal_roots,
                    &self.preferences.exclusions,
                );
                self.status = crate::i18n::localized_format!(
                    "索引已自动更新，当前共 {} 项",
                    "Index updated: {} items",
                    self.index.len()
                );
            }
            Err(error) => {
                self.status = crate::i18n::localized_format!(
                    "增量索引更新失败：{error}",
                    "Index update failed: {error}"
                )
            }
        }
    }

    fn apply_filter(&mut self, filter: SavedFilter) {
        self.time_range = filter.effective_time_range();
        self.query = filter.query;
        self.category = filter.category;
        self.sort_order = filter.order;
        self.include_paths = filter.paths;
        self.personal = filter.personal && !self.preferences.personal_roots.is_empty();
        if filter.personal && self.preferences.personal_roots.is_empty() {
            self.open_scope_editor();
        }
        self.thumbnails_enabled = filter.thumbnails;
        self.raster = filter.raster;
        self.filter_signature.clear();
    }
    fn current_filter(&self) -> SavedFilter {
        SavedFilter {
            name: self.bookmark_name.trim().into(),
            query: self.query.clone(),
            category: self.category,
            order: self.sort_order,
            paths: self.include_paths,
            personal: self.personal,
            recent: false,
            time_range: self.time_range.clone(),
            thumbnails: self.thumbnails_enabled,
            raster: self.raster,
        }
    }
    fn update_search(&mut self) {
        let (since, until) = self.time_range.bounds().unwrap_or((Some(0), Some(0)));
        let signature = format!(
            "{}:{:?}:{}:{}",
            self.personal,
            self.time_range,
            self.raster,
            if self.time_range.relative() {
                since.unwrap_or_default() / 60_000
            } else {
                0
            }
        );

        if self.query == self.last_query
            && self.category == self.last_category
            && self.sort_order == self.last_sort_order
            && self.include_paths == self.last_include_paths
            && signature == self.filter_signature
        {
            return;
        }
        let started = Instant::now();
        let mut session = index::SearchSession::new(
            &self.index,
            &self.query,
            self.category,
            self.sort_order,
            self.include_paths,
            self.personal.then_some(self.personal_mask.as_slice()),
            since,
            until,
            self.raster,
        );
        let key = format!(
            "{:?}:{:?}:{:?}:{}:{}:{:?}",
            self.query,
            self.category,
            self.sort_order,
            self.include_paths,
            signature,
            if self.personal {
                self.preferences.personal_roots.as_slice()
            } else {
                &[]
            }
        );
        let page = self.paging.refreshed_page(&key, session.total());
        self.results = session.page(page, paging::PAGE_SIZE);
        self.paging.reset(session, &self.results, page, key);
        if let Some(position) = self
            .selected_result
            .and_then(|id| self.results.iter().position(|v| *v == id))
        {
            self.paging.selection_row = page * paging::PAGE_SIZE + position;
        } else {
            self.selected_result = self.paging.id(self.paging.selection_row);
        }
        self.query_ms = started.elapsed().as_secs_f64() * 1_000.0;
        self.filter_signature = signature;
        self.last_query.clone_from(&self.query);
        self.last_category = self.category;
        self.last_sort_order = self.sort_order;
        self.last_include_paths = self.include_paths;
    }

    fn move_selection(&mut self, direction: isize) {
        let row = self
            .paging
            .selection_row
            .saturating_add_signed(direction)
            .min(self.paging.total.saturating_sub(1));
        self.paging.select(row, &mut self.selected_result);
    }

    fn preview_ui(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.selected_result else {
            ui.add_space(32.0);
            ui.label(crate::i18n::tr("选择文件查看预览"));
            return;
        };
        let path = self.index.path(id);
        let name = self.index.name(id).to_owned();
        let category = self.index.category(id);
        let modified = self.index.modified(id);
        let preview = if category == Category::Image {
            self.preview.get(&path, modified)
        } else {
            None
        };
        egui::Panel::bottom("preview_actions")
            .frame(
                egui::Frame::new()
                    .fill(Color32::WHITE)
                    .inner_margin(egui::Margin::symmetric(0, 8)),
            )
            .show(ui, |ui| {
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_sized(
                            [110.0, 36.0],
                            egui::Button::new(
                                RichText::new(crate::i18n::tr("打开")).color(Color32::WHITE),
                            )
                            .fill(design::ACCENT),
                        )
                        .clicked()
                    {
                        if let Err(e) = self.open(&path) {
                            self.status = e;
                        }
                    }
                    if ui
                        .add_sized(
                            [140.0, 36.0],
                            egui::Button::new(crate::i18n::tr("在访达中显示")),
                        )
                        .clicked()
                    {
                        Self::reveal(&path);
                    }
                });
            });
        egui::ScrollArea::vertical()
            .id_salt("selected_preview")
            .show(ui, |ui| {
                if let Some(image) = &preview {
                    let available = egui::vec2(
                        ui.available_width(),
                        (ui.available_height() * 0.48).clamp(160.0, 320.0),
                    );
                    ui.vertical_centered(|ui| {
                        ui.add(
                            egui::Image::new(&image.texture).fit_to_exact_size(
                                image.texture.size_vec2()
                                    * (available.x / image.texture.size_vec2().x)
                                        .min(available.y / image.texture.size_vec2().y),
                            ),
                        );
                    });
                } else {
                    egui::Frame::new()
                        .fill(Color32::from_rgb(248, 250, 251))
                        .inner_margin(24)
                        .show(ui, |ui| {
                            ui.set_min_height(120.0);
                            ui.set_min_width((ui.available_width() - 48.0).max(100.0));
                            if category == Category::Image
                                && !self.preview.finished(&path, modified)
                            {
                                ui.spinner();
                                ui.label(crate::i18n::tr("正在载入预览…"));
                            } else if category == Category::Image {
                                ui.label(crate::i18n::tr("此图片暂不能预览"));
                                ui.label(crate::i18n::tr("可直接打开查看原文件。"));
                            } else {
                                ui.label(crate::i18n::tr(category.label()));
                                ui.label(crate::i18n::tr("打开文件，或在访达中查看。"));
                            }
                        });
                }
                ui.add_space(16.0);
                ui.add(
                    egui::Label::new(design::semibold(&name, design::SECTION))
                        .wrap()
                        .selectable(true),
                );
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(short_path(&path))
                            .size(design::CAPTION)
                            .color(design::SECONDARY),
                    )
                    .wrap()
                    .selectable(true),
                )
                .on_hover_text(&path);
                ui.add_space(16.0);
                ui.label(
                    RichText::new(crate::i18n::localized_format!(
                        "修改日期  {}",
                        "Modified  {}",
                        format_modified(modified)
                    ))
                    .size(design::CAPTION)
                    .color(design::SECONDARY),
                );
                if let Some(image) = &preview {
                    ui.label(
                        RichText::new(crate::i18n::localized_format!(
                            "大小  {:.1} MB",
                            "Size  {:.1} MB",
                            image.bytes as f64 / 1048576.0
                        ))
                        .size(design::CAPTION)
                        .color(design::SECONDARY),
                    );
                    ui.label(
                        RichText::new(crate::i18n::localized_format!(
                            "尺寸  {} × {}",
                            "Dimensions  {} × {}",
                            image.dimensions[0],
                            image.dimensions[1]
                        ))
                        .size(design::CAPTION)
                        .color(design::SECONDARY),
                    );
                }
            });
    }

    fn reveal(path: &str) {
        let _ = Command::new("/usr/bin/open").args(["-R", path]).spawn();
    }

    fn open(&mut self, path: &str) -> Result<(), String> {
        let mut child = Command::new("/usr/bin/open")
            .arg(path)
            .spawn()
            .map_err(|error| {
                crate::i18n::localized_format!(
                    "无法调用系统打开功能：{error}",
                    "Could not open file: {error}"
                )
            })?;
        let (sender, receiver) = mpsc::channel();
        self.open_receiver = Some(receiver);
        let context = self.app_context.clone();
        std::thread::spawn(move || {
            let result = child
                .wait()
                .map_err(|error| {
                    crate::i18n::localized_format!(
                        "无法确认打开结果：{error}",
                        "Could not confirm open result: {error}"
                    )
                })
                .and_then(|status| {
                    if status.success() {
                        Ok(())
                    } else {
                        Err(crate::i18n::localized_format!(
                            "系统无法打开这个项目（状态码：{status}）",
                            "System could not open this item (status: {status})"
                        ))
                    }
                });
            let _ = sender.send(result);
            context.request_repaint();
        });
        Ok(())
    }
}

fn result_pointer_click(ui: &egui::Ui, rect: egui::Rect, double: bool) -> bool {
    let Some(pos) = ui.input(|i| i.pointer.interact_pos()) else {
        return false;
    };
    ui.is_enabled()
        && rect.intersect(ui.clip_rect()).contains(pos)
        && ui.ctx().layer_id_at(pos) == Some(ui.layer_id())
        && ui.input(|i| {
            if double {
                i.pointer
                    .button_double_clicked(egui::PointerButton::Primary)
            } else {
                i.pointer.button_clicked(egui::PointerButton::Primary)
            }
        })
}

fn result_row_response(ui: &mut egui::Ui, row: usize, height: f32) -> (egui::Rect, egui::Response) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let response = ui.interact(
        rect,
        egui::Id::new(("search_result_row", row)),
        egui::Sense::click(),
    );
    (rect, response)
}

fn result_row_height(ui: &mut egui::Ui, thumbnails: bool) -> f32 {
    // show_rows includes parent spacing in its virtual stride.
    ui.spacing_mut().item_spacing.y = 0.0;
    if thumbnails { 72.0 } else { 40.0 }
}

fn settings_window(title: &str, width: f32) -> egui::Window<'_> {
    egui::Window::new(title)
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .min_width(width)
        .max_width(width)
        .frame(
            egui::Frame::new()
                .fill(Color32::WHITE)
                .inner_margin(24)
                .corner_radius(12)
                .stroke(egui::Stroke::new(1.0, design::BORDER)),
        )
}
fn primary_button(label: &str) -> egui::Button<'_> {
    egui::Button::new(design::medium(label, design::BODY).color(Color32::WHITE))
        .fill(design::ACCENT)
}
fn category_icon(kind: Category) -> &'static str {
    match kind.label() {
        "全部" => "layout-grid",
        "文件夹" => "folder",
        "文档" => "file-text",
        "图片" => "photo",
        "音频" => "music",
        "视频" => "video",
        "应用" => "apps",
        "压缩包" => "file-zip",
        "代码" => "code",
        _ => "dots",
    }
}

fn short_path(path: &str) -> String {
    let Some(base) = directories::BaseDirs::new() else {
        return path.to_owned();
    };
    let home = base.home_dir().to_string_lossy();
    if let Some(rest) = path
        .strip_prefix(home.as_ref())
        .filter(|v| v.is_empty() || v.starts_with('/'))
    {
        format!("~{rest}")
    } else {
        path.to_owned()
    }
}

fn format_modified(value: Option<i64>) -> String {
    let Some(value) = value else {
        return crate::i18n::tr("暂无日期").into();
    };
    let seconds = value.div_euclid(1000) as libc::time_t;
    // localtime_r uses the computer's local timezone and writes into owned storage.
    let mut result: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&seconds, &mut result) }.is_null() {
        return crate::i18n::tr("暂无日期").into();
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        result.tm_year + 1900,
        result.tm_mon + 1,
        result.tm_mday,
        result.tm_hour,
        result.tm_min,
        result.tm_sec
    )
}

fn is_index_relevant(kind: &notify::EventKind) -> bool {
    matches!(
        kind,
        notify::EventKind::Create(_) | notify::EventKind::Remove(_) | notify::EventKind::Modify(_)
    )
}

fn coalesce_paths(mut paths: Vec<PathBuf>, roots: &[PathBuf]) -> Vec<PathBuf> {
    paths.retain(|path| !roots.iter().any(|root| root == path));
    paths.sort();
    paths.dedup();
    if paths.len() >= 50 {
        if let Some(common) = common_parent(&paths) {
            let is_deep_enough = roots.iter().any(|root| {
                common
                    .strip_prefix(root)
                    .is_ok_and(|relative| relative.components().count() >= 2)
            });
            if is_deep_enough {
                return vec![common];
            }
        }
    }
    paths.sort_by_key(|path| path.components().count());
    let mut result = Vec::<PathBuf>::new();
    for path in paths {
        if !result.iter().any(|parent| path.starts_with(parent)) {
            result.push(path);
        }
    }
    result
}

fn common_parent(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut common = paths.first()?.parent()?.to_path_buf();
    for path in paths.iter().skip(1) {
        while !path.starts_with(&common) {
            if !common.pop() {
                return None;
            }
        }
    }
    Some(common)
}

fn install_visual_style(context: &egui::Context) {
    let mut style = (*context.style_of(egui::Theme::Light)).clone();
    style.visuals = egui::Visuals::light();
    style.spacing.item_spacing = egui::vec2(12.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.spacing.interact_size = egui::vec2(40.0, 32.0);
    style.spacing.icon_spacing = 8.0;
    style.spacing.menu_margin = egui::Margin::same(8);
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::proportional(design::BODY),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::proportional(design::BODY),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        egui::FontId::proportional(design::CAPTION),
    );
    style.visuals.override_text_color = Some(design::TEXT);
    style.visuals.panel_fill = design::SURFACE;
    style.visuals.window_fill = Color32::from_rgb(252, 252, 253);
    style.visuals.window_corner_radius = egui::CornerRadius::same(8);
    style.visuals.menu_corner_radius = egui::CornerRadius::same(8);
    style.visuals.selection.bg_fill = Color32::from_rgb(224, 242, 245);
    style.visuals.selection.stroke = egui::Stroke::new(1.0, design::ACCENT_TEXT);
    for widget in [
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
    ] {
        widget.corner_radius = egui::CornerRadius::same(6);
        widget.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(215, 219, 226));
    }
    style.visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(250, 250, 252);
    style.visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, design::BORDER);
    style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(228, 236, 246);
    context.set_visuals(style.visuals.clone());
    context.set_style_of(egui::Theme::Dark, style.clone());
    context.set_style_of(egui::Theme::Light, style);
    context.set_theme(egui::Theme::Light);
}

fn install_chinese_font(context: &egui::Context) {
    fonts::install(context);
}

impl eframe::App for MiaoFindApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "search_preferences_v1", &self.preferences);
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.index.persist().is_ok() && self.index.dates_ready() {
            #[cfg(target_os = "macos")]
            if let Some(history) = &self.history {
                let _ = history.checkpoint();
            }
        }
    }

    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        if self.hotkey_requested.swap(false, Ordering::AcqRel) {
            let focused = context.input(|input| input.viewport().focused.unwrap_or(false));
            if focused {
                context.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            } else {
                context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                context.send_viewport_cmd(egui::ViewportCommand::Focus);
                self.focus_search = true;
            }
        }
    }

    fn ui(&mut self, root_ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = root_ui.ctx().clone();
        if let Some(result) = self
            .open_receiver
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.open_receiver = None;
            if let Err(error) = result {
                self.status = error;
            }
        }
        self.receive_scan_updates(&context);
        self.receive_file_updates(&context);
        self.receive_dates(&context);
        #[cfg(target_os = "macos")]
        if let Some(complete) = self.history.as_mut().and_then(|h| h.poll(&mut self.index)) {
            self.filter_signature.clear();
            if !complete {
                self.status =
                    crate::i18n::tr("文件变更历史不完整，如需核对全部日期，请重新建立索引").into();
            }
        }
        self.thumbnails.receive(&context);
        self.preview.receive(&context);
        let popup_was_open = egui::Popup::is_any_open(&context);
        let dialog_open = self.date_editor
            || self.scope_editor
            || self.save_editor
            || self.manage_filters
            || self.shortcut_editor;
        if self.time_range.relative() {
            context.request_repaint_after(std::time::Duration::from_secs(60));
        }
        self.filter_windows(&context);
        if self.date_editor {
            let mut open = true;
            egui::Window::new(crate::i18n::tr("自定义修改日期范围"))
                .default_pos(egui::pos2(280.0, 180.0))
                .default_width(420.0)
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(&context, |ui| {
                    ui.label(crate::i18n::tr(
                        "输入起止日期（YYYY-MM-DD），包含结束日期当天。",
                    ));
                    ui.horizontal(|ui| {
                        ui.label(crate::i18n::tr("开始日期"));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.date_start)
                                .vertical_align(egui::Align::Center)
                                .desired_width(140.0),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label(crate::i18n::tr("结束日期"));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.date_end)
                                .vertical_align(egui::Align::Center)
                                .desired_width(140.0),
                        );
                    });
                    ui.label(crate::i18n::tr(
                        "例如 2025-01-01 至 2026-12-31；使用本地时区。",
                    ));
                    if !self.date_feedback.is_empty() {
                        ui.colored_label(Color32::DARK_RED, &self.date_feedback);
                    }
                    ui.horizontal(|ui| {
                        if ui.button(crate::i18n::tr("应用日期范围")).clicked() {
                            let range = TimeRange::Custom {
                                start: self.date_start.trim().into(),
                                end: self.date_end.trim().into(),
                            };
                            match range.bounds() {
                                Ok(_) => {
                                    self.time_range = range;
                                    self.date_editor = false;
                                }
                                Err(error) => self.date_feedback = error,
                            }
                        }
                        if ui.button(crate::i18n::tr("取消")).clicked() {
                            self.date_editor = false;
                        }
                    });
                });
            if !open {
                self.date_editor = false;
            }
        }
        egui::Panel::top("search_bar")
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(247, 248, 250))
                    .inner_margin(egui::Margin::symmetric(16, 12)),
            )
            .show(root_ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.add(self.icons.image("search", 22.0, design::ACCENT_TEXT));
                    ui.label(design::semibold(
                        crate::i18n::localized_format!(
                            "随手找 v{}",
                            "Everywhere v{}",
                            env!("CARGO_PKG_VERSION")
                        ),
                        16.0,
                    ));
                    ui.add_enabled_ui(!self.scanning, |ui| {
                        ui.menu_button(crate::i18n::tr("索引"), |ui| {
                            ui.set_min_width(220.0);
                            if ui.button(crate::i18n::tr("全盘索引")).clicked() {
                                self.start_scan(index::full_disk_roots());
                                ui.close();
                            }
                            if ui.button(crate::i18n::tr("选择位置…")).clicked() {
                                self.choose_roots(true);
                                ui.close();
                            }
                            if !self.roots.is_empty() {
                                if ui.button(crate::i18n::tr("添加位置…")).clicked() {
                                    self.choose_roots(false);
                                    ui.close();
                                }
                                if ui.button(crate::i18n::tr("重新建立索引")).clicked() {
                                    self.start_scan(self.roots.clone());
                                    ui.close();
                                }
                            }
                        });
                    });
                    design::checkbox(ui, &mut self.include_paths, crate::i18n::tr("同时匹配路径"))
                        .on_hover_text(crate::i18n::tr(
                            "开启后，父目录名称也参与匹配，例如用项目名查找项目内文件",
                        ));
                    let active = self.hotkey.as_ref().is_some_and(|c| c.active());
                    ui.label(
                        RichText::new(crate::i18n::localized_format!(
                            "{} {} · 双击打开",
                            "{} {} · Double-click to open",
                            self.preferences.shortcut.label(),
                            if active {
                                crate::i18n::tr("唤起")
                            } else {
                                crate::i18n::tr("未生效")
                            }
                        ))
                        .size(design::CAPTION)
                        .color(design::SECONDARY),
                    );
                    if ui.button(crate::i18n::tr("快捷键设置")).clicked() {
                        self.shortcut_draft = self.preferences.shortcut.clone();
                        self.shortcut_editor = true;
                    }
                });
                ui.add_space(4.0);
                let response = ui
                    .horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            egui::ComboBox::from_id_salt("sort_mode")
                                .width(170.0)
                                .selected_text(crate::i18n::tr(self.sort_order.label()))
                                .show_ui(ui, |ui| {
                                    for order in [
                                        SortOrder::Relevance,
                                        SortOrder::Latest,
                                        SortOrder::Oldest,
                                        SortOrder::Name,
                                    ] {
                                        ui.add_enabled_ui(
                                            !matches!(order, SortOrder::Latest | SortOrder::Oldest)
                                                || self.index.dates_ready(),
                                            |ui| {
                                                ui.selectable_value(
                                                    &mut self.sort_order,
                                                    order,
                                                    crate::i18n::tr(order.label()),
                                                );
                                            },
                                        );
                                    }
                                });
                            ui.add_sized(
                                [ui.available_width(), 40.0],
                                egui::TextEdit::singleline(&mut self.query)
                                    .vertical_align(egui::Align::Center)
                                    .hint_text(if self.include_paths {
                                        crate::i18n::tr("搜索名称和路径，空格组合关键词")
                                    } else {
                                        crate::i18n::tr("搜索文件或文件夹名称，空格组合关键词")
                                    })
                                    .font(egui::FontId::proportional(16.0)),
                            )
                        })
                        .inner
                    })
                    .inner;
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("scope_mode")
                        .width(140.0)
                        .selected_text(if self.personal {
                            crate::i18n::tr("我的搜索范围")
                        } else {
                            crate::i18n::tr("全部位置")
                        })
                        .show_ui(ui, |ui| {
                            ui.set_min_width(220.0);
                            ui.selectable_value(
                                &mut self.personal,
                                false,
                                crate::i18n::tr("全部位置"),
                            );
                            if ui
                                .selectable_label(self.personal, crate::i18n::tr("我的搜索范围…"))
                                .clicked()
                            {
                                self.open_scope_editor();
                                ui.close();
                            }
                        });
                    ui.add_enabled_ui(self.index.dates_ready(), |ui| {
                        egui::ComboBox::from_id_salt("date_range")
                            .width(140.0)
                            .selected_text(self.time_range.label())
                            .show_ui(ui, |ui| {
                                ui.set_min_width(220.0);
                                ui.selectable_value(
                                    &mut self.time_range,
                                    TimeRange::Any,
                                    crate::i18n::tr("不限时间"),
                                );
                                ui.separator();
                                for range in [
                                    TimeRange::Days3,
                                    TimeRange::Week,
                                    TimeRange::Month,
                                    TimeRange::Year,
                                ] {
                                    ui.selectable_value(
                                        &mut self.time_range,
                                        range.clone(),
                                        range.label(),
                                    );
                                }
                                ui.separator();
                                if ui
                                    .selectable_label(
                                        matches!(self.time_range, TimeRange::Custom { .. }),
                                        crate::i18n::tr("自定义日期…"),
                                    )
                                    .clicked()
                                {
                                    if let TimeRange::Custom { start, end } = &self.time_range {
                                        self.date_start = start.clone();
                                        self.date_end = end.clone();
                                    }
                                    self.date_feedback.clear();
                                    self.date_editor = true;
                                    ui.close();
                                }
                            })
                            .response
                            .on_hover_text(crate::i18n::tr(
                                "按修改时间筛选；一个月为30天，一年为365天；不是最近打开时间",
                            ));
                    });
                    ui.menu_button(crate::i18n::tr("常用筛选"), |ui| {
                        ui.set_min_width(240.0);
                        ui.label(
                            design::medium(crate::i18n::tr("快捷搜索"), design::CAPTION)
                                .color(design::SECONDARY),
                        );
                        for preset in SavedFilter::builtins() {
                            if ui
                                .add(egui::Button::new(crate::i18n::tr(&preset.name)).frame(false))
                                .clicked()
                            {
                                self.apply_filter(preset);
                                ui.close();
                            }
                        }
                        ui.separator();
                        ui.label(
                            design::medium(crate::i18n::tr("我的常用筛选"), design::CAPTION)
                                .color(design::SECONDARY),
                        );
                        if self.preferences.saved.is_empty() {
                            ui.label(
                                RichText::new(crate::i18n::tr("还没有自定义筛选"))
                                    .size(design::CAPTION)
                                    .color(design::SECONDARY),
                            );
                        }
                        for f in self.preferences.saved.clone() {
                            let selected = self.query == f.query
                                && self.category == f.category
                                && self.personal == f.personal
                                && self.time_range == f.effective_time_range()
                                && self.sort_order == f.order
                                && self.include_paths == f.paths
                                && self.raster == f.raster
                                && self.thumbnails_enabled == f.thumbnails;
                            if ui
                                .add(egui::Button::new(&f.name).selected(selected))
                                .clicked()
                            {
                                self.apply_filter(f);
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button(crate::i18n::tr("新增常用筛选…")).clicked() {
                            self.open_filter_editor(false);
                            ui.close();
                        }
                        if ui.button(crate::i18n::tr("保存当前筛选…")).clicked() {
                            self.open_filter_editor(true);
                            ui.close();
                        }
                        if ui.button(crate::i18n::tr("管理筛选…")).clicked() {
                            self.manage_filters = true;
                            ui.close();
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.menu_button(crate::i18n::tr("设置"), |ui| {
                            ui.set_min_width(240.0);
                            ui.label("语言 / Language");
                            let previous = self.preferences.english;
                            ui.horizontal(|ui| {
                                ui.selectable_value(&mut self.preferences.english, false, "中文");
                                ui.selectable_value(&mut self.preferences.english, true, "English");
                            });
                            if previous != self.preferences.english {
                                i18n::set_english(self.preferences.english);
                                self.status = if self.preferences.english {
                                    "Language set to English".into()
                                } else {
                                    "界面语言已设为中文".into()
                                };
                                self.shortcut_feedback.clear();
                                self.date_feedback.clear();
                                context.send_viewport_cmd(egui::ViewportCommand::Title(
                                    crate::i18n::localized_format!(
                                        "随手找 v{}",
                                        "Everywhere v{}",
                                        env!("CARGO_PKG_VERSION")
                                    ),
                                ));
                                if let Some(storage) = _frame.storage_mut() {
                                    eframe::set_value(
                                        storage,
                                        "search_preferences_v1",
                                        &self.preferences,
                                    );
                                    storage.flush();
                                }
                                context.request_repaint();
                            }
                            ui.separator();
                            design::checkbox(
                                ui,
                                &mut self.preferences.preview_visible,
                                crate::i18n::tr("显示右侧预览"),
                            );
                            ui.separator();
                            design::checkbox(ui, &mut self.raster, crate::i18n::tr("仅可预览图片"))
                                .on_hover_text(crate::i18n::tr("仅PNG / JPEG / WebP / GIF"));
                            ui.separator();
                            if ui.button(crate::i18n::tr("我的搜索范围…")).clicked() {
                                self.open_scope_editor();
                                ui.close();
                            }
                            if ui.button(crate::i18n::tr("快捷键设置…")).clicked() {
                                self.shortcut_draft = self.preferences.shortcut.clone();
                                self.shortcut_editor = true;
                                ui.close();
                            }
                        });
                        ui.separator();
                        ui.selectable_value(
                            &mut self.thumbnails_enabled,
                            true,
                            crate::i18n::tr("缩略图"),
                        );
                        ui.selectable_value(
                            &mut self.thumbnails_enabled,
                            false,
                            crate::i18n::tr("列表"),
                        );
                    });
                });
                if self.raster {
                    ui.label(
                        RichText::new(crate::i18n::tr(
                            "格式筛选：仅可预览图片（可在‘设置’中取消）",
                        ))
                        .small(),
                    );
                }
                if self.personal {
                    ui.label(
                        RichText::new(crate::i18n::localized_format!(
                            "我的搜索范围 · {} 个目录（可在‘范围’中查看）",
                            "My Search Locations · {} folders (see location settings)",
                            self.preferences.personal_roots.len()
                        ))
                        .small(),
                    );
                }
                if let TimeRange::Custom { start, end } = &self.time_range {
                    ui.label(
                        RichText::new(crate::i18n::localized_format!(
                            "修改日期：{start} 至 {end}（包含结束当天）",
                            "Modified: {start} to {end} (inclusive)"
                        ))
                        .small(),
                    );
                }
                if !popup_was_open
                    && !dialog_open
                    && context
                        .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
                {
                    if self.query.is_empty() {
                        context.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    } else {
                        self.query.clear();
                        response.request_focus();
                    }
                }
                if self.focus_search {
                    response.request_focus();
                    self.focus_search = false;
                }
            });
        self.update_search();
        self.paging.poll(&mut self.selected_result, &context);
        egui::Panel::bottom("status_bar").show(root_ui, |ui| {
            ui.style_mut().override_text_style = Some(egui::TextStyle::Small);
            ui.horizontal_wrapped(|ui| {
                if self.scanning || self.date_receiver.is_some() {
                    ui.spinner();
                }
                let status = if self.date_receiver.is_some() {
                    crate::i18n::tr("正在核对修改日期")
                } else {
                    &self.status
                };
                ui.add_sized(
                    [230.0_f32.min(ui.available_width() * 0.3), 18.0],
                    egui::Label::new(status).truncate(),
                );
                ui.separator();
                ui.label(crate::i18n::localized_format!(
                    "{} 个结果 · {:.2} ms",
                    "{} results · {:.2} ms",
                    self.paging.total,
                    self.query_ms
                ));
                if self.paging.total > 0 && !self.paging.visible.is_empty() {
                    ui.separator();
                    ui.label(crate::i18n::localized_format!(
                        "当前 {}–{} · 滚动加载",
                        "Showing {}–{} · Scroll to load",
                        self.paging.visible.start + 1,
                        self.paging.visible.end
                    ));
                    if self.paging.visible_loading() {
                        ui.spinner();
                    }
                }
            });
        });
        egui::Panel::left("categories")
            .frame(
                egui::Frame::new()
                    .fill(design::SURFACE)
                    .inner_margin(egui::Margin::symmetric(12, 16)),
            )
            .default_size(if i18n::english() { 200.0 } else { 160.0 })
            .min_size(if i18n::english() { 190.0 } else { 144.0 })
            .max_size(220.0)
            .show(root_ui, |ui| {
                ui.label(
                    design::medium(crate::i18n::tr("分类"), design::CAPTION)
                        .color(design::SECONDARY),
                );
                ui.add_space(8.0);
                for kind in Category::ALL {
                    ui.add_sized(
                        [ui.available_width(), 34.0],
                        egui::Button::new("")
                            .left_text((
                                design::medium(crate::i18n::tr(kind.label()), design::BODY).color(
                                    if self.category == kind {
                                        design::ACCENT_TEXT
                                    } else {
                                        design::TEXT
                                    },
                                ),
                                self.icons.image(
                                    category_icon(kind),
                                    20.0,
                                    if self.category == kind {
                                        design::ACCENT_TEXT
                                    } else {
                                        Color32::from_rgb(66, 79, 97)
                                    },
                                ),
                            ))
                            .fill(if self.category == kind {
                                Color32::from_rgb(224, 242, 245)
                            } else {
                                Color32::TRANSPARENT
                            })
                            .stroke(egui::Stroke::NONE)
                            .selected(self.category == kind),
                    )
                    .clicked()
                    .then(|| {
                        self.category = kind;
                        if kind != Category::Image {
                            self.raster = false;
                        }
                    });
                }
            });
        self.update_search();
        let mut keyboard_selection = false;
        if !popup_was_open && !dialog_open && !egui::Popup::is_any_open(&context) {
            if context.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)) {
                self.move_selection(1);
                keyboard_selection = true;
            }
            if context.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
                self.move_selection(-1);
                keyboard_selection = true;
            }
            if context.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
                if let Some(id) = self.selected_result {
                    if let Err(e) = self.open(&self.index.path(id)) {
                        self.status = e;
                    }
                }
            }
        }
        let show_preview = self.preferences.preview_visible && root_ui.available_width() >= 730.0;
        if show_preview {
            let width = (root_ui.available_width() * 0.36).clamp(300.0, 430.0);
            egui::Panel::right("file_preview")
                .exact_size(width)
                .frame(egui::Frame::new().fill(Color32::WHITE).inner_margin(16))
                .show(root_ui, |ui| {
                    self.preview_ui(ui);
                });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::WHITE).inner_margin(8))
            .show(root_ui, |ui| {
                let width = ui.available_width();
                let date_width = if width >= 520.0 { 170.0 } else { 128.0 };
                let name_width = (width - date_width - 12.0).max(150.0);
                let (header, _) =
                    ui.allocate_exact_size(egui::vec2(width, 32.0), egui::Sense::hover());
                let name_start = if self.thumbnails_enabled { 80.0 } else { 12.0 };
                let name_rect = egui::Rect::from_min_size(
                    header.min + egui::vec2(name_start, 0.0),
                    egui::vec2((name_width - name_start).max(60.0), 32.0),
                );
                if ui
                    .put(
                        name_rect,
                        egui::Button::new("")
                            .left_text(design::medium(
                                if self.sort_order == SortOrder::Name {
                                    crate::i18n::tr("名称 ↑")
                                } else {
                                    crate::i18n::tr("名称")
                                },
                                design::BODY,
                            ))
                            .frame(false),
                    )
                    .clicked()
                {
                    self.sort_order = SortOrder::Name;
                }
                let label = match self.sort_order {
                    SortOrder::Latest => crate::i18n::tr("修改日期 ↓"),
                    SortOrder::Oldest => crate::i18n::tr("修改日期 ↑"),
                    _ => crate::i18n::tr("修改日期"),
                };
                let date_rect = egui::Rect::from_min_size(
                    header.min + egui::vec2(name_width + 8.0, 0.0),
                    egui::vec2(date_width - 8.0, 32.0),
                );
                if ui
                    .add_enabled_ui(self.index.dates_ready(), |ui| {
                        ui.put(
                            date_rect,
                            egui::Button::new("")
                                .left_text(design::medium(label, design::BODY))
                                .frame(false),
                        )
                    })
                    .inner
                    .clicked()
                {
                    self.sort_order = if self.sort_order == SortOrder::Latest {
                        SortOrder::Oldest
                    } else {
                        SortOrder::Latest
                    };
                }
                ui.separator();
                if self.index.is_empty() && !self.scanning {
                    ui.label(crate::i18n::tr("请选择位置建立文件名索引"));
                    if ui.button(crate::i18n::tr("选择位置…")).clicked() {
                        self.choose_roots(true);
                    }
                } else if self.paging.total == 0 {
                    ui.add_space(24.0);
                    ui.label(if self.query.is_empty() {
                        crate::i18n::tr("输入名称，开始找文件")
                    } else {
                        crate::i18n::tr("没有找到符合条件的文件")
                    });
                    ui.label(
                        RichText::new(crate::i18n::tr("试试减少关键词，或放宽位置与日期范围。"))
                            .color(design::SECONDARY),
                    );
                }
                let height = result_row_height(ui, self.thumbnails_enabled);
                let mut scroll = egui::ScrollArea::vertical()
                    .id_salt("search_results")
                    .auto_shrink([false, false]);
                if std::mem::take(&mut self.paging.reset_scroll) {
                    scroll = scroll.vertical_scroll_offset(0.0);
                }
                if keyboard_selection && self.paging.total > 0 {
                    scroll = scroll.vertical_scroll_offset(
                        (self.paging.selection_row as f32 * height - ui.available_height() * 0.5)
                            .max(0.0),
                    );
                }
                scroll.show_rows(ui, height, self.paging.total, |ui, range| {
                    self.paging.show_range(range.clone(), &context);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for row in range {
                        let (rect, response) = result_row_response(ui, row, height);
                        let Some(id) = self.paging.id(row) else {
                            ui.put(
                                rect,
                                egui::Label::new(
                                    RichText::new(crate::i18n::tr("正在加载…"))
                                        .size(design::CAPTION)
                                        .color(design::SECONDARY),
                                )
                                .sense(egui::Sense::hover()),
                            );
                            if response.clicked() || result_pointer_click(ui, rect, false) {
                                self.paging.select(row, &mut self.selected_result);
                                context.request_repaint();
                            }
                            continue;
                        };
                        let path = self.index.path(id);
                        let parent = std::path::Path::new(&path)
                            .parent()
                            .map(|p| short_path(&p.to_string_lossy()))
                            .unwrap_or_default();
                        let name = self.index.name(id).to_owned();
                        let date = format_modified(self.index.modified(id));
                        let selected = self.selected_result == Some(id);
                        let bg = if selected {
                            Color32::from_rgb(224, 242, 245)
                        } else if response.hovered() {
                            Color32::from_rgb(244, 248, 250)
                        } else {
                            Color32::WHITE
                        };
                        ui.painter().rect_filled(rect, 4.0, bg);
                        if selected {
                            ui.painter().rect_filled(
                                egui::Rect::from_min_size(rect.min, egui::vec2(3.0, height)),
                                2.0,
                                design::ACCENT,
                            );
                        }
                        let thumbnail_width = if self.thumbnails_enabled { 68.0 } else { 0.0 };
                        if self.thumbnails_enabled && self.index.category(id) == Category::Image {
                            if let Some(texture) =
                                self.thumbnails
                                    .get(&context, &path, self.index.modified(id))
                            {
                                let size = texture.size_vec2();
                                let fit = size * (56.0 / size.x.max(size.y));
                                ui.painter().image(
                                    texture.id(),
                                    egui::Rect::from_center_size(
                                        rect.min + egui::vec2(34.0, height / 2.0),
                                        fit,
                                    ),
                                    egui::Rect::from_min_max(
                                        egui::Pos2::ZERO,
                                        egui::pos2(1.0, 1.0),
                                    ),
                                    Color32::WHITE,
                                );
                            }
                        }
                        if self.thumbnails_enabled && self.index.category(id) != Category::Image {
                            let image = self.icons.image(
                                category_icon(self.index.category(id)),
                                24.0,
                                design::SECONDARY,
                            );
                            ui.put(
                                egui::Rect::from_center_size(
                                    rect.min + egui::vec2(34.0, height / 2.0),
                                    egui::vec2(24.0, 24.0),
                                ),
                                image,
                            );
                        }
                        let text_width = (name_width - thumbnail_width - 16.0).max(60.0);
                        let area = egui::Rect::from_min_size(
                            rect.min + egui::vec2(thumbnail_width + 12.0, 0.0),
                            egui::vec2(text_width, height),
                        );
                        let label = ui
                            .scope_builder(
                                egui::UiBuilder::new()
                                    .max_rect(area)
                                    .layout(egui::Layout::top_down(egui::Align::Min)),
                                |ui| {
                                    ui.set_clip_rect(ui.clip_rect().intersect(area));
                                    ui.add_space(if self.thumbnails_enabled { 13.0 } else { 10.0 });
                                    let r = ui.add(
                                        egui::Label::new(
                                            RichText::new(&name)
                                                .size(design::BODY)
                                                .color(design::TEXT),
                                        )
                                        .truncate()
                                        .sense(egui::Sense::hover()),
                                    );
                                    if self.thumbnails_enabled {
                                        ui.add_space(4.0);
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&parent)
                                                    .size(design::CAPTION)
                                                    .color(design::SECONDARY),
                                            )
                                            .truncate()
                                            .sense(egui::Sense::hover()),
                                        );
                                    }
                                    r
                                },
                            )
                            .inner;
                        let date_area = egui::Rect::from_min_size(
                            rect.min + egui::vec2(name_width + 8.0, 0.0),
                            egui::vec2(date_width - 8.0, height),
                        );
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .max_rect(date_area)
                                .layout(egui::Layout::left_to_right(egui::Align::Center)),
                            |ui| {
                                ui.set_clip_rect(ui.clip_rect().intersect(date_area));
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(date.get(..16).unwrap_or(&date))
                                            .size(design::CAPTION)
                                            .color(design::SECONDARY),
                                    )
                                    .truncate()
                                    .sense(egui::Sense::hover()),
                                );
                            },
                        );
                        let response = response.union(label).on_hover_text(&path);
                        if response.clicked() || result_pointer_click(ui, rect, false) {
                            self.paging.select(row, &mut self.selected_result);
                            context.request_repaint();
                        }
                        if response.double_clicked() || result_pointer_click(ui, rect, true) {
                            if let Err(e) = self.open(&path) {
                                self.status = e;
                            }
                        }
                        response.context_menu(|ui| {
                            if ui.button(crate::i18n::tr("打开")).clicked() {
                                if let Err(e) = self.open(&path) {
                                    self.status = e;
                                }
                                ui.close();
                            }
                            if ui.button(crate::i18n::tr("在访达中显示")).clicked() {
                                Self::reveal(&path);
                                ui.close();
                            }
                            if ui.button(crate::i18n::tr("复制完整路径")).clicked() {
                                ui.ctx().copy_text(path.clone());
                                ui.close();
                            }
                        });
                    }
                });
            });
        if self.scanning {
            context.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn content_and_metadata_modifications_refresh_dates() {
        use notify::{
            EventKind,
            event::{AccessKind, DataChange, MetadataKind, ModifyKind},
        };
        assert!(super::is_index_relevant(&EventKind::Modify(
            ModifyKind::Data(DataChange::Content)
        )));
        assert!(super::is_index_relevant(&EventKind::Modify(
            ModifyKind::Metadata(MetadataKind::WriteTime)
        )));
        assert!(!super::is_index_relevant(&EventKind::Access(
            AccessKind::Read
        )));
    }
}

#[cfg(test)]
mod virtual_list_tests {
    use super::*;
    #[test]
    fn millions_of_rows_paint_only_viewport_and_keyboard_target_is_visible_in_both_modes() {
        for thumbnails in [false, true] {
            let context = egui::Context::default();
            context.begin_pass(Default::default());
            let mut ui = egui::Ui::new(
                context.clone(),
                egui::Id::new("virtual_rows_test"),
                egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(640.0, 400.0),
                )),
            );
            let height = result_row_height(&mut ui, thumbnails);
            let mut painted = 0;
            let mut visible = 0..0;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .vertical_scroll_offset(500.0 * height - 200.0)
                .show_rows(&mut ui, height, 1_750_000, |ui, range| {
                    visible = range.clone();
                    for _ in range {
                        painted += 1;
                        ui.allocate_space(egui::vec2(600.0, height));
                    }
                });
            let mut output = context.end_pass();
            output.textures_delta.clear();
            assert!(
                visible.contains(&500),
                "keyboard target must remain visible: {visible:?}"
            );
            assert!(
                painted < 20,
                "virtual rendering must remain bounded: {painted}"
            );
        }
    }
}
