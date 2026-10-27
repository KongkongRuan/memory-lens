use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, Pos2, Rect, RichText, Sense, Vec2,
};
use egui_plot::{Legend, Line, Plot};

use crate::sys::{self, Snapshot};

const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const HISTORY_POINTS: usize = 300;

mod colors {
    use eframe::egui::Color32;
    pub const PROCESS: Color32 = Color32::from_rgb(0x4C, 0x9A, 0xFF);
    pub const NONPAGED: Color32 = Color32::from_rgb(0xF9, 0x70, 0x66);
    pub const PAGED: Color32 = Color32::from_rgb(0xFD, 0xB0, 0x22);
    pub const COMPRESS: Color32 = Color32::from_rgb(0xB7, 0x9C, 0xFF);
    pub const CACHE: Color32 = Color32::from_rgb(0x32, 0xD5, 0x83);
    pub const MODIFIED: Color32 = Color32::from_rgb(0xFF, 0x8A, 0x5C);
    pub const STANDBY: Color32 = Color32::from_rgb(0x8B, 0x99, 0xAD);
    pub const FREE: Color32 = Color32::from_rgb(0x55, 0x5F, 0x6C);
    pub const BAD: Color32 = Color32::from_rgb(0xC0, 0x39, 0x2B);
    pub const CARD_BG: Color32 = Color32::from_rgb(0x1B, 0x21, 0x2B);
    pub const TEXT_DIM: Color32 = Color32::from_rgb(0xAF, 0xB8, 0xC5);
    pub const TEXT_MAIN: Color32 = Color32::from_rgb(0xE6, 0xEA, 0xF0);
    pub const ACCENT: Color32 = Color32::from_rgb(0x5C, 0xA9, 0xFF);
}

pub fn format_bytes(bytes: u64) -> String {
    const GB: u64 = 1 << 30;
    const MB: u64 = 1 << 20;
    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    } else {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    }
}

struct Segment {
    name: &'static str,
    bytes: u64,
    color: Color32,
    hint: &'static str,
}

struct HistPoint {
    t: f64,
    used_pct: f64,
    commit_pct: f64,
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Overview,
    Processes,
    MemoryInfo,
}

#[derive(PartialEq, Clone, Copy)]
enum ProcView {
    Merged,
    Single,
    Tree,
}

pub struct MemoryLensApp {
    snapshot: Snapshot,
    history: VecDeque<HistPoint>,
    started: Instant,
    last_collect: Instant,
    tab: Tab,
    proc_filter: String,
    proc_view: ProcView,
    // 待确认结束的进程 (pid, 名称)
    kill_confirm: Option<(Vec<u32>, String)>,
    kill_result: Option<String>,
    // 树状视图中折叠的 PID 集合
    collapsed: std::collections::HashSet<u32>,
    // "窗口定位"：是否正在跟踪光标
    pick_active: bool,
    // 最近一次采样到的光标下窗口信息（由后台钩子线程写入）
    pick_info: Arc<Mutex<Option<sys::WindowTarget>>>,
    // 通知后台钩子线程停止
    pick_stop: Arc<AtomicBool>,
    memory_page: crate::memory_page::MemoryPage,
}

impl MemoryLensApp {
    pub fn new() -> Self {
        let snapshot = sys::collect();
        let mut app = MemoryLensApp {
            snapshot,
            history: VecDeque::with_capacity(HISTORY_POINTS + 1),
            started: Instant::now(),
            last_collect: Instant::now(),
            tab: Tab::Overview,
            proc_filter: String::new(),
            proc_view: ProcView::Merged,
            kill_confirm: None,
            kill_result: None,
            collapsed: std::collections::HashSet::new(),
            pick_active: false,
            pick_info: Arc::new(Mutex::new(None)),
            pick_stop: Arc::new(AtomicBool::new(false)),
            memory_page: crate::memory_page::MemoryPage::new(),
        };
        app.push_history();
        app
    }

    pub fn tick(&mut self, ctx: &egui::Context) {
        self.memory_page.tick(ctx, &self.snapshot);
        if self.last_collect.elapsed() >= REFRESH_INTERVAL {
            self.snapshot = sys::collect();
            self.last_collect = Instant::now();
            self.push_history();
        }
    }

    pub fn shutdown(&mut self) {
        self.pick_stop.store(true, Ordering::SeqCst);
        self.memory_page.shutdown();
    }

    pub fn next_refresh_in(&self) -> Duration {
        REFRESH_INTERVAL.saturating_sub(self.last_collect.elapsed())
    }

    /// (使用中, 可用) —— RAMMap 口径：使用中 = 总量 − (Zero+Free+Standby+Modified+Bad)
    fn usage(&self) -> (u64, u64) {
        let s = &self.snapshot;
        match &s.page_lists {
            Some(pl) => {
                let free_like =
                    pl.zero + pl.free + pl.standby + pl.modified + pl.modified_no_write + pl.bad;
                let in_use = s.physical_total.saturating_sub(free_like);
                let available = pl.zero + pl.free + pl.standby;
                (in_use, available)
            }
            None => (
                s.physical_total.saturating_sub(s.physical_available),
                s.physical_available,
            ),
        }
    }

