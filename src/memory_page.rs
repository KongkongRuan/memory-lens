use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Vec2};

use crate::app::format_bytes;
use crate::memory::{
    self, BenchmarkError, BenchmarkProgress, BenchmarkResult, BenchmarkStage, MemoryHardware,
    MemoryModule,
};
use crate::spd::{self, SpdDocument, SpdModule, TimingProfile};
use crate::sys::Snapshot;

const CARD_BG: Color32 = Color32::from_rgb(0x1B, 0x21, 0x2B);
const PANEL_BG: Color32 = Color32::from_rgb(0x16, 0x1C, 0x25);
const TEXT_MAIN: Color32 = Color32::from_rgb(0xE6, 0xEA, 0xF0);
const TEXT_DIM: Color32 = Color32::from_rgb(0xAF, 0xB8, 0xC5);
const ACCENT: Color32 = Color32::from_rgb(0x5C, 0xA9, 0xFF);
const GREEN: Color32 = Color32::from_rgb(0x32, 0xD5, 0x83);
const ORANGE: Color32 = Color32::from_rgb(0xFD, 0xB0, 0x22);
const RED: Color32 = Color32::from_rgb(0xF9, 0x70, 0x66);
const EMPTY: Color32 = Color32::from_rgb(0x35, 0x3E, 0x4C);

pub struct MemoryPage {
    hardware: Option<Result<MemoryHardware, String>>,
    hardware_loading: bool,
    hardware_started: bool,
    hardware_rx: Option<mpsc::Receiver<Result<MemoryHardware, String>>>,
    spd: Option<Result<SpdDocument, String>>,
    spd_loading: bool,
    spd_rx: Option<mpsc::Receiver<Result<SpdDocument, String>>>,
    selected_module: usize,
    selected_profile: usize,
    available_bytes: u64,
    benchmark_running: bool,
    benchmark_rx: Option<mpsc::Receiver<BenchmarkMessage>>,
    benchmark_cancel: Option<Arc<AtomicBool>>,
    benchmark_progress: Option<BenchmarkProgress>,
    benchmark_result: Option<BenchmarkResult>,
    benchmark_error: Option<BenchmarkError>,
}

enum BenchmarkMessage {
    Progress(BenchmarkProgress),
    Finished(Result<BenchmarkResult, BenchmarkError>),
}

impl MemoryPage {
    pub fn new() -> Self {
        Self {
            hardware: None,
            hardware_loading: false,
            hardware_started: false,
            hardware_rx: None,
            spd: None,
            spd_loading: false,
            spd_rx: None,
            selected_module: 0,
            selected_profile: 0,
            available_bytes: 0,
            benchmark_running: false,
            benchmark_rx: None,
            benchmark_cancel: None,
            benchmark_progress: None,
            benchmark_result: None,
            benchmark_error: None,
        }
    }

    pub fn tick(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        self.available_bytes = snapshot.physical_available;
        let hardware_message = match self.hardware_rx.as_ref() {
            Some(rx) => match rx.try_recv() {
                Ok(result) => Some(Ok(result)),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(())),
            },
            None => None,
        };
        if let Some(message) = hardware_message {
            self.hardware = Some(match message {
                Ok(result) => result,
                Err(()) => Err("硬件信息查询线程意外退出".into()),
            });
            self.hardware_loading = false;
            self.hardware_rx = None;
        }

