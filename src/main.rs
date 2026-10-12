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

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, RichText};
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};
use miao_find::index::{self, Category, CompactIndex, IndexDelta, ScanMessage, SortOrder};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 620.0])
            .with_min_inner_size([680.0, 420.0])
            .with_title(format!("随手找 v{}", env!("CARGO_PKG_VERSION"))),
        ..Default::default()
    };

    eframe::run_native(
        "随手找",
        options,
        Box::new(|context| Ok(Box::new(MiaoFindApp::new(context)))),
    )
}

struct MiaoFindApp {
    index: CompactIndex,
    results: Vec<usize>,
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
    date_receiver: Option<Receiver<Vec<(u64, i64, i64)>>>,
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
    _hotkey_manager: Option<GlobalHotKeyManager>,
    hotkey_requested: Arc<AtomicBool>,
    focus_search: bool,
    app_context: egui::Context,
}

impl MiaoFindApp {
    fn new(context: &eframe::CreationContext<'_>) -> Self {
        context.egui_ctx.set_visuals(egui::Visuals::light());
        install_chinese_font(&context.egui_ctx);
        index::warm_up_parallel_search();
        let hotkey_requested = Arc::new(AtomicBool::new(false));
        let (hotkey_manager, hotkey_status) =
            install_global_hotkey(&context.egui_ctx, Arc::clone(&hotkey_requested));
        let mut app = match index::load_cached() {
            Ok(index) => {
                let count = index.len();
                let roots = index.roots().to_vec();
                Self {
                    index,
                    results: Vec::new(),
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
                    date_receiver: None,
                    status: if count == 0 {
                        "请选择一个文件夹建立索引".to_owned()
                    } else {
                        format!("已载入 {count} 个文件和文件夹")
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
                    _hotkey_manager: hotkey_manager,
                    hotkey_requested,
                    focus_search: false,
                    app_context: context.egui_ctx.clone(),
                }
            }
            Err(error) => Self {
                index: CompactIndex::default(),
                results: Vec::new(),
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
                date_receiver: None,
                status: format!("索引初始化失败：{error}"),
                scanning: false,
                scan_receiver: None,
                watcher: None,
                watch_receiver: None,
                update_receiver: None,
                pending_paths: Vec::new(),
                last_watch_event: None,
                watcher_started: None,
                query_ms: 0.0,
                _hotkey_manager: hotkey_manager,
                hotkey_requested,
                focus_search: false,
                app_context: context.egui_ctx.clone(),
            },
        };
        if let Some(error) = hotkey_status {
            app.status = error;
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

    fn start_dates(&mut self) {
        if self.index.is_empty() {
            return;
        }
        let snapshot = self.index.clone();
        let (sender, receiver) = mpsc::channel();
        self.date_receiver = Some(receiver);
        let context = self.app_context.clone();
        std::thread::spawn(move || {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build();
            let dates = match pool {
                Ok(pool) => pool.install(|| snapshot.collect_dates()),
                Err(_) => snapshot.collect_dates(),
            };
            let _ = sender.send(dates);
            context.request_repaint();
        });
    }
    fn receive_dates(&mut self, context: &egui::Context) {
        if let Some(receiver) = self.date_receiver.take() {
            match receiver.try_recv() {
                Ok(dates) => {
                    self.index.apply_dates(dates);
                    if let Err(error) = self.index.save_dates() {
                        self.status = format!("日期缓存保存失败：{error}");
                    }
                    self.last_query.clear();
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.date_receiver = Some(receiver);
                    context.request_repaint_after(std::time::Duration::from_millis(100));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.status = "修改日期补充失败，请重新打开重试".into()
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
            self.status = "没有可索引的位置".to_owned();
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.scanning = true;
        self.status = if roots == index::full_disk_roots() {
            "正在建立全盘索引…".to_owned()
        } else {
            format!("正在并行扫描 {} 个位置…", roots.len())
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
                    self.status = format!("正在建立索引：已发现 {count} 项…");
                    context.request_repaint();
                }
                ScanMessage::Finished { index, elapsed_ms } => {
                    self.roots = index.roots().to_vec();
                    self.index = index;
                    self.date_receiver = None;
                    self.pending_paths.clear();
                    self.update_receiver = None;
                    self.scanning = false;
                    self.last_query.clear();
                    self.status = format!(
                        "索引完成：{} 项，用时 {:.1} 秒",
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
            self.status = "索引可搜索，但启动文件变化监听失败".to_owned();
            return;
        };
        for root in &self.roots {
            if let Err(error) = watcher.watch(root, RecursiveMode::Recursive) {
                self.status = format!("索引可搜索，但无法监听 {}：{error}", root.display());
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
                Err(error) => self.status = format!("文件变化监听异常：{error}"),
            }
        }
        self.watch_receiver = Some(receiver);

        if let Some(update_receiver) = self.update_receiver.take() {
            match update_receiver.try_recv() {
                Ok(Ok(delta)) => self.apply_delta(delta),
                Ok(Err(error)) => self.status = format!("增量索引更新失败：{error}"),
                Err(mpsc::TryRecvError::Empty) => {
                    self.update_receiver = Some(update_receiver);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.status = "后台索引更新意外停止".to_owned();
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
            self.status = "正在后台同步文件变化…".to_owned();
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
                self.status = format!("索引已自动更新，当前共 {} 项", self.index.len());
            }
            Err(error) => self.status = format!("增量索引更新失败：{error}"),
        }
    }

    fn update_search(&mut self) {
        if self.query == self.last_query
            && self.category == self.last_category
            && self.sort_order == self.last_sort_order
            && self.include_paths == self.last_include_paths
        {
            return;
        }
        let started = Instant::now();
        self.results = index::search_sorted_with_scope(
            &self.index,
            &self.query,
            500,
            self.category,
            self.sort_order,
            self.include_paths,
        );
        if !self
            .selected_result
            .is_some_and(|id| self.results.contains(&id))
        {
            self.selected_result = self.results.first().copied();
        }
        self.query_ms = started.elapsed().as_secs_f64() * 1_000.0;
        self.last_query.clone_from(&self.query);
        self.last_category = self.category;
        self.last_sort_order = self.sort_order;
        self.last_include_paths = self.include_paths;
    }

    fn move_selection(&mut self, direction: isize) {
        if self.results.is_empty() {
            self.selected_result = None;
            return;
        }
        let current = self
            .selected_result
            .and_then(|selected| self.results.iter().position(|&item| item == selected))
            .unwrap_or(0);
        let next = (current as isize + direction)
            .clamp(0, self.results.len().saturating_sub(1) as isize) as usize;
        self.selected_result = Some(self.results[next]);
    }

    fn reveal(path: &str) {
        let _ = Command::new("/usr/bin/open").args(["-R", path]).spawn();
    }

    fn open(path: &str) -> Result<(), String> {
        let status = Command::new("/usr/bin/open")
            .arg(path)
            .status()
            .map_err(|error| format!("无法调用系统打开功能：{error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("系统无法打开这个项目（状态码：{status}）"))
        }
    }
}

fn format_modified(value: Option<i64>) -> String {
    let Some(value) = value else {
        return "暂无日期".into();
    };
    let seconds = value.div_euclid(1000) as libc::time_t;
    // localtime_r uses the computer's local timezone and writes into owned storage.
    let mut result: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&seconds, &mut result) }.is_null() {
        return "暂无日期".into();
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

fn install_global_hotkey(
    context: &egui::Context,
    requested: Arc<AtomicBool>,
) -> (Option<GlobalHotKeyManager>, Option<String>) {
    let hotkey = HotKey::new(Some(Modifiers::ALT), Code::Space);
    let hotkey_id = hotkey.id();
    let manager = match GlobalHotKeyManager::new() {
        Ok(manager) => manager,
        Err(error) => return (None, Some(format!("全局快捷键初始化失败：{error}"))),
    };
    if let Err(error) = manager.register(hotkey) {
        return (None, Some(format!("⌥ Space 已被其他应用占用：{error}")));
    }
    let repaint = context.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.id == hotkey_id && event.state == HotKeyState::Pressed {
            requested.store(true, Ordering::Release);
            repaint.request_repaint();
        }
    }));
    (Some(manager), None)
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

fn install_chinese_font(context: &egui::Context) {
    let chinese_candidates = [
        "/System/Library/Fonts/STHeiti Medium.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
    ];
    let Some(chinese_bytes) = chinese_candidates
        .iter()
        .find_map(|path| std::fs::read(path).ok())
    else {
        return;
    };
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "macos-chinese".to_owned(),
        std::sync::Arc::new(FontData::from_owned(chinese_bytes)),
    );
    if let Ok(system_bytes) = std::fs::read("/System/Library/Fonts/SFNS.ttf") {
        fonts.font_data.insert(
            "macos-system".to_owned(),
            std::sync::Arc::new(FontData::from_owned(system_bytes)),
        );
    }
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        let family_fonts = fonts.families.entry(family).or_default();
        if fonts.font_data.contains_key("macos-system") {
            family_fonts.insert(0, "macos-system".to_owned());
        }
        family_fonts.push("macos-chinese".to_owned());
    }
    context.set_fonts(fonts);
}

impl eframe::App for MiaoFindApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.index.persist();
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
        context.set_visuals(egui::Visuals::light());
        self.receive_scan_updates(&context);
        self.receive_file_updates(&context);
        self.receive_dates(&context);
        egui::Panel::top("search_bar").show(root_ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("随手找 v{}", env!("CARGO_PKG_VERSION"))).strong());
                ui.add_enabled_ui(!self.scanning, |ui| {
                    ui.menu_button("索引", |ui| {
                        if ui.button("全盘索引").clicked() {
                            self.start_scan(index::full_disk_roots());
                            ui.close();
                        }
                        if ui.button("选择位置…").clicked() {
                            self.choose_roots(true);
                            ui.close();
                        }
                        if !self.roots.is_empty() {
                            if ui.button("添加位置…").clicked() {
                                self.choose_roots(false);
                                ui.close();
                            }
                            if ui.button("重新建立索引").clicked() {
                                self.start_scan(self.roots.clone());
                                ui.close();
                            }
                        }
                    });
                });
                ui.checkbox(&mut self.include_paths, "同时匹配路径")
                    .on_hover_text("开启后，父目录名称也参与匹配，例如用项目名查找项目内文件");
                egui::ComboBox::from_id_salt("sort_mode")
                    .selected_text(self.sort_order.label())
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
                                    ui.selectable_value(&mut self.sort_order, order, order.label());
                                },
                            );
                        }
                    });
                ui.label("⌥ Space 唤起 · 双击打开");
            });
            let response = ui.add_sized(
                [ui.available_width(), 30.0],
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text(if self.include_paths {
                        "搜索名称和路径，空格组合关键词"
                    } else {
                        "搜索文件或文件夹名称，空格组合关键词"
                    })
                    .font(egui::FontId::proportional(16.0)),
            );
            if context.input(|i| i.key_pressed(egui::Key::Escape)) {
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
        egui::Panel::bottom("status_bar").show(root_ui, |ui| {
            ui.horizontal(|ui| {
                if self.scanning || self.date_receiver.is_some() {
                    ui.spinner();
                }
                if self.date_receiver.is_some() {
                    ui.label("正在核对修改日期");
                } else {
                    ui.label(&self.status);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!(
                        "{} 个结果 · {:.2} ms",
                        self.results.len(),
                        self.query_ms
                    ));
                    if self.results.len() == 500 {
                        ui.label("显示前 500 项");
                    }
                });
            });
        });
        egui::Panel::left("categories")
            .default_size(135.0)
            .min_size(110.0)
            .max_size(220.0)
            .show(root_ui, |ui| {
                ui.label(RichText::new("分类").strong());
                ui.separator();
                for kind in Category::ALL {
                    ui.add_sized(
                        [ui.available_width(), 27.0],
                        egui::Button::new("")
                            .left_text(kind.label())
                            .frame_when_inactive(false)
                            .selected(self.category == kind),
                    )
                    .clicked()
                    .then(|| self.category = kind);
                }
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::WHITE).inner_margin(4))
            .show(root_ui, |ui| {
                let width = ui.available_width();
                let date_width = 175.0;
                let name_width = (width * 0.30).clamp(155.0, 350.0);
                let path_width = (width - name_width - date_width - 16.0).max(80.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_sized(
                            [name_width, 27.0],
                            egui::Button::new(if self.sort_order == SortOrder::Name {
                                "名称 ↑"
                            } else {
                                "名称"
                            }),
                        )
                        .clicked()
                    {
                        self.sort_order = SortOrder::Name;
                    }
                    ui.add_sized([path_width, 27.0], egui::Label::new("所在路径"));
                    let label = match self.sort_order {
                        SortOrder::Latest => "修改日期 ↓",
                        SortOrder::Oldest => "修改日期 ↑",
                        SortOrder::Name | SortOrder::Relevance => "修改日期",
                    };
                    if ui
                        .add_enabled(
                            self.index.dates_ready(),
                            egui::Button::new(label).min_size(egui::vec2(date_width, 27.0)),
                        )
                        .clicked()
                    {
                        self.sort_order = if self.sort_order == SortOrder::Latest {
                            SortOrder::Oldest
                        } else {
                            SortOrder::Latest
                        };
                    }
                });
                self.update_search();
                if context.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                    self.move_selection(1);
                }
                if context.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                    self.move_selection(-1);
                }
                if context.input(|i| i.key_pressed(egui::Key::Enter)) {
                    if let Some(id) = self.selected_result {
                        if let Err(e) = Self::open(&self.index.path(id)) {
                            self.status = e;
                        }
                    }
                }
                ui.separator();
                if self.index.is_empty() && !self.scanning {
                    ui.label("请选择位置建立文件名索引");
                    if ui.button("选择位置…").clicked() {
                        self.choose_roots(true);
                    }
                }
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show_rows(ui, 27.0, self.results.len(), |ui, range| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for row in range {
                            let id = self.results[row];
                            let path = self.index.path(id);
                            let parent = std::path::Path::new(&path)
                                .parent()
                                .map(|p| p.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            let name = format!(
                                "{}  {}",
                                self.index.category(id).icon(),
                                self.index.name(id)
                            );
                            let date = format_modified(self.index.modified(id));
                            let selected = self.selected_result == Some(id);
                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), 27.0),
                                egui::Sense::click(),
                            );
                            let bg = if selected {
                                Color32::from_rgb(25, 85, 170)
                            } else if response.hovered() {
                                Color32::from_rgb(232, 240, 250)
                            } else {
                                Color32::WHITE
                            };
                            ui.painter().rect_filled(rect, 0.0, bg);
                            let color = if selected {
                                Color32::WHITE
                            } else {
                                Color32::from_rgb(20, 20, 20)
                            };
                            let mut cell = |offset: f32, cell_width: f32, text: &str| {
                                let clip = egui::Rect::from_min_size(
                                    rect.min + egui::vec2(offset + 4.0, 0.0),
                                    egui::vec2(cell_width - 8.0, 27.0),
                                );
                                ui.scope_builder(
                                    egui::UiBuilder::new()
                                        .max_rect(clip)
                                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                                    |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(text).size(14.0).color(color),
                                            )
                                            .truncate()
                                            .sense(egui::Sense::click()),
                                        )
                                    },
                                )
                                .inner
                            };
                            let response = response
                                .union(cell(0.0, name_width, &name))
                                .union(cell(name_width + 8.0, path_width, &parent))
                                .union(cell(name_width + path_width + 16.0, date_width, &date));
                            let response = response.on_hover_text(&path);
                            if response.clicked() {
                                self.selected_result = Some(id);
                            }
                            if response.double_clicked() {
                                if let Err(e) = Self::open(&path) {
                                    self.status = e;
                                }
                            }
                            response.context_menu(|ui| {
                                if ui.button("打开").clicked() {
                                    if let Err(e) = Self::open(&path) {
                                        self.status = e;
                                    }
                                    ui.close();
                                }
                                if ui.button("在 Finder 中显示").clicked() {
                                    Self::reveal(&path);
                                    ui.close();
                                }
                                if ui.button("复制完整路径").clicked() {
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