    /// 把物理内存分解为若干段，总和严格等于物理总量（最后一段兜底）。
    fn build_segments(&self) -> Vec<Segment> {
        let s = &self.snapshot;
        let mut segs = Vec::new();
        match &s.page_lists {
            Some(pl) => {
                let free = pl.zero + pl.free;
                let modified = pl.modified + pl.modified_no_write;
                let in_use = s
                    .physical_total
                    .saturating_sub(free + pl.standby + modified + pl.bad);
                let known =
                    s.nonpaged_pool + s.paged_pool_resident + s.compression + s.system_cache;
                let other = in_use.saturating_sub(known);
                segs.push(Segment {
                    name: "进程与应用",
                    bytes: other,
                    color: colors::PROCESS,
                    hint: "所有用户进程实际占用的物理内存（含共享页），以及驱动锁定、虚拟机占用等。点击可查看各进程明细",
                });
                segs.push(Segment {
                    name: "内存压缩",
                    bytes: s.compression,
                    color: colors::COMPRESS,
                    hint: "Windows 把不活跃页面压缩后存放的区域，任务管理器里隐藏在 System 进程中",
                });
                segs.push(Segment {
                    name: "非分页池",
                    bytes: s.nonpaged_pool,
                    color: colors::NONPAGED,
                    hint: "内核与驱动使用的不可换出内存，驱动泄漏时它会持续增长",
                });
                segs.push(Segment {
                    name: "分页池(常驻)",
                    bytes: s.paged_pool_resident,
                    color: colors::PAGED,
                    hint: "内核与驱动使用的可换出内存中当前驻留物理内存的部分",
                });
                segs.push(Segment {
                    name: "系统缓存",
                    bytes: s.system_cache,
                    color: colors::CACHE,
                    hint: "系统文件缓存的工作集（正在活跃使用的缓存页）",
                });
                segs.push(Segment {
                    name: "已修改",
                    bytes: modified,
                    color: colors::MODIFIED,
                    hint: "已修改、等待写回磁盘的页面，写回前无法被立即回收",
                });
                segs.push(Segment {
                    name: "备用(缓存)",
                    bytes: pl.standby,
                    color: colors::STANDBY,
                    hint: "缓存的文件数据，需要时可立即回收，任务管理器把它计入“可用”",
                });
                segs.push(Segment {
                    name: "空闲",
                    bytes: free,
                    color: colors::FREE,
                    hint: "完全未使用的内存",
                });
                if pl.bad > 0 {
                    segs.push(Segment {
                        name: "坏页",
                        bytes: pl.bad,
                        color: colors::BAD,
                        hint: "硬件损坏被隔离的物理页",
                    });
                }
            }
            None => {
                let (in_use, available) = self.usage();
                segs.push(Segment {
                    name: "使用中（未细分）",
                    bytes: in_use,
                    color: colors::PROCESS,
                    hint: "以管理员身份运行可查看内核池、内存压缩等细分",
                });
                segs.push(Segment {
                    name: "可用",
                    bytes: available,
                    color: colors::FREE,
                    hint: "空闲内存 + 可随时回收的缓存",
                });
            }
        }
        segs
    }