        let spd_message = match self.spd_rx.as_ref() {
            Some(rx) => match rx.try_recv() {
                Ok(result) => Some(Ok(result)),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(())),
            },
            None => None,
        };
        if let Some(message) = spd_message {
            self.spd = Some(match message {
                Ok(result) => result,
                Err(()) => Err("SPD 读取线程意外退出".into()),
            });
            self.spd_loading = false;
            self.spd_rx = None;
            self.selected_module = 0;
            self.selected_profile = 0;
        }

        let mut messages = Vec::new();
        let mut disconnected = false;
        if let Some(rx) = &self.benchmark_rx {
            loop {
                match rx.try_recv() {
                    Ok(message) => messages.push(message),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        for message in messages {
            match message {
                BenchmarkMessage::Progress(progress) => self.benchmark_progress = Some(progress),
                BenchmarkMessage::Finished(result) => {
                    self.benchmark_running = false;
                    self.benchmark_rx = None;
                    self.benchmark_progress = None;
                    match result {
                        Ok(result) => self.benchmark_result = Some(result),
                        Err(error) => self.benchmark_error = Some(error),
                    }
                }
            }
        }
        if disconnected && self.benchmark_running {
            self.benchmark_running = false;
            self.benchmark_rx = None;
            self.benchmark_progress = None;
            self.benchmark_error = Some(BenchmarkError::Failed("测速线程意外退出".into()));
        }
        if self.hardware_loading || self.spd_loading || self.benchmark_running {
            ctx.request_repaint();
        }
    }

    pub fn shutdown(&mut self) {
        if let Some(cancel) = &self.benchmark_cancel {
            cancel.store(true, Ordering::Release);
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        if !self.hardware_started {
            self.start_hardware(ui.ctx());
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                match self.hardware.clone() {
                    Some(Ok(hardware)) => self.ui_hardware(ui, &hardware, snapshot),
                    Some(Err(error)) => {
                        section(ui, "内存基本信息", "Windows 固件与 WMI", |ui| {
                            ui.colored_label(RED, error);
                            if ui.button("重新查询").clicked() {
                                self.start_hardware(ui.ctx());
                            }
                        });
                    }
                    None => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(RichText::new("正在读取内存基本信息…").color(TEXT_DIM));
                        });
                        ui.add_space(12.0);
                    }
                }
                if self.hardware_loading && self.hardware.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("正在刷新硬件信息").size(12.5).color(TEXT_DIM));
                    });
                    ui.add_space(8.0);
                }
                self.ui_spd(ui);
                ui.add_space(12.0);
                self.ui_benchmark(ui);
                ui.add_space(8.0);
            });
    }

    fn start_hardware(&mut self, ctx: &egui::Context) {
        if self.hardware_loading {
            return;
        }
        self.hardware_started = true;
        self.hardware_loading = true;
        let (sender, receiver) = mpsc::channel();
        self.hardware_rx = Some(receiver);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = memory::collect_hardware();
            let _ = sender.send(result);
            ctx.request_repaint();
        });
    }

    fn start_spd(&mut self, ctx: &egui::Context) {
        if self.spd_loading {
            return;
        }
        self.spd_loading = true;
        let (sender, receiver) = mpsc::channel();
        self.spd_rx = Some(receiver);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = spd::read_spd();
            let _ = sender.send(result);
            ctx.request_repaint();
        });
    }

    fn start_benchmark(&mut self, ctx: &egui::Context) {
        if self.benchmark_running {
            return;
        }
        if self.available_bytes < 256 * 1024 * 1024 {
            self.benchmark_error = Some(BenchmarkError::Failed(
                "当前可用内存不足 256 MiB，请关闭部分程序后重试".into(),
            ));
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.benchmark_cancel = Some(cancel.clone());
        self.benchmark_running = true;
        self.benchmark_error = None;
        self.benchmark_result = None;
        self.benchmark_progress = Some(BenchmarkProgress {
            stage: BenchmarkStage::Preparing,
            completed: 0,
            total: 1,
        });
        let (sender, receiver) = mpsc::channel();
        self.benchmark_rx = Some(receiver);
        let ctx = ctx.clone();
        let available = self.available_bytes;
        std::thread::spawn(move || {
            let progress_sender = sender.clone();
            let progress_ctx = ctx.clone();
            let result = memory::run_benchmark(available, cancel, move |progress| {
                let _ = progress_sender.send(BenchmarkMessage::Progress(progress));
                progress_ctx.request_repaint();
            });
            let _ = sender.send(BenchmarkMessage::Finished(result));
            ctx.request_repaint();
        });
    }

    fn ui_hardware(&mut self, ui: &mut egui::Ui, hardware: &MemoryHardware, snapshot: &Snapshot) {
        let speed = speed_summary(hardware);
        let kind = kind_summary(hardware);
        let slots = hardware
            .total_slots
            .map(|total| format!("{} / {}", hardware.modules.len(), total))
            .unwrap_or_else(|| format!("{} / 未知", hardware.modules.len()));
        ui.columns(4, |columns| {
            summary_card(
                &mut columns[0],
                "已安装容量",
                &format_bytes(hardware.installed_bytes),
                &format!("{} 根内存", hardware.modules.len()),
            );
            summary_card(&mut columns[1], "当前配置频率", &speed.0, &speed.1);
            summary_card(&mut columns[2], "槽位使用", &slots, "已用 / 总槽位");
            summary_card(&mut columns[3], "内存规格", &kind.0, &kind.1);
        });
        ui.add_space(12.0);

        ui.columns(2, |columns| {
            section(&mut columns[0], "插槽与容量", "物理安装情况", |ui| {
                if let Some(total) = hardware.total_slots {
                    slot_bar(ui, hardware.modules.len(), total);
                    ui.label(
                        RichText::new(format!(
                            "{} 个已使用 · {} 个空闲",
                            hardware.modules.len(),
                            total.saturating_sub(hardware.modules.len())
                        ))
                        .color(TEXT_MAIN),
                    );
                } else {
                    ui.label(RichText::new("固件未提供总槽位数").color(TEXT_DIM));
                }
                ui.add_space(6.0);
                detail_row(ui, "已安装容量", &format_bytes(hardware.installed_bytes));
                detail_row(ui, "系统可寻址", &format_bytes(snapshot.physical_total));
                if let Some(maximum) = hardware.max_capacity_bytes {
                    detail_row(ui, "固件最大容量", &format_bytes(maximum));
                }
            });
            section(&mut columns[1], "运行时信息", "当前值来自 SMBIOS / WMI", |ui| {
                detail_row(ui, "当前配置频率", &speed.0);
                detail_row(ui, "标称频率", &rated_summary(hardware));
                let voltages: Vec<u32> = hardware.modules.iter().filter_map(|m| m.voltage_mv).collect();
                let voltage = if voltages.len() == hardware.modules.len() && !voltages.is_empty() {
                    let min = *voltages.iter().min().unwrap();
                    let max = *voltages.iter().max().unwrap();
                    if min == max {
                        format!("{:.3} V", min as f64 / 1000.0)
                    } else {
                        format!("{:.3} – {:.3} V", min as f64 / 1000.0, max as f64 / 1000.0)
                    }
                } else {
                    "未提供".into()
                };
                detail_row(ui, "配置电压", &voltage);
                ui.add_space(4.0);
                ui.label(
                    RichText::new("频率与电压是固件报告值，不是实时总线测量。")
                        .size(12.5)
                        .color(TEXT_DIM),
                );
            });
        });
        for warning in &hardware.warnings {
            ui.label(RichText::new(warning).size(12.5).color(ORANGE));
        }
        ui.add_space(12.0);
        section(ui, "内存条详情", "每根内存独立显示，不对混插参数做统一假设", |ui| {
            egui::Grid::new("memory_module_details")
                .num_columns(7)
                .striped(true)
                .spacing([14.0, 8.0])
                .show(ui, |ui| {
                    for title in ["插槽 / Bank", "容量", "规格", "当前 / 标称", "厂商", "型号", "电压"] {
                        ui.label(RichText::new(title).size(12.5).color(TEXT_DIM));
                    }
                    ui.end_row();
                    for module in &hardware.modules {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(nonempty(&module.device_locator)).color(TEXT_MAIN));
                            if !module.bank_label.trim().is_empty() {
                                ui.label(RichText::new(&module.bank_label).size(11.5).color(TEXT_DIM));
                            }
                        });
                        ui.label(format_bytes(module.capacity));
                        ui.vertical(|ui| {
                            ui.label(nonempty_opt(&module.memory_type));
                            if let Some(form) = &module.form_factor {
                                ui.label(RichText::new(form).size(11.5).color(TEXT_DIM));
                            }
                        });
                        ui.label(format!(
                            "{} / {}",
                            rate(module.configured_speed_mt_s),
                            rate(module.speed_mt_s)
                        ));
                        ui.label(nonempty(&module.manufacturer));
                        ui.label(nonempty(&module.part_number));
                        ui.label(
                            module
                                .voltage_mv
                                .map(|v| format!("{:.3} V", v as f64 / 1000.0))
                                .unwrap_or_else(|| "未提供".into()),
                        );
                        ui.end_row();
                    }
                });
        });
    }

    fn ui_spd(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(PANEL_BG)
            .corner_radius(CornerRadius::same(8))
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new("SPD 详细时序").size(17.0).strong().color(TEXT_MAIN));
                        ui.label(
                            RichText::new("JEDEC / XMP / EXPO 预设，非当前运行时值")
                                .size(12.5)
                                .color(TEXT_DIM),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.spd_loading {
                            ui.spinner();
                            ui.label(RichText::new("正在读取 SPD").size(13.0).color(TEXT_DIM));
                        } else {
                            let label = if self.spd.is_some() { "重新读取" } else { "读取详细时序" };
                            if ui.button(label).clicked() {
                                self.start_spd(ui.ctx());
                            }
                        }
                    });
                });
                ui.add_space(6.0);
                if self.spd.is_none() {
                    ui.label(
                        RichText::new("需要管理员权限和官方签名 PawnIO 驱动。首次读取会执行官方安装，驱动服务常驻；可在 Windows“已安装的应用”中卸载。")
                            .size(12.5)
                            .color(TEXT_DIM),
                    );
                }
                match self.spd.clone() {
                    Some(Ok(document)) => self.ui_spd_document(ui, &document),
                    Some(Err(error)) => {
                        ui.add_space(6.0);
                        ui.colored_label(RED, error);
                    }
                    None => {}
                }
            });
    }

    fn ui_spd_document(&mut self, ui: &mut egui::Ui, document: &SpdDocument) {
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("访问驱动：{}（常驻）", document.driver)).size(12.0).color(TEXT_DIM));
            ui.label(RichText::new("卸载：Windows 设置 → 应用 → PawnIO").size(12.0).color(TEXT_DIM));
        });
        for warning in &document.warnings {
            ui.label(RichText::new(warning).size(12.5).color(ORANGE));
        }
        if document.modules.is_empty() {
            ui.label(RichText::new("未检测到可用的 SPD 模块").color(TEXT_DIM));
            return;
        }
        self.selected_module = self.selected_module.min(document.modules.len() - 1);
        let module = &document.modules[self.selected_module];
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("内存条").color(TEXT_DIM));
            egui::ComboBox::from_id_salt("spd_module_selector")
                .selected_text(module_label(module))
                .width(360.0)
                .show_ui(ui, |ui| {
                    for (index, item) in document.modules.iter().enumerate() {
                        ui.selectable_value(&mut self.selected_module, index, module_label(item));
                    }
                });
            ui.label(RichText::new("预设").color(TEXT_DIM));
            if module.profiles.is_empty() {
                ui.label(RichText::new("未提供").color(TEXT_DIM));
            } else {
                self.selected_profile = self.selected_profile.min(module.profiles.len() - 1);
                let selected = self.selected_profile;
                egui::ComboBox::from_id_salt("spd_profile_selector")
                    .selected_text(&module.profiles[selected].title)
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for (index, profile) in module.profiles.iter().enumerate() {
                            ui.selectable_value(&mut self.selected_profile, index, &profile.title);
                        }
                    });
            }
        });
        if self.selected_profile >= module.profiles.len() {
            self.selected_profile = 0;
        }
        if let Some(error) = &module.error {
            ui.colored_label(ORANGE, error);
        }
        if let Some(profile) = module.profiles.get(self.selected_profile) {
            ui_profile(ui, module, profile);
        } else if !module.legacy_timings.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("基础 SPD 字段（工具包原始名称与数值）").color(TEXT_MAIN));
            egui::Grid::new("legacy_spd_timings").striped(true).spacing([20.0, 6.0]).show(ui, |ui| {
                for timing in &module.legacy_timings {
                    ui.label(&timing.name);
                    ui.label(&timing.value);
                    ui.end_row();
                }
            });
        } else {
            ui.label(RichText::new("该内存类型的详细预设暂无法解码，原始 SPD 保留在下方。").color(TEXT_DIM));
        }
        if !module.legacy_timings.is_empty() && !module.profiles.is_empty() {
            ui.add_space(6.0);
            egui::CollapsingHeader::new("更多基础 SPD 字段")
                .default_open(false)
                .show(ui, |ui| {
                    egui::Grid::new("more_legacy_timings").striped(true).spacing([18.0, 5.0]).show(ui, |ui| {
                        for timing in &module.legacy_timings {
                            ui.label(&timing.name);
                            ui.label(&timing.value);
                            ui.end_row();
                        }
                    });
                });
        }
        ui.add_space(6.0);
        egui::CollapsingHeader::new("原始 SPD 与诊断")
            .default_open(false)
            .show(ui, |ui| {
                if !module.raw.is_empty() {
                    let mut raw = format_raw(&module.raw);
                    ui.add(
                        egui::TextEdit::multiline(&mut raw)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .desired_rows(10)
                            .interactive(false),
                    );
                }
                if !document.diagnostics.trim().is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(&document.diagnostics).size(11.5).color(TEXT_DIM));
                }
            });
    }

    fn ui_benchmark(&mut self, ui: &mut egui::Ui) {
        let result = self.benchmark_result.clone();
        let error = self.benchmark_error.clone();
        let progress = self.benchmark_progress;
        let mut action = None;
        egui::Frame::new()
            .fill(PANEL_BG)
            .corner_radius(CornerRadius::same(8))
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.columns(2, |columns| {
                    columns[0].label(RichText::new("内存测速").size(17.0).strong().color(TEXT_MAIN));
                    columns[0].label(
                        RichText::new("顺序读取 / 写入 / 复制 · 多轮中位数")
                            .size(12.5)
                            .color(TEXT_DIM),
                    );
                    columns[0].add_space(6.0);
                    let buffer_text = result
                        .as_ref()
                        .map(|r| format!("缓冲区 {} × 2", format_bytes(r.buffer_bytes)))
                        .unwrap_or_else(|| "缓冲区按当前可用内存自动选择".into());
                    columns[0].label(RichText::new(buffer_text).size(12.5).color(TEXT_DIM));
                    columns[0].add_space(8.0);
                    if self.benchmark_running {
                        if columns[0].button("取消测速").clicked() {
                            action = Some(false);
                        }
                    } else {
                        let text = if result.is_some() { "重新测速" } else { "开始测速" };
                        if columns[0].button(text).clicked() {
                            action = Some(true);
                        }
                    }
                    if let Some(progress) = progress {
                        columns[0].add_space(8.0);
                        let fraction = if progress.total == 0 {
                            0.0
                        } else {
                            progress.completed as f32 / progress.total as f32
                        };
                        columns[0].add(
                            egui::ProgressBar::new(fraction.clamp(0.0, 1.0))
                                .text(format!("{}  {}/{}", stage_label(progress.stage), progress.completed, progress.total)),
                        );
                    }
                    if let Some(error) = &error {
                        columns[0].add_space(6.0);
                        match error {
                            BenchmarkError::Cancelled => {
                                columns[0].label(RichText::new("测速已取消").color(TEXT_DIM));
                            }
                            BenchmarkError::Failed(message) => {
                                columns[0].colored_label(RED, message);
                            }
                        }
                    }
                    columns[1].vertical(|ui| {
                        if let Some(result) = &result {
                            benchmark_row(ui, "顺序读取", result.read_gib_s, ACCENT);
                            benchmark_row(ui, "顺序写入", result.write_gib_s, ORANGE);
                            benchmark_row(ui, "内存复制", result.copy_gib_s, GREEN);
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(format!(
                                    "{} 轮中位数 · 用时 {:.1} 秒",
                                    result.samples, result.elapsed_seconds
                                ))
                                .size(12.0)
                                .color(TEXT_DIM),
                            );
                        } else {
                            ui.centered_and_justified(|ui| {
                                ui.label(RichText::new("完成测速后显示三项吞吐结果").color(TEXT_DIM));
                            });
                        }
                    });
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new("结果为本进程实测吞吐，受 CPU、缓存和后台负载影响，不等同于理论带宽或稳定性测试。")
                        .size(12.0)
                        .color(TEXT_DIM),
                );
            });
        match action {
            Some(true) => self.start_benchmark(ui.ctx()),
            Some(false) => {
                if let Some(cancel) = &self.benchmark_cancel {
                    cancel.store(true, Ordering::Release);
                }
            }
            None => {}
        }
    }
}

