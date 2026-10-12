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
use miao_find::index::{self, Category, CompactIndex, IndexDelta, ScanMessage};
use notify::event::ModifyKind;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 620.0])
            .with_min_inner_size([680.0, 420.0])
            .with_title("随手找"),
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
        context.egui_ctx.set_visuals(egui::Visuals::dark());
        install_chinese_font(&context.egui_ctx);
        index::warm_up_parallel_search();
        let hotkey_requested = Arc::new(AtomicBool::new(false));
        let (hotkey_manager, hotkey_status) =
            install_global_hotkey(&context.egui_ctx, Arc::clone(&hotkey_requested));
        let mut app = match index::load() {
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
        app
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
        if self.query == self.last_query && self.category == self.last_category {
            return;
        }
        let started = Instant::now();
        self.results = index::search_filtered(&self.index, &self.query, 500, self.category);
        self.selected_result = self.results.first().copied();
        self.query_ms = started.elapsed().as_secs_f64() * 1_000.0;
        self.last_query.clone_from(&self.query);
        self.last_category = self.category;
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
        notify::EventKind::Create(_)
            | notify::EventKind::Remove(_)
            | notify::EventKind::Modify(ModifyKind::Name(_))
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

#[cfg(test)]
mod tests {
    #[test]
    fn smoke_test() {
        assert_eq!(2 + 2, 4);
    }
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
        self.receive_scan_updates(&context);
        self.receive_file_updates(&context);

        egui::CentralPanel::default().show(root_ui, |ui| {
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.heading(RichText::new("随手找").size(25.0));
                ui.add_space(10.0);
                ui.label(RichText::new("⌥ Space · 文件名与路径").color(Color32::GRAY));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled_ui(!self.scanning, |ui| {
                        ui.menu_button("索引", |ui| {
                            if ui.button("全盘索引").clicked() {
                                self.start_scan(index::full_disk_roots());
                                ui.close();
                            }
                            if ui.button("选择一个或多个位置…").clicked() {
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
                });
            });
            ui.add_space(8.0);
            let response = ui.add_sized(
                [ui.available_width(), 42.0],
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("输入文件名、扩展名或路径，可用空格组合关键词")
                    .font(egui::TextStyle::Heading),
            );
            if context.input(|input| input.key_pressed(egui::Key::Escape)) {
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
            ui.horizontal_wrapped(|ui| {
                for kind in Category::ALL {
                    ui.selectable_value(&mut self.category, kind, kind.label());
                }
            });
            self.update_search();
            if context.input(|input| input.key_pressed(egui::Key::ArrowDown)) {
                self.move_selection(1);
            }
            if context.input(|input| input.key_pressed(egui::Key::ArrowUp)) {
                self.move_selection(-1);
            }
            if context.input(|input| input.key_pressed(egui::Key::Enter)) {
                if let Some(selected) = self.selected_result {
                    if let Err(error) = Self::open(&self.index.path(selected)) {
                        self.status = error;
                    }
                }
            }

            ui.add_space(8.0);
            ui.separator();

            if self.index.is_empty() && !self.scanning {
                ui.vertical_centered(|ui| {
                    ui.add_space(100.0);
                    ui.heading("建立第一个文件名索引");
                    ui.label(
                        "可一次选择多个位置，或直接建立全盘索引；受保护目录需要完全磁盘访问权限。",
                    );
                    ui.add_space(14.0);
                    if ui.button("选择一个或多个位置").clicked() {
                        self.choose_roots(true);
                    }
                });
            } else {
                let result_height = (ui.available_height() - 38.0).max(80.0);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(result_height)
                    .show_rows(ui, 42.0, self.results.len(), |ui, range| {
                        for &entry_index in &self.results[range] {
                            let path = self.index.path(entry_index);
                            let name = self.index.name(entry_index).to_owned();
                            let category = self.index.category(entry_index);
                            let icon = category.icon();
                            let selected = self.selected_result == Some(entry_index);
                            let frame = egui::Frame::new()
                                .fill(if selected {
                                    ui.visuals().selection.bg_fill
                                } else {
                                    Color32::TRANSPARENT
                                })
                                .inner_margin(egui::Margin::symmetric(5, 2))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        let icon_response = ui.add(
                                            egui::Label::new(icon).sense(egui::Sense::click()),
                                        );
                                        let text_response = ui
                                            .vertical(|ui| {
                                                let name_response = ui.add(
                                                    egui::Label::new(
                                                        RichText::new(format!(
                                                            "{}   · {}",
                                                            name,
                                                            category.label()
                                                        ))
                                                        .strong(),
                                                    )
                                                    .sense(egui::Sense::click()),
                                                );
                                                let path_response = ui.add(
                                                    egui::Label::new(
                                                        RichText::new(&path)
                                                            .small()
                                                            .color(Color32::GRAY),
                                                    )
                                                    .sense(egui::Sense::click()),
                                                );
                                                name_response.union(path_response)
                                            })
                                            .inner;
                                        icon_response.union(text_response)
                                    })
                                    .inner
                                });
                            let response = frame
                                .response
                                .interact(egui::Sense::click())
                                .union(frame.inner);
                            if response.clicked() {
                                self.selected_result = Some(entry_index);
                                if let Err(error) = Self::open(&path) {
                                    self.status = error;
                                }
                            }
                            response.context_menu(|ui| {
                                if ui.button("打开").clicked() {
                                    if let Err(error) = Self::open(&path) {
                                        self.status = error;
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
                            ui.separator();
                        }
                    });
            }

            ui.separator();
            ui.horizontal(|ui| {
                if self.scanning {
                    ui.spinner();
                }
                ui.label(&self.status);
                if !self.roots.is_empty() {
                    ui.label(
                        RichText::new(format!("· {} 个索引位置 · ⌥ Space", self.roots.len()))
                            .color(Color32::GRAY),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !self.query.is_empty() {
                        ui.label(format!(
                            "{} 个结果 · {:.2} ms",
                            self.results.len(),
                            self.query_ms
                        ));
                    }
                });
            });
        });

        if self.scanning {
            context.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}