    fn push_history(&mut self) {
        let s = &self.snapshot;
        let (in_use, _) = self.usage();
        let used_pct = if s.physical_total > 0 {
            in_use as f64 / s.physical_total as f64 * 100.0
        } else {
            0.0
        };
        let commit_pct = if s.commit_limit > 0 {
            s.commit_total as f64 / s.commit_limit as f64 * 100.0
        } else {
            0.0
        };
        self.history.push_back(HistPoint {
            t: self.started.elapsed().as_secs_f64(),
            used_pct,
            commit_pct,
        });
        while self.history.len() > HISTORY_POINTS {
            self.history.pop_front();
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        // 标题 + 占用百分比
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("内存透镜 Memory Lens")
                    .size(24.0)
                    .strong()
                    .color(colors::TEXT_MAIN),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (in_use, _) = self.usage();
                let used_pct = if self.snapshot.physical_total > 0 {
                    in_use as f32 / self.snapshot.physical_total as f32 * 100.0
                } else {
                    0.0
                };
                let color = if self.snapshot.physical_total == 0 {
                    colors::TEXT_DIM
                } else if used_pct < 60.0 {
                    colors::CACHE
                } else if used_pct < 80.0 {
                    colors::PAGED
                } else {
                    colors::NONPAGED
                };
                let percent = if self.snapshot.physical_total > 0 {
                    format!("{used_pct:.0}%")
                } else {
                    "--".into()
                };
                ui.label(RichText::new(percent).size(34.0).strong().color(color));
                ui.label(
                    RichText::new(" 物理内存使用 ")
                        .size(15.0)
                        .color(colors::TEXT_DIM),
                );
            });
        });
        ui.add_space(10.0);

        // 标签页
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut self.tab,
                Tab::Overview,
                RichText::new("  总览  ").size(16.0),
            );
            ui.selectable_value(
                &mut self.tab,
                Tab::Processes,
                RichText::new(format!("  进程明细 ({})  ", self.snapshot.processes.len()))
                    .size(16.0),
            );
            ui.selectable_value(
                &mut self.tab,
                Tab::MemoryInfo,
                RichText::new("  内存信息  ").size(16.0),
            );
        });
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(8.0);

        match self.tab {
            Tab::Overview => self.ui_overview(ui),
            Tab::Processes => self.ui_processes(ui),
            Tab::MemoryInfo => self.memory_page.ui(ui, &self.snapshot),
        }
    }

    fn ui_overview(&mut self, ui: &mut egui::Ui) {
        if self.snapshot.physical_total == 0 {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("无法读取系统内存信息").size(18.0));
            });
            return;
        }
        let s = &self.snapshot;
        let (in_use, available) = self.usage();
        let used_pct = in_use as f32 / s.physical_total as f32 * 100.0;
        let segments = self.build_segments();

        // 概览卡片
        ui.columns(4, |cols| {
            card(&mut cols[0], "物理总量", format_bytes(s.physical_total), "");
            card(
                &mut cols[1],
                "使用中",
                format_bytes(in_use),
                &format!("{used_pct:.1}%"),
            );
            card(&mut cols[2], "可用", format_bytes(available), "含备用缓存");
            card(
                &mut cols[3],
                "已提交",
                format!(
                    "{} / {}",
                    format_bytes(s.commit_total),
                    format_bytes(s.commit_limit)
                ),
                "含页面文件",
            );
        });
        ui.add_space(12.0);

        ui.label(
            RichText::new("物理内存分解")
                .size(17.0)
                .strong()
                .color(colors::TEXT_MAIN),
        );
        ui.label(
            RichText::new("点击“进程与应用”段可查看具体进程")
                .size(13.0)
                .color(colors::ACCENT),
        );
        ui.add_space(4.0);

        if let Some(clicked) = draw_breakdown_bar(ui, &segments) {
            if segments[clicked].name == "进程与应用" {
                self.tab = Tab::Processes;
            }
        }
        ui.add_space(6.0);

        ui.horizontal_wrapped(|ui| {
            for seg in &segments {
                if seg.bytes == 0 {
                    continue;
                }
                ui.label(RichText::new("●").size(13.0).color(seg.color));
                ui.label(
                    RichText::new(format!("{} {}", seg.name, format_bytes(seg.bytes)))
                        .size(13.0)
                        .color(colors::TEXT_MAIN),
                );
                ui.add_space(8.0);
            }
        });
        ui.add_space(12.0);

        // 明细 + 历史曲线
        ui.columns(2, |cols| {
            cols[0].label(
                RichText::new("分类明细")
                    .size(16.0)
                    .strong()
                    .color(colors::TEXT_MAIN),
            );
            egui::Grid::new("detail_grid")
                .num_columns(3)
                .striped(true)
                .spacing([20.0, 6.0])
                .show(&mut cols[0], |ui| {
                    ui.label(RichText::new("类别").size(13.0).color(colors::TEXT_DIM));
                    ui.label(RichText::new("大小").size(13.0).color(colors::TEXT_DIM));
                    ui.label(RichText::new("占比").size(13.0).color(colors::TEXT_DIM));
                    ui.end_row();
                    let total = s.physical_total as f64;
                    for seg in &segments {
                        if seg.bytes == 0 {
                            continue;
                        }
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("●").size(13.0).color(seg.color));
                            ui.label(
                                RichText::new(seg.name).size(14.0).color(colors::TEXT_MAIN),
                            );
                        });
                        ui.label(
                            RichText::new(format_bytes(seg.bytes))
                                .size(14.0)
                                .color(colors::TEXT_MAIN),
                        );
                        ui.label(
                            RichText::new(format!("{:.1}%", seg.bytes as f64 / total * 100.0))
                                .size(14.0)
                                .color(colors::TEXT_MAIN),
                        );
                        ui.end_row();
                    }
                });
            cols[0].add_space(8.0);
            cols[0].label(
                RichText::new(
                    "提示：共享内存在每个进程中被重复计算，所以各进程占用之和必然大于实际使用——\n这正是任务管理器里数字“对不上”的原因之一。",
                )
                .size(13.0)
                .color(colors::TEXT_DIM),
            );

            cols[1].label(
                RichText::new("历史趋势（最近 5 分钟）")
                    .size(16.0)
                    .strong()
                    .color(colors::TEXT_MAIN),
            );
            let now = self.started.elapsed().as_secs_f64();
            let used: Vec<[f64; 2]> = self.history.iter().map(|p| [p.t, p.used_pct]).collect();
            let commit: Vec<[f64; 2]> =
                self.history.iter().map(|p| [p.t, p.commit_pct]).collect();
            Plot::new("history")
                .height(220.0)
                .include_y(0.0)
                .include_y(100.0)
                .allow_zoom(false)
                .allow_drag(false)
                .allow_scroll(false)
                .legend(Legend::default())
                .x_axis_formatter(move |mark, _| format!("-{:.0}s", (now - mark.value).max(0.0)))
                .y_axis_formatter(|mark, _| format!("{:.0}%", mark.value))
                .show(&mut cols[1], |plot_ui| {
                    plot_ui.line(
                        Line::new(used)
                            .name("物理内存使用")
                            .color(colors::PROCESS),
                    );
                    plot_ui.line(
                        Line::new(commit)
                            .name("已提交(对上限)")
                            .color(colors::PAGED),
                    );
                });
        });
    }

    fn ui_processes(&mut self, ui: &mut egui::Ui) {
        if self.snapshot.physical_total == 0 {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("无法读取系统内存与进程信息").size(18.0));
            });
            return;
        }
        // 顶部工具行
        let mut back = false;
        let mut view = self.proc_view;
        ui.horizontal(|ui| {
            if ui
                .button(RichText::new("← 返回总览").size(14.0))
                .clicked()
            {
                back = true;
            }
            ui.add_space(16.0);
            ui.label(RichText::new("搜索：").size(14.0).color(colors::TEXT_DIM));
            ui.add(
                egui::TextEdit::singleline(&mut self.proc_filter)
                    .desired_width(160.0)
                    .hint_text("进程名…"),
            );
            ui.add_space(12.0);
            ui.label(RichText::new("视图：").size(14.0).color(colors::TEXT_DIM));
            ui.selectable_value(&mut view, ProcView::Merged, RichText::new("合并同名").size(14.0));
            ui.selectable_value(&mut view, ProcView::Single, RichText::new("单进程").size(14.0));
            ui.selectable_value(&mut view, ProcView::Tree, RichText::new("树状").size(14.0));
            ui.add_space(16.0);
            let pick_label = if self.pick_active {
                RichText::new("⏹ 停止定位").size(14.0)
            } else {
                RichText::new("🎯 窗口定位").size(14.0)
            };
            if ui.button(pick_label).clicked() {
                if self.pick_active {
                    self.stop_pick();
                } else {
                    self.start_pick(ui.ctx());
                }
            }
            if self.pick_active {
                ui.label(
                    RichText::new("移动鼠标到目标窗口，点击或按 Esc 停止")
                        .size(12.5)
                        .color(colors::ACCENT),
                );
            }
        });
        self.proc_view = view;
        if back {
            self.stop_pick();
            self.tab = Tab::Overview;
            return;
        }

        // 结束进程结果提示
        if let Some(msg) = self.kill_result.clone() {
            ui.add_space(4.0);
            ui.label(RichText::new(msg).size(13.0).color(colors::TEXT_DIM));
        }
        ui.add_space(4.0);

        // 把进程数据克隆到局部，避免渲染时与 &mut self 冲突
        let procs: Vec<sys::ProcessInfo> = self.snapshot.processes.clone();
        let physical_total = self.snapshot.physical_total.max(1) as f64;
        let proc_count = procs.len();
        let total_ws: u64 = procs.iter().map(|p| p.working_set).sum();
        ui.label(
            RichText::new(format!(
                "共 {proc_count} 个进程，工作集合计 {}（含共享页，会大于实际物理占用）",
                format_bytes(total_ws)
            ))
            .size(13.0)
            .color(colors::TEXT_DIM),
        );
        ui.add_space(4.0);

        // 计算 Top5 阈值（按单进程工作集）
        let mut sorted_ws: Vec<u64> = procs.iter().map(|p| p.working_set).collect();
        sorted_ws.sort_unstable_by(|a, b| b.cmp(a));
        let top_threshold = sorted_ws.get(4).copied().unwrap_or(u64::MAX);

        // 表头
        self.ui_proc_header(ui);
        ui.separator();

        let filter = self.proc_filter.to_lowercase();
        let view = self.proc_view;

        let mut kill_req: Option<(Vec<u32>, String)> = None;

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                match view {
                    ProcView::Merged => {
                        let rows = build_merged_rows(&procs, &filter);
                        let max_ws = rows.first().map(|r| r.ws).unwrap_or(1).max(1);
                        for row in &rows {
                            if let Some(k) = self.ui_proc_row(
                                ui,
                                &row.display_name,
                                &row.pid_label,
                                &row.parent_label,
                                &row.cmdline,
                                row.ws,
                                &row.pids,
                                max_ws,
                                physical_total,
                                row.ws >= top_threshold,
                                false,
                                0,
                            ) {
                                kill_req = Some(k);
                            }
                        }
                    }
                    ProcView::Single => {
                        let max_ws =
                            procs.first().map(|p| p.working_set).unwrap_or(1).max(1);
                        for p in &procs {
                            if !filter.is_empty() && !p.name.to_lowercase().contains(&filter) {
                                continue;
                            }
                            let parent = parent_label(&procs, p.ppid);
                            if let Some(k) = self.ui_proc_row(
                                ui,
                                &p.name,
                                &p.pid.to_string(),
                                &parent,
                                &p.cmdline,
                                p.working_set,
                                std::slice::from_ref(&p.pid),
                                max_ws,
                                physical_total,
                                p.working_set >= top_threshold,
                                false,
                                0,
                            ) {
                                kill_req = Some(k);
                            }
                        }
                    }
                    ProcView::Tree => {
                        kill_req =
                            self.ui_proc_tree(ui, &procs, &filter, top_threshold, physical_total);
                    }
                }
            });

        if let Some(k) = kill_req {
            self.kill_confirm = Some(k);
        }

        // 结束进程确认弹窗
        self.ui_kill_confirm(ui.ctx());

        // "窗口定位"悬浮面板
        self.ui_pick_overlay(ui.ctx());
    }

    /// 开始"窗口定位"：启动后台线程轮询光标下的窗口。
    fn start_pick(&mut self, ctx: &egui::Context) {
        self.pick_stop.store(false, Ordering::SeqCst);
        let stop = self.pick_stop.clone();
        let info = self.pick_info.clone();
        let ctx = ctx.clone();
        self.pick_active = true;

        std::thread::spawn(move || {
            use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
            // 本进程 PID，用于判断点击是否落在自己窗口上（面板按钮点击不应触发停止）
            let self_pid = std::process::id();
            let mut primed = false;
            while !stop.load(Ordering::SeqCst) {
                let down = unsafe {
                    (GetAsyncKeyState(0x01) as u16 & 0x8000 != 0) // VK_LBUTTON
                        || (GetAsyncKeyState(0x02) as u16 & 0x8000 != 0) // VK_RBUTTON
                };
                if down {
                    // 点击落在本程序窗口内 -> 忽略（让用户能点"停止"/"结束该进程"）
                    let on_self = sys::window_at_cursor()
                        .map(|t| t.pid == self_pid)
                        .unwrap_or(false);
                    if primed && !on_self {
                        stop.store(true, Ordering::SeqCst);
                        ctx.request_repaint();
                        break;
                    }
                } else {
                    primed = true;
                }

                if let Some(t) = sys::window_at_cursor() {
                    if let Ok(mut g) = info.lock() {
                        *g = Some(t);
                    }
                    ctx.request_repaint();
                }
                std::thread::sleep(Duration::from_millis(80));
            }
        });
    }

    fn stop_pick(&mut self) {
        self.pick_active = false;
        self.pick_stop.store(true, Ordering::SeqCst);
        if let Ok(mut g) = self.pick_info.lock() {
            *g = None;
        }
    }

    /// 在屏幕上方悬浮显示"窗口定位"结果。
    fn ui_pick_overlay(&mut self, ctx: &egui::Context) {
        if !self.pick_active {
            return;
        }
        // Esc 停止
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.stop_pick();
            return;
        }
        let info = self.pick_info.lock().ok().and_then(|g| g.clone());
        let screen = ctx.screen_rect();
        let anchor = Pos2::new(screen.center().x, screen.top() + 60.0);

        let mut kill_req: Option<(Vec<u32>, String)> = None;

        egui::Area::new(egui::Id::new("pick_overlay"))
            .order(egui::Order::Foreground)
            .fixed_pos(anchor)
            .pivot(egui::Align2::CENTER_TOP)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(Color32::from_rgb(0x1E, 0x24, 0x30))
                    .stroke(egui::Stroke::new(1.0_f32, Color32::from_rgb(0x3A, 0x44, 0x55)))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(egui::Margin::symmetric(16, 12))
                    .show(ui, |ui| {
                        ui.set_min_width(360.0);
                        ui.label(
                            RichText::new("🎯 窗口定位中")
                                .size(15.0)
                                .strong()
                                .color(colors::ACCENT),
                        );
                        ui.add_space(4.0);
                        match &info {
                            Some(t) => {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new("进程：").size(14.0).color(colors::TEXT_DIM),
                                    );
                                    ui.label(
                                        RichText::new(&t.name)
                                            .size(15.0)
                                            .strong()
                                            .color(colors::TEXT_MAIN),
                                    );
                                    ui.label(
                                        RichText::new(format!("PID {}", t.pid))
                                            .size(13.5)
                                            .color(colors::TEXT_DIM)
                                            .monospace(),
                                    );
                                });
                                if !t.title.is_empty() {
                                    ui.label(
                                        RichText::new(format!("窗口：{}", t.title))
                                            .size(13.0)
                                            .color(colors::TEXT_DIM),
                                    );
                                }
                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    if ui
                                        .button(
                                            RichText::new("结束该进程")
                                                .size(13.5)
                                                .color(Color32::from_rgb(0xFF, 0x6B, 0x6B)),
                                        )
                                        .clicked()
                                    {
                                        kill_req = Some((vec![t.pid], t.name.clone()));
                                    }
                                    if ui
                                        .button(RichText::new("停止").size(13.5))
                                        .clicked()
                                    {
                                        self.stop_pick();
                                    }
                                });
                            }
                            None => {
                                ui.label(
                                    RichText::new("移动鼠标到目标窗口…")
                                        .size(13.0)
                                        .color(colors::TEXT_DIM),
                                );
                            }
                        }
                    });
            });

        if let Some(k) = kill_req {
            self.stop_pick();
            self.kill_confirm = Some(k);
        }
    }

    fn ui_proc_header(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let heads = [("进程名", 200.0), ("PID", 70.0), ("父进程", 120.0)];
            for (h, w) in heads {
                ui.allocate_ui_with_layout(
                    Vec2::new(w, 18.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.label(RichText::new(h).size(12.0).color(colors::TEXT_DIM));
                    },
                );
            }
            ui.allocate_ui_with_layout(
                Vec2::new(240.0, 18.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.label(RichText::new("占用").size(12.0).color(colors::TEXT_DIM));
                },
            );
            ui.label(RichText::new("大小      占比").size(12.0).color(colors::TEXT_DIM));
        });
    }

    /// 在屏幕上方画一个深色 tooltip（不依赖 egui 默认 popup 样式，避免白框）。
    fn paint_tooltip(ctx: &egui::Context, id: egui::Id, anchor: Rect, add: impl FnOnce(&mut egui::Ui)) {
        egui::Area::new(id)
            .order(egui::Order::Tooltip)
            .fixed_pos(anchor.left_bottom() + Vec2::new(0.0, 4.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(Color32::from_rgb(0x1E, 0x24, 0x30))
                    .stroke(egui::Stroke::new(1.0_f32, Color32::from_rgb(0x3A, 0x44, 0x55)))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, add);
            });
    }

    /// 渲染一行进程，返回请求结束的 (pid, 名称)。
    #[allow(clippy::too_many_arguments)]
    fn ui_proc_row(
        &mut self,
        ui: &mut egui::Ui,
        name: &str,
        pid_label: &str,
        parent_label: &str,
        cmdline: &str,
        ws: u64,
        kill_pids: &[u32],
        max_ws: u64,
        physical_total: f64,
        is_top: bool,
        tree_toggle: bool,
        depth: usize,
    ) -> Option<(Vec<u32>, String)> {
        let pct_phys = ws as f64 / physical_total * 100.0;
        let mut kill = None;
        let name_color = if is_top {
            Color32::from_rgb(0xFF, 0x6B, 0x6B)
        } else {
            colors::TEXT_MAIN
        };
        // 整行包成一个可交互区域，右键点在行内任意位置都触发菜单
        let row_resp = ui
            .horizontal(|ui| {
                // 名称列
                let name_resp = ui
                    .allocate_ui_with_layout(
                        Vec2::new(200.0, 22.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.add_space(depth as f32 * 16.0);
                            if tree_toggle {
                                ui.add(egui::Label::new(
                                    RichText::new("▾").size(12.0).color(colors::TEXT_DIM),
                                )
                                .selectable(false).sense(Sense::hover()));
                            } else if depth > 0 {
                                ui.add(egui::Label::new(
                                    RichText::new("·").size(12.0).color(colors::TEXT_DIM),
                                )
                                .selectable(false).sense(Sense::hover()));
                            }
                            let text = if is_top {
                                RichText::new(format!("▲ {name}"))
                                    .size(14.0)
                                    .color(name_color)
                                    .strong()
                            } else {
                                RichText::new(name).size(14.0).color(name_color)
                            };
                            // 不可选中的 label，避免文本光标吞掉右键
                            ui.add(egui::Label::new(text).selectable(false).sense(Sense::hover()));
                        },
                    )
                    .response;
                if !cmdline.is_empty() && name_resp.hovered() {
                    let ctx = ui.ctx().clone();
                    let anchor = name_resp.rect;
                    let text = cmdline.to_string();
                    Self::paint_tooltip(&ctx, egui::Id::new("cmdline_tt"), anchor, |ui| {
                        ui.set_max_width(480.0);
                        ui.label(RichText::new("命令行").size(12.0).color(colors::TEXT_DIM));
                        ui.label(
                            RichText::new(text)
                                .size(12.5)
                                .color(colors::TEXT_MAIN)
                                .monospace(),
                        );
                    });
                }
                // PID 列
                ui.allocate_ui_with_layout(
                    Vec2::new(70.0, 22.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(pid_label)
                                    .size(13.0)
                                    .color(colors::TEXT_DIM)
                                    .monospace(),
                            )
                            .selectable(false).sense(Sense::hover()),
                        );
                    },
                );
                // 父进程列
                ui.allocate_ui_with_layout(
                    Vec2::new(120.0, 22.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(parent_label).size(13.0).color(colors::ACCENT),
                            )
                            .selectable(false).sense(Sense::hover()),
                        );
                    },
                );
                // 占用条
                let (bar_rect, _) = ui.allocate_exact_size(Vec2::new(240.0, 16.0), Sense::hover());
                let frac = (ws as f32 / max_ws as f32).clamp(0.0, 1.0);
                let mut fill = bar_rect;
                fill.set_width((bar_rect.width() * frac).max(3.0));
                let bar_color = if is_top {
                    Color32::from_rgb(0xFF, 0x6B, 0x6B)
                } else {
                    colors::PROCESS
                };
                ui.painter().rect_filled(fill, CornerRadius::same(3), bar_color);
                // 数值
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("{:>10}  {:>5.1}%", format_bytes(ws), pct_phys))
                            .size(14.0)
                            .color(name_color),
                    )
                    .selectable(false).sense(Sense::hover()),
                );
            })
            .response;
        // 整行右键菜单
        row_resp.context_menu(|ui| {
            // 复制 "名称 PID标签"，如 "chrome.exe ×17 8836~52948"
            let name_pid = if pid_label.is_empty() {
                name.to_string()
            } else {
                format!("{name} {pid_label}")
            };
            if ui.button("复制名称和 PID").clicked() {
                ui.ctx().copy_text(name_pid);
                ui.close_menu();
            }
            if !cmdline.is_empty() {
                if ui.button("复制命令行").clicked() {
                    ui.ctx().copy_text(cmdline.to_string());
                    ui.close_menu();
                }
            }
            if !kill_pids.is_empty() {
                let label = if kill_pids.len() > 1 {
                    format!("结束全部 {} 个进程", kill_pids.len())
                } else {
                    "结束进程".to_string()
                };
                if ui
                    .button(RichText::new(label).color(Color32::from_rgb(0xFF, 0x6B, 0x6B)))
                    .clicked()
                {
                    kill = Some((kill_pids.to_vec(), name.to_string()));
                    ui.close_menu();
                }
            }
        });
        ui.add_space(2.0);
        kill
    }

    /// 树状视图：按父子关系分组，可折叠。
    fn ui_proc_tree(
        &mut self,
        ui: &mut egui::Ui,
        procs: &[sys::ProcessInfo],
        filter: &str,
        top_threshold: u64,
        physical_total: f64,
    ) -> Option<(Vec<u32>, String)> {
        let mut kill = None;

        // children: ppid -> [pid]
        let mut children: std::collections::HashMap<u32, Vec<u32>> =
            std::collections::HashMap::new();
        let mut by_pid: std::collections::HashMap<u32, &sys::ProcessInfo> =
            std::collections::HashMap::new();
        for p in procs {
            by_pid.insert(p.pid, p);
            children.entry(p.ppid).or_default().push(p.pid);
        }
        // 根：父进程不在集合里的进程
        let mut roots: Vec<u32> = procs
            .iter()
            .filter(|p| !by_pid.contains_key(&p.ppid) || p.ppid == p.pid)
            .map(|p| p.pid)
            .collect();
        // 子树合计工作集
        fn subtree_ws(
            pid: u32,
            by_pid: &std::collections::HashMap<u32, &sys::ProcessInfo>,
            children: &std::collections::HashMap<u32, Vec<u32>>,
            memo: &mut std::collections::HashMap<u32, u64>,
        ) -> u64 {
            if let Some(&v) = memo.get(&pid) {
                return v;
            }
            let mut total = by_pid.get(&pid).map(|p| p.working_set).unwrap_or(0);
            if let Some(kids) = children.get(&pid) {
                for &k in kids {
                    if k != pid {
                        total += subtree_ws(k, by_pid, children, memo);
                    }
                }
            }
            memo.insert(pid, total);
            total
        }
        let mut memo = std::collections::HashMap::new();
        let mut ws_of = |pid: u32| subtree_ws(pid, &by_pid, &children, &mut memo);
        roots.sort_by_key(|&pid| std::cmp::Reverse(ws_of(pid)));

        let max_ws = roots.first().map(|&pid| ws_of(pid)).unwrap_or(1).max(1);

        // 过滤：保留自身或任一后代匹配过滤词的节点
        fn matches(
            pid: u32,
            filter: &str,
            by_pid: &std::collections::HashMap<u32, &sys::ProcessInfo>,
            children: &std::collections::HashMap<u32, Vec<u32>>,
        ) -> bool {
            let p = match by_pid.get(&pid) {
                Some(p) => p,
                None => return false,
            };
            if filter.is_empty() || p.name.to_lowercase().contains(filter) {
                return true;
            }
            children
                .get(&pid)
                .map(|kids| kids.iter().any(|&k| k != pid && matches(k, filter, by_pid, children)))
                .unwrap_or(false)
        }

        // 递归渲染
        #[allow(clippy::too_many_arguments)]
        fn render_node(
            app: &mut MemoryLensApp,
            ui: &mut egui::Ui,
            pid: u32,
            depth: usize,
            filter: &str,
            top_threshold: u64,
            physical_total: f64,
            max_ws: u64,
            by_pid: &std::collections::HashMap<u32, &sys::ProcessInfo>,
            children: &std::collections::HashMap<u32, Vec<u32>>,
            memo: &mut std::collections::HashMap<u32, u64>,
            kill: &mut Option<(Vec<u32>, String)>,
        ) {
            let Some(p) = by_pid.get(&pid) else { return };
            if !matches(pid, filter, by_pid, children) {
                return;
            }
            let kids: Vec<u32> = children
                .get(&pid)
                .map(|v| v.iter().copied().filter(|&k| k != pid).collect())
                .unwrap_or_default();
            let has_kids = !kids.is_empty();
            let total = subtree_ws(pid, by_pid, children, memo);
            let collapsed = app.collapsed.contains(&pid);
            let parent = parent_label_set(by_pid, p.ppid);

            // 行：点名称左侧箭头折叠/展开
            let row_kill = {
                let pct_phys = total as f64 / physical_total * 100.0;
                let is_top = total >= top_threshold;
                let name_color = if is_top {
                    Color32::from_rgb(0xFF, 0x6B, 0x6B)
                } else {
                    colors::TEXT_MAIN
                };
                let mut row_kill = None;
                let tree_row_resp = ui
                    .horizontal(|ui| {
                        let name_resp = ui
                            .allocate_ui_with_layout(
                                Vec2::new(200.0, 22.0),
                                Layout::left_to_right(Align::Center),
                                |ui| {
                                    ui.add_space(depth as f32 * 14.0);
                                    if has_kids {
                                        let arrow = if collapsed { "▸" } else { "▾" };
                                        if ui
                                            .selectable_label(
                                                false,
                                                RichText::new(arrow)
                                                    .size(13.0)
                                                    .color(colors::TEXT_DIM),
                                            )
                                            .clicked()
                                        {
                                            if collapsed {
                                                app.collapsed.remove(&pid);
                                            } else {
                                                app.collapsed.insert(pid);
                                            }
                                        }
                                    } else {
                                        ui.add_space(16.0);
                                    }
                                    let label = if has_kids {
                                        format!("{} ({})", p.name, kids.len())
                                    } else {
                                        p.name.clone()
                                    };
                                    let text = if is_top {
                                        RichText::new(format!("▲ {label}"))
                                            .size(14.0)
                                            .color(name_color)
                                            .strong()
                                    } else {
                                        RichText::new(label).size(14.0).color(name_color)
                                    };
                                    ui.add(egui::Label::new(text).selectable(false).sense(Sense::hover()));
                                },
                            )
                            .response;
                        if !p.cmdline.is_empty() && name_resp.hovered() {
                            let ctx = ui.ctx().clone();
                            let anchor = name_resp.rect;
                            let text = p.cmdline.clone();
                            MemoryLensApp::paint_tooltip(&ctx, egui::Id::new("cmdline_tt2"), anchor, |ui| {
                                ui.set_max_width(480.0);
                                ui.label(
                                    RichText::new("命令行")
                                        .size(12.0)
                                        .color(colors::TEXT_DIM),
                                );
                                ui.label(
                                    RichText::new(text)
                                        .size(12.5)
                                        .color(colors::TEXT_MAIN)
                                        .monospace(),
                                );
                            });
                        }
                        ui.allocate_ui_with_layout(
                            Vec2::new(70.0, 22.0),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(pid.to_string())
                                            .size(13.0)
                                            .color(colors::TEXT_DIM)
                                            .monospace(),
                                    )
                                    .selectable(false).sense(Sense::hover()),
                                );
                            },
                        );
                        ui.allocate_ui_with_layout(
                            Vec2::new(120.0, 22.0),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(parent).size(13.0).color(colors::ACCENT),
                                    )
                                    .selectable(false).sense(Sense::hover()),
                                );
                            },
                        );
                        let (bar_rect, _) =
                            ui.allocate_exact_size(Vec2::new(240.0, 16.0), Sense::hover());
                        let frac = (total as f32 / max_ws as f32).clamp(0.0, 1.0);
                        let mut fill = bar_rect;
                        fill.set_width((bar_rect.width() * frac).max(3.0));
                        let bar_color = if is_top {
                            Color32::from_rgb(0xFF, 0x6B, 0x6B)
                        } else {
                            colors::PROCESS
                        };
                        ui.painter().rect_filled(fill, CornerRadius::same(3), bar_color);
                        let size_text = if has_kids {
                            format!("{:>10}*", format_bytes(total))
                        } else {
                            format!("{:>10} ", format_bytes(total))
                        };
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("{size_text}  {pct_phys:>5.1}%"))
                                    .size(14.0)
                                    .color(name_color),
                            )
                            .selectable(false).sense(Sense::hover()),
                        );
                    })
                    .response;
                tree_row_resp.context_menu(|ui| {
                    let name_pid = format!("{} {}", p.name, pid);
                    if ui.button("复制名称和 PID").clicked() {
                        ui.ctx().copy_text(name_pid);
                        ui.close_menu();
                    }
                    if !p.cmdline.is_empty() && ui.button("复制命令行").clicked() {
                        ui.ctx().copy_text(p.cmdline.clone());
                        ui.close_menu();
                    }
                    if ui
                        .button(
                            RichText::new("结束进程").color(Color32::from_rgb(0xFF, 0x6B, 0x6B)),
                        )
                        .clicked()
                    {
                        row_kill = Some((vec![pid], p.name.clone()));
                        ui.close_menu();
                    }
                });
                ui.add_space(2.0);
                row_kill
            };
            if row_kill.is_some() {
                *kill = row_kill;
            }

            if has_kids && !collapsed {
                let mut sorted_kids = kids.clone();
                sorted_kids.sort_by_key(|&k| {
                    std::cmp::Reverse(subtree_ws(k, by_pid, children, memo))
                });
                for k in sorted_kids {
                    render_node(
                        app,
                        ui,
                        k,
                        depth + 1,
                        filter,
                        top_threshold,
                        physical_total,
                        max_ws,
                        by_pid,
                        children,
                        memo,
                        kill,
                    );
                }
            }
        }

        let mut sorted_roots = roots.clone();
        for pid in sorted_roots.drain(..) {
            render_node(
                self,
                ui,
                pid,
                0,
                filter,
                top_threshold,
                physical_total,
                max_ws,
                &by_pid,
                &children,
                &mut memo,
                &mut kill,
            );
        }

        ui.add_space(4.0);
        ui.label(
            RichText::new("* 含子进程合计")
                .size(12.0)
                .color(colors::TEXT_DIM),
        );
        kill
    }

    fn ui_kill_confirm(&mut self, ctx: &egui::Context) {
        let Some((pids, name)) = self.kill_confirm.clone() else { return };
        let count = pids.len();

        let red = Color32::from_rgb(0xE5, 0x4B, 0x4B);
        let mut action: Option<bool> = None; // Some(true)=确认, Some(false)=取消

        // 全屏半透明遮罩（拦截点击，阻止操作背后界面）
        let screen = ctx.screen_rect();
        egui::Area::new(egui::Id::new("kill_backdrop"))
            .order(egui::Order::Middle)
            .interactable(true)
            .show(ctx, |ui| {
                let resp = ui.allocate_rect(screen, Sense::click());
                ui.painter()
                    .rect_filled(screen, CornerRadius::same(0), Color32::from_black_alpha(140));
                if resp.clicked() {
                    action = Some(false); // 点击遮罩关闭
                }
            });

        // 固定尺寸的弹窗（宽 420，高由内容决定但用 fixed 框架防止累积）
        let win_size = Vec2::new(420.0, 300.0);
        let win_rect = Rect::from_center_size(screen.center(), win_size);

        egui::Area::new(egui::Id::new("kill_dialog"))
            .order(egui::Order::Foreground)
            .interactable(true)
            .show(ctx, |ui| {
                // 卡片背景
                ui.painter().rect(
                    win_rect,
                    CornerRadius::same(12),
                    Color32::from_rgb(0x1E, 0x24, 0x30),
                    egui::Stroke::new(1.0_f32, Color32::from_rgb(0x3A, 0x44, 0x55)),
                    egui::StrokeKind::Inside,
                );

                // 在 win_rect 内创建子 UI（留出内边距）
                let inner = win_rect.shrink2(Vec2::new(24.0, 22.0));
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(inner)
                        .layout(Layout::top_down(Align::LEFT)),
                );

                // 图标 + 标题
                child.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(48.0, 48.0), Sense::hover());
                    ui.painter()
                        .circle_filled(rect.center(), 24.0, Color32::from_rgb(0x4A, 0x2A, 0x2A));
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "⚠",
                        egui::FontId::proportional(26.0),
                        Color32::from_rgb(0xF0, 0x5C, 0x5C),
                    );
                    ui.add_space(14.0);
                    ui.vertical(|ui| {
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("结束进程")
                                .size(19.0)
                                .strong()
                                .color(colors::TEXT_MAIN),
                        );
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new("此操作无法撤销")
                                .size(13.0)
                                .color(colors::TEXT_DIM),
                        );
                    });
                });
                child.add_space(20.0);

                // 目标进程信息卡片
                egui::Frame::new()
                    .fill(Color32::from_rgb(0x16, 0x1B, 0x25))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(16, 14))
                    .show(&mut child, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("●").size(14.0).color(colors::PROCESS));
                            ui.label(
                                RichText::new(&name)
                                    .size(16.0)
                                    .strong()
                                    .color(colors::TEXT_MAIN),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let pid_text = if count > 1 {
                                    format!("{count} 个进程")
                                } else {
                                    format!("PID {}", pids[0])
                                };
                                ui.label(
                                    RichText::new(pid_text)
                                        .size(13.5)
                                        .color(colors::TEXT_DIM)
                                        .monospace(),
                                );
                            });
                        });
                    });
                child.add_space(16.0);

                child.label(
                    RichText::new(if count > 1 {
                        format!("这 {count} 个进程将被强制终止，未保存的数据会丢失。")
                    } else {
                        "该进程将被强制终止，未保存的数据会丢失。".to_string()
                    })
                    .size(14.0)
                    .color(colors::TEXT_DIM),
                );

                // 按钮固定在底部（用剩余空间下推）
                child.with_layout(Layout::bottom_up(Align::RIGHT), |ui| {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        let confirm = egui::Button::new(
                            RichText::new("  结束进程  ")
                                .size(15.0)
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(red)
                        .corner_radius(CornerRadius::same(6));
                        if ui.add(confirm).clicked() {
                            action = Some(true);
                        }
                        ui.add_space(12.0);
                        let cancel = egui::Button::new(
                            RichText::new("  取消  ")
                                .size(15.0)
                                .color(colors::TEXT_MAIN),
                        )
                        .fill(Color32::from_rgb(0x2A, 0x32, 0x40))
                        .stroke(egui::Stroke::new(
                            1.0_f32,
                            Color32::from_rgb(0x44, 0x4F, 0x60),
                        ))
                        .corner_radius(CornerRadius::same(6));
                        if ui.add(cancel).clicked() {
                            action = Some(false);
                        }
                    });
                });
            });

        match action {
            Some(true) => {
                let (ok, fail) = sys::kill_processes(&pids);
                self.kill_result = Some(if count > 1 {
                    if fail == 0 {
                        format!("已结束 {name} 全部 {ok} 个进程")
                    } else {
                        format!("{name}：已结束 {ok} 个，{fail} 个失败（权限不足或受保护）")
                    }
                } else if fail == 0 {
                    format!("已结束 {name} (PID {})", pids[0])
                } else {
                    format!("结束 {name} 失败：权限不足或进程受保护")
                });
                self.kill_confirm = None;
            }
            Some(false) => {
                self.kill_confirm = None;
            }
            None => {}
        }
    }
}