fn section(ui: &mut egui::Ui, title: &str, subtitle: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(PANEL_BG)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).size(16).strong().color(TEXT_MAIN));
            if !subtitle.is_empty() {
                ui.label(RichText::new(subtitle).size(12.0).color(TEXT_DIM));
            }
            ui.add_space(8.0);
            body(ui);
        });
}

fn summary_card(ui: &mut egui::Ui, label: &str, value: &str, sub: &str) {
    egui::Frame::new()
        .fill(CARD_BG)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(label).size(12.5).color(TEXT_DIM));
            ui.label(RichText::new(value).size(20).strong().color(TEXT_MAIN));
            ui.label(RichText::new(sub).size(11.5).color(TEXT_DIM));
        });
}

fn detail_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(13.0).color(TEXT_DIM));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).size(13.5).color(TEXT_MAIN));
        });
    });
}

fn slot_bar(ui: &mut egui::Ui, used: usize, total: usize) {
    if total == 0 {
        return;
    }
    let count = total.min(32);
    let width = (ui.available_width() - (count - 1) as f32 * 6.0) / count as f32;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for index in 0..count {
            let color = if index < used { ACCENT } else { EMPTY };
            let (rect, _) = ui.allocate_exact_size(Vec2::new(width.max(8.0), 24.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, CornerRadius::same(4), color);
        }
    });
    if total > count {
        ui.label(RichText::new(format!("共 {total} 个槽位")).size(12.0).color(TEXT_DIM));
    }
}

fn ui_profile(ui: &mut egui::Ui, module: &SpdModule, profile: &TimingProfile) {
    ui.add_space(10.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(&profile.core_timings).size(25).strong().color(ACCENT));
        if let Some(rate) = profile.data_rate_mt_s {
            let prefix = if module.memory_type.contains("DDR5") { "DDR5-" } else { "" };
            ui.label(RichText::new(format!("{prefix}{rate} MT/s")).size(16).strong().color(TEXT_MAIN));
        }
        match profile.crc_valid {
            Some(true) => {
                ui.label(RichText::new("CRC 通过").size(12.0).color(GREEN));
            }
            Some(false) => {
                ui.label(RichText::new("CRC 不匹配").size(12.0).color(RED));
            }
            None => {}
        }
    });
    ui.label(RichText::new(&profile.subtitle).size(12.0).color(TEXT_DIM));
    if !profile.voltage_summary.is_empty() {
        ui.label(RichText::new(&profile.voltage_summary).size(12.5).color(TEXT_DIM));
    }
    ui.add_space(8.0);
    egui::Grid::new("spd_timing_table")
        .num_columns(3)
        .striped(true)
        .spacing([22.0, 6.0])
        .show(ui, |ui| {
            for title in ["参数", "周期（nCK）", "时间"] {
                ui.label(RichText::new(title).size(12.5).color(TEXT_DIM));
            }
            ui.end_row();
            for row in &profile.rows {
                ui.label(RichText::new(&row.parameter).size(13.0).color(TEXT_MAIN));
                ui.label(&row.cycles);
                ui.label(&row.time);
                ui.end_row();
            }
        });
}