/// 合并同名进程后的行。
struct MergedRow {
    display_name: String,
    pid_label: String,
    parent_label: String,
    cmdline: String,
    ws: u64,
    pids: Vec<u32>,
}

fn parent_label(procs: &[sys::ProcessInfo], ppid: u32) -> String {
    let mut map = std::collections::HashMap::new();
    for p in procs {
        map.insert(p.pid, p);
    }
    parent_label_set(&map, ppid)
}

fn parent_label_set(
    by_pid: &std::collections::HashMap<u32, &sys::ProcessInfo>,
    ppid: u32,
) -> String {
    match by_pid.get(&ppid) {
        Some(par) => par.name.clone(),
        None if ppid == 0 => "—".into(),
        None => format!("(已退出 {ppid})"),
    }
}

fn build_merged_rows(procs: &[sys::ProcessInfo], filter: &str) -> Vec<MergedRow> {
    struct Agg {
        count: u32,
        ws: u64,
        min_pid: u32,
        max_pid: u32,
        ppids: Vec<u32>,
        pids: Vec<u32>,
        cmdline: String,
    }
    let mut map: std::collections::HashMap<String, Agg> = std::collections::HashMap::new();
    for p in procs {
        if !filter.is_empty() && !p.name.to_lowercase().contains(filter) {
            continue;
        }
        let e = map.entry(p.name.clone()).or_insert(Agg {
            count: 0,
            ws: 0,
            min_pid: p.pid,
            max_pid: p.pid,
            ppids: Vec::new(),
            pids: Vec::new(),
            cmdline: p.cmdline.clone(),
        });
        e.count += 1;
        e.ws += p.working_set;
        e.min_pid = e.min_pid.min(p.pid);
        e.max_pid = e.max_pid.max(p.pid);
        e.pids.push(p.pid);
        if !e.ppids.contains(&p.ppid) {
            e.ppids.push(p.ppid);
        }
        if e.cmdline.is_empty() && !p.cmdline.is_empty() {
            e.cmdline = p.cmdline.clone();
        }
    }
    let mut rows: Vec<MergedRow> = map
        .into_iter()
        .map(|(name, a)| {
            let display_name = if a.count > 1 {
                format!("{name} ×{}", a.count)
            } else {
                name
            };
            let pid_label = if a.count == 1 {
                format!("{}", a.min_pid)
            } else {
                format!("{}~{}", a.min_pid, a.max_pid)
            };
            let parent_label = if a.ppids.len() == 1 {
                parent_label(procs, a.ppids[0])
            } else {
                format!("{} 种", a.ppids.len())
            };
            MergedRow {
                display_name,
                pid_label,
                parent_label,
                cmdline: a.cmdline,
                ws: a.ws,
                pids: a.pids,
            }
        })
        .collect();
    rows.sort_by(|a, b| b.ws.cmp(&a.ws));
    rows
}