fn benchmark_row(ui: &mut egui::Ui, label: &str, value: f64, color: Color32) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(13.0).color(TEXT_DIM));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(format!("{value:.2} GiB/s")).size(19).strong().color(color));
        });
    });
}

fn speed_summary(hardware: &MemoryHardware) -> (String, String) {
    let mut values: Vec<u32> = hardware.modules.iter().filter_map(|m| m.configured_speed_mt_s).collect();
    values.sort_unstable();
    values.dedup();
    if values.len() == hardware.modules.len() && values.len() == 1 {
        (format!("{} MT/s", values[0]), "固件报告当前值".into())
    } else if values.is_empty() {
        ("未提供".into(), "固件未报告当前频率".into())
    } else {
        (
            "混插".into(),
            values.iter().map(u32::to_string).collect::<Vec<_>>().join(" / ") + " MT/s",
        )
    }
}

fn rated_summary(hardware: &MemoryHardware) -> String {
    let mut values: Vec<u32> = hardware.modules.iter().filter_map(|m| m.speed_mt_s).collect();
    values.sort_unstable();
    values.dedup();
    if values.is_empty() {
        "未提供".into()
    } else {
        values.iter().map(u32::to_string).collect::<Vec<_>>().join(" / ") + " MT/s"
    }
}

fn kind_summary(hardware: &MemoryHardware) -> (String, String) {
    let mut kinds: Vec<String> = hardware.modules.iter().filter_map(|m| m.memory_type.clone()).collect();
    kinds.sort();
    kinds.dedup();
    if kinds.len() == 1 {
        let forms: Vec<String> = hardware.modules.iter().filter_map(|m| m.form_factor.clone()).collect();
        (kinds.remove(0), if forms.is_empty() { "固件报告规格".into() } else { forms.join(" / ") })
    } else if kinds.is_empty() {
        ("未提供".into(), "未知规格".into())
    } else {
        ("混合规格".into(), kinds.join(" / "))
    }
}

fn module_label(module: &SpdModule) -> String {
    let mut label = format!("SPD 0x{:02X} · {}", module.address, module.memory_type);
    if module.capacity_gb > 0.0 {
        label.push_str(&format!(" · {:.1} GB", module.capacity_gb));
    }
    let part = module.part_number.trim();
    if !part.is_empty() {
        label.push_str(" · ");
        label.push_str(part);
    }
    label
}

fn rate(value: Option<u32>) -> String {
    value.map(|v| format!("{v} MT/s")).unwrap_or_else(|| "未提供".into())
}

fn nonempty(value: &str) -> &str {
    if value.trim().is_empty() { "未提供" } else { value.trim() }
}

fn nonempty_opt(value: &Option<String>) -> &str {
    value.as_deref().map(nonempty).unwrap_or("未提供")
}

fn stage_label(stage: BenchmarkStage) -> &'static str {
    match stage {
        BenchmarkStage::Preparing => "准备与预热",
        BenchmarkStage::Read => "顺序读取",
        BenchmarkStage::Write => "顺序写入",
        BenchmarkStage::Copy => "内存复制",
    }
}

fn format_raw(raw: &[u8]) -> String {
    let mut text = String::new();
    for (index, chunk) in raw.chunks(16).enumerate() {
        text.push_str(&format!("{index:04X}: "));
        for byte in chunk {
            text.push_str(&format!("{byte:02X} "));
        }
        text.push('\n');
    }
    text
}