fn card(ui: &mut egui::Ui, label: &str, value: String, sub: &str) {
    egui::Frame::new()
        .fill(colors::CARD_BG)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(label).size(13.0).color(colors::TEXT_DIM));
            ui.label(
                RichText::new(value)
                    .size(20.0)
                    .strong()
                    .color(colors::TEXT_MAIN),
            );
            if !sub.is_empty() {
                ui.label(RichText::new(sub).size(12.0).color(colors::TEXT_DIM));
            }
        });
}

/// 返回被点击的段索引。
fn draw_breakdown_bar(ui: &mut egui::Ui, segments: &[Segment]) -> Option<usize> {
    let height = 46.0;
    let total: u64 = segments.iter().map(|s| s.bytes).sum();
    if total == 0 {
        return None;
    }
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
    let painter = ui.painter_at(rect);
    let nonzero: Vec<(usize, &Segment)> = segments
        .iter()
        .enumerate()
        .filter(|(_, s)| s.bytes > 0)
        .collect();
    let n = nonzero.len();
    let mut x = rect.left();
    let mut clicked = None;
    for (idx, (seg_i, seg)) in nonzero.iter().enumerate() {
        let w = if idx == n - 1 {
            rect.right() - x
        } else {
            rect.width() * seg.bytes as f32 / total as f32
        };
        let seg_rect =
            Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::new(w.max(0.0), height));
        let rounding = CornerRadius {
            nw: if idx == 0 { 6 } else { 0 },
            sw: if idx == 0 { 6 } else { 0 },
            ne: if idx == n - 1 { 6 } else { 0 },
            se: if idx == n - 1 { 6 } else { 0 },
        };
        painter.rect_filled(seg_rect, rounding, seg.color);
        x += w;

        let resp = ui.interact(seg_rect, ui.id().with("seg").with(seg_i), Sense::click());
        if resp.clicked() {
            clicked = Some(*seg_i);
        }
        if resp.hovered() {
            let ctx = ui.ctx().clone();
            let anchor = resp.rect;
            let (name, bytes, hint) = (seg.name, seg.bytes, seg.hint);
            MemoryLensApp::paint_tooltip(&ctx, egui::Id::new("seg_tt"), anchor, |ui| {
                ui.label(
                    RichText::new(name)
                        .size(15.0)
                        .strong()
                        .color(colors::TEXT_MAIN),
                );
                ui.label(
                    RichText::new(format!(
                        "{}（{:.1}%）",
                        format_bytes(bytes),
                        bytes as f64 / total as f64 * 100.0
                    ))
                    .size(14.0)
                    .color(colors::TEXT_MAIN),
                );
                ui.label(
                    RichText::new(hint)
                        .size(13.0)
                        .color(colors::TEXT_DIM),
                );
            });
        }
    }
    clicked
}
