use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Vec2};

use crate::app::format_bytes;
use crate::memory::{
    self, BenchmarkError, BenchmarkProgress, BenchmarkResult, BenchmarkStage, MemoryHardware,
    MemoryModule,
};
use crate::spd::{self, SpdDocument, SpdModule, TimingProfile, TimingRow};
use crate::sys::Snapshot;

const CARD_BG: Color32 = Color32::from_rgb(0x1B, 0x21, 0x2B);
const CARD_HOVER: Color32 = Color32::from_rgb(0x22, 0x2B, 0x38);
const CARD_BORDER: Color32 = Color32::from_rgb(0x30, 0x3A, 0x49);
const TAG_BG: Color32 = Color32::from_rgb(0x24, 0x2D, 0x39);
const PANEL_BG: Color32 = Color32::from_rgb(0x16, 0x1C, 0x25);
const TEXT_MAIN: Color32 = Color32::from_rgb(0xE6, 0xEA, 0xF0);
const TEXT_DIM: Color32 = Color32::from_rgb(0xAF, 0xB8, 0xC5);
const ACCENT: Color32 = Color32::from_rgb(0x5C, 0xA9, 0xFF);
const ACCENT_DARK: Color32 = Color32::from_rgb(0x17, 0x2D, 0x46);
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
            section(
                &mut columns[0],
                "插槽与容量",
                "物理安装情况",
                |ui| {
                    if let Some(total) = hardware.total_slots {
                        slot_bar(ui, hardware.modules.len(), total);
                        ui.label(
                            RichText::new("色块为数量示意，不代表实际槽位位置或顺序")
                                .size(11.5)
                                .color(TEXT_DIM),
                        );
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
                },
            );
            section(
                &mut columns[1],
                "运行时信息",
                "当前值来自 SMBIOS / WMI",
                |ui| {
                    detail_row(ui, "当前配置频率", &speed.0);
                    detail_row(ui, "标称频率", &rated_summary(hardware));
                    let voltages: Vec<u32> = hardware
                        .modules
                        .iter()
                        .filter_map(|m| m.voltage_mv)
                        .collect();
                    let voltage =
                        if voltages.len() == hardware.modules.len() && !voltages.is_empty() {
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
                },
            );
        });
        for warning in &hardware.warnings {
            ui.label(RichText::new(warning).size(12.5).color(ORANGE));
        }
        ui.add_space(12.0);
        section(
            ui,
            "内存条详情",
            "每根内存独立显示，不对混插参数做统一假设",
            |ui| {
                for (index, module) in hardware.modules.iter().enumerate() {
                    if index > 0 {
                        ui.add_space(8.0);
                    }
                    memory_module_card(ui, module);
                }
            },
        );
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
                            RichText::new(
                                "JEDEC 时序；DDR5 另含 XMP/EXPO 预设，均为 SPD 预设而非当前运行时值",
                            )
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
                            if action_button(ui, label).clicked() {
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
                    Some(Ok(document)) => {
                        let hardware = self
                            .hardware
                            .as_ref()
                            .and_then(|result| result.as_ref().ok())
                            .cloned();
                        self.ui_spd_document(ui, &document, hardware.as_ref());
                    }
                    Some(Err(error)) => {
                        ui.add_space(6.0);
                        ui.colored_label(RED, error);
                    }
                    None => {}
                }
            });
    }

    fn ui_spd_document(
        &mut self,
        ui: &mut egui::Ui,
        document: &SpdDocument,
        hardware: Option<&MemoryHardware>,
    ) {
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            tag(ui, nonempty(&document.driver), GREEN);
            ui.label(
                RichText::new("SPD 预设 · 非实时总线测量")
                    .size(11.5)
                    .color(TEXT_DIM),
            );
            ui.label(
                RichText::new("卸载：Windows 设置 → 应用 → PawnIO")
                    .size(11.5)
                    .color(TEXT_DIM),
            );
        });
        for warning in &document.warnings {
            ui.label(RichText::new(warning).size(12.5).color(ORANGE));
        }
        if document.modules.is_empty() {
            ui.label(RichText::new("未检测到可用的 SPD 模块").color(TEXT_DIM));
            return;
        }

        self.selected_module = self.selected_module.min(document.modules.len() - 1);
        let previous_module = self.selected_module;
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("当前模块").size(12.0).color(TEXT_DIM));
            if document.modules.len() == 1 {
                selector_value(ui, &module_label(&document.modules[self.selected_module]));
            } else {
                styled_module_selector(ui, &mut self.selected_module, &document.modules);
            }
        });
        if self.selected_module != previous_module {
            self.selected_profile = 0;
        }
        let module = &document.modules[self.selected_module];

        ui.add_space(10.0);
        spd_module_summary(ui, module, hardware);
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("预设").size(12.0).color(TEXT_DIM));
            if module.profiles.is_empty() {
                ui.label(RichText::new("未提供").color(TEXT_DIM));
            } else {
                self.selected_profile = self.selected_profile.min(module.profiles.len() - 1);
                let selected = self.selected_profile;
                let mut chosen = None;
                for (index, profile) in module.profiles.iter().enumerate() {
                    if profile_tab(ui, index == selected, &profile.title).clicked() {
                        chosen = Some(index);
                    }
                }
                if let Some(index) = chosen {
                    self.selected_profile = index;
                }
            }
        });
        if self.selected_profile >= module.profiles.len() {
            self.selected_profile = 0;
        }

        ui.add_space(4.0);
        ui.label(
            RichText::new(spd_profile_note(&module.memory_type))
                .size(11.5)
                .color(TEXT_DIM),
        );

        if let Some(error) = &module.error {
            ui.colored_label(ORANGE, error);
        }
        if let Some(profile) = module.profiles.get(self.selected_profile) {
            ui_profile(ui, module, profile);
        } else if !module.legacy_timings.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("基础 SPD 字段（工具包原始名称与数值）").color(TEXT_MAIN));
            egui::Grid::new("legacy_spd_timings")
                .striped(true)
                .spacing([20.0, 6.0])
                .show(ui, |ui| {
                    for timing in &module.legacy_timings {
                        ui.label(&timing.name);
                        ui.label(&timing.value);
                        ui.end_row();
                    }
                });
        } else {
            ui.label(
                RichText::new("该内存类型的详细预设暂无法解码，原始 SPD 保留在下方。")
                    .color(TEXT_DIM),
            );
        }
        if !module.legacy_timings.is_empty() && !module.profiles.is_empty() {
            ui.add_space(6.0);
            egui::CollapsingHeader::new("更多基础 SPD 字段")
                .default_open(false)
                .show(ui, |ui| {
                    egui::Grid::new("more_legacy_timings")
                        .striped(true)
                        .spacing([18.0, 5.0])
                        .show(ui, |ui| {
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
                    ui.label(
                        RichText::new(&document.diagnostics)
                            .size(11.5)
                            .color(TEXT_DIM),
                    );
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
            ui.label(RichText::new(title).size(16.0).strong().color(TEXT_MAIN));
            if !subtitle.is_empty() {
                ui.label(RichText::new(subtitle).size(12.0).color(TEXT_DIM));
            }
            ui.add_space(8.0);
            body(ui);
        });
}

fn memory_module_card(ui: &mut egui::Ui, module: &MemoryModule) {
    let voltage = module
        .voltage_mv
        .map(|value| format!("{:.3} V", value as f64 / 1000.0))
        .unwrap_or_else(|| "未提供".into());
    let bank = if module.bank_label.trim().is_empty() {
        "Bank 未提供".to_owned()
    } else {
        module.bank_label.trim().to_owned()
    };

    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0_f32, CARD_BORDER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(4.0, 44.0), egui::Sense::hover());
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(2), ACCENT);
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(nonempty(&module.device_locator))
                            .size(15.0)
                            .strong()
                            .color(TEXT_MAIN),
                    );
                    ui.label(RichText::new(bank).size(11.5).color(TEXT_DIM));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(format_bytes(module.capacity))
                                .size(19.0)
                                .strong()
                                .color(TEXT_MAIN),
                        );
                        ui.label(RichText::new("已安装容量").size(10.5).color(TEXT_DIM));
                    });
                });
            });

            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                tag(ui, nonempty_opt(&module.memory_type), ACCENT);
                if let Some(form) = &module.form_factor {
                    tag(ui, form, TEXT_DIM);
                }
            });

            ui.add_space(10.0);
            ui.columns(3, |columns| {
                metric(
                    &mut columns[0],
                    "当前频率",
                    &rate(module.configured_speed_mt_s),
                );
                metric(&mut columns[1], "标称频率", &rate(module.speed_mt_s));
                metric(&mut columns[2], "配置电压", &voltage);
            });
            ui.add_space(6.0);
            ui.columns(3, |columns| {
                metric(&mut columns[0], "厂商", nonempty(&module.manufacturer));
                metric(&mut columns[1], "型号", nonempty(&module.part_number));
                metric_mono(&mut columns[2], "序列号", nonempty(&module.serial_number));
            });
        });
}

fn tag(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(TAG_BG)
        .stroke(egui::Stroke::new(1.0_f32, CARD_BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(10.5).color(color));
        });
}

fn metric(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).size(10.5).color(TEXT_DIM));
        ui.label(RichText::new(value).size(13.0).color(TEXT_MAIN));
    });
}

fn metric_mono(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).size(10.5).color(TEXT_DIM));
        ui.label(RichText::new(value).size(12.0).monospace().color(TEXT_MAIN));
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
            ui.label(RichText::new(value).size(20.0).strong().color(TEXT_MAIN));
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
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(width.max(8.0), 24.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, CornerRadius::same(4), color);
        }
    });
    if total > count {
        ui.label(
            RichText::new(format!("共 {total} 个槽位"))
                .size(12.0)
                .color(TEXT_DIM),
        );
    }
}

fn action_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(
            RichText::new(text)
                .size(12.0)
                .strong()
                .color(Color32::from_rgb(0x08, 0x14, 0x20)),
        )
        .fill(ACCENT)
        .stroke(egui::Stroke::new(1.0_f32, ACCENT))
        .corner_radius(CornerRadius::same(7))
        .min_size(Vec2::new(0.0, 28.0)),
    )
}

fn selector_value(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0_f32, CARD_BORDER))
        .corner_radius(CornerRadius::same(7))
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(12.0).color(TEXT_MAIN));
        });
}

fn styled_module_selector(ui: &mut egui::Ui, selected: &mut usize, modules: &[SpdModule]) {
    ui.scope(|ui| {
        let widgets = &mut ui.style_mut().visuals.widgets;
        widgets.inactive.bg_fill = CARD_BG;
        widgets.inactive.weak_bg_fill = CARD_BG;
        widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, CARD_BORDER);
        widgets.inactive.fg_stroke.color = TEXT_MAIN;
        widgets.inactive.corner_radius = CornerRadius::same(7);
        widgets.hovered.bg_fill = CARD_HOVER;
        widgets.hovered.weak_bg_fill = CARD_HOVER;
        widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, ACCENT);
        widgets.hovered.fg_stroke.color = TEXT_MAIN;
        widgets.hovered.corner_radius = CornerRadius::same(7);
        widgets.active.bg_fill = ACCENT_DARK;
        widgets.active.weak_bg_fill = ACCENT_DARK;
        widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, ACCENT);
        widgets.active.fg_stroke.color = TEXT_MAIN;
        widgets.active.corner_radius = CornerRadius::same(7);

        egui::ComboBox::from_id_salt("spd_module_selector")
            .selected_text(module_label(&modules[*selected]))
            .width(360.0)
            .show_ui(ui, |ui| {
                for (index, item) in modules.iter().enumerate() {
                    ui.selectable_value(selected, index, module_label(item));
                }
            });
    });
}

fn spd_module_summary(ui: &mut egui::Ui, module: &SpdModule, hardware: Option<&MemoryHardware>) {
    let slot = hardware
        .and_then(|hardware| matching_slot(module, &hardware.modules))
        .unwrap_or("未关联插槽");

    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0_f32, CARD_BORDER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.columns(4, |columns| {
                metric(&mut columns[0], "总线", &bus_label(module));
                metric(
                    &mut columns[1],
                    "SPD 地址",
                    &format!("0x{:02X}", module.address),
                );
                metric(&mut columns[2], "模块厂商", nonempty(&module.manufacturer));
                metric(
                    &mut columns[3],
                    "DRAM 颗粒",
                    nonempty(&module.dram_manufacturer),
                );
            });
            ui.add_space(8.0);
            ui.columns(3, |columns| {
                metric(&mut columns[0], "型号", nonempty(&module.part_number));
                metric_mono(&mut columns[1], "序列号", nonempty(&module.serial_number));
                metric(&mut columns[2], "对应插槽", slot);
            });
        });
}

fn profile_tab(ui: &mut egui::Ui, selected: bool, text: &str) -> egui::Response {
    let (fill, color) = if selected {
        (ACCENT_DARK, ACCENT)
    } else {
        (CARD_BG, TEXT_DIM)
    };
    ui.add(
        egui::Button::new(RichText::new(text).size(11.5).color(color))
            .fill(fill)
            .stroke(egui::Stroke::new(
                1.0_f32,
                if selected { ACCENT } else { CARD_BORDER },
            ))
            .corner_radius(CornerRadius::same(12))
            .min_size(Vec2::new(0.0, 27.0)),
    )
}

fn ui_profile(ui: &mut egui::Ui, module: &SpdModule, profile: &TimingProfile) {
    ui.add_space(12.0);
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0_f32, CARD_BORDER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("核心时序").size(10.5).color(TEXT_DIM));
                    ui.label(
                        RichText::new(&profile.core_timings)
                            .size(26.0)
                            .strong()
                            .color(ACCENT),
                    );
                });
                if let Some(rate) = profile.data_rate_mt_s {
                    ui.add_space(24.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("数据率").size(10.5).color(TEXT_DIM));
                        ui.label(
                            RichText::new(format!(
                                "{}{} MT/s",
                                memory_type_prefix(&module.memory_type),
                                rate
                            ))
                            .size(17.0)
                            .strong()
                            .color(TEXT_MAIN),
                        );
                    });
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    match profile.crc_valid {
                        Some(true) => {
                            tag(ui, "CRC 通过", GREEN);
                        }
                        Some(false) => {
                            tag(ui, "CRC 不匹配", RED);
                        }
                        None => {}
                    }
                });
            });
            ui.add_space(4.0);
            ui.label(RichText::new(&profile.subtitle).size(11.5).color(TEXT_DIM));
            if !profile.voltage_summary.is_empty() {
                ui.label(
                    RichText::new(&profile.voltage_summary)
                        .size(11.5)
                        .color(TEXT_DIM),
                );
            }
        });

    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("时序明细")
                .size(14.0)
                .strong()
                .color(TEXT_MAIN),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new("周期（nCK） / 时间")
                    .size(10.5)
                    .color(TEXT_DIM),
            );
        });
    });
    let split = profile.rows.len().div_ceil(2);
    ui.add_space(6.0);
    ui.columns(2, |columns| {
        timing_table(&mut columns[0], &profile.rows[..split], "spd_timing_left");
        timing_table(&mut columns[1], &profile.rows[split..], "spd_timing_right");
    });
}

fn timing_table(ui: &mut egui::Ui, rows: &[TimingRow], id: &str) {
    egui::Frame::new()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(1.0_f32, CARD_BORDER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if rows.is_empty() {
                ui.label(RichText::new("未提供").size(12.0).color(TEXT_DIM));
                return;
            }
            egui::Grid::new(id)
                .num_columns(3)
                .striped(true)
                .spacing([10.0, 5.0])
                .show(ui, |ui| {
                    for title in ["参数", "周期", "时间"] {
                        ui.label(RichText::new(title).size(10.5).color(TEXT_DIM));
                    }
                    ui.end_row();
                    for row in rows {
                        ui.label(RichText::new(&row.parameter).size(12.0).color(TEXT_MAIN));
                        ui.label(
                            RichText::new(&row.cycles)
                                .size(11.0)
                                .monospace()
                                .color(TEXT_MAIN),
                        );
                        ui.label(
                            RichText::new(&row.time)
                                .size(11.0)
                                .monospace()
                                .color(TEXT_DIM),
                        );
                        ui.end_row();
                    }
                });
        });
}

fn spd_profile_note(memory_type: &str) -> &'static str {
    let memory_type = memory_type.to_ascii_uppercase();
    if memory_type.contains("DDR4") {
        "当前仅解析 JEDEC 时序；DDR4 XMP 2.0 预设尚未解析。"
    } else if memory_type.contains("DDR3") {
        "当前仅解析 JEDEC 时序。"
    } else if memory_type.contains("DDR5") {
        "JEDEC、XMP/EXPO 均为 SPD 预设，非当前运行时值。"
    } else {
        "当前仅显示可解析的 SPD 预设，非当前运行时值。"
    }
}

fn bus_label(module: &SpdModule) -> String {
    let name = module.bus_name.trim();
    if name.is_empty() {
        format!("总线 {}", module.bus)
    } else {
        format!("{name}（总线 {}）", module.bus)
    }
}

fn matching_slot<'a>(module: &SpdModule, modules: &'a [MemoryModule]) -> Option<&'a str> {
    let serial = normalize_match_key(&module.serial_number);
    if !serial.is_empty() {
        let mut matches = modules.iter().filter(|item| {
            !item.device_locator.trim().is_empty()
                && normalize_match_key(&item.serial_number) == serial
        });
        if let Some(first) = matches.next() {
            if matches.next().is_none() {
                return Some(first.device_locator.as_str());
            }
        }
        return None;
    }

    let part_number = normalize_match_key(&module.part_number);
    if !part_number.is_empty() {
        let mut matches = modules.iter().filter(|item| {
            !item.device_locator.trim().is_empty()
                && normalize_match_key(&item.part_number) == part_number
        });
        if let Some(first) = matches.next() {
            if matches.next().is_none() {
                return Some(first.device_locator.as_str());
            }
        }
    }

    None
}

fn normalize_match_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase()
}

fn display_memory_type(memory_type: &str) -> String {
    let upper = memory_type.to_ascii_uppercase();
    if upper.contains("DDR5") {
        "DDR5".to_owned()
    } else if upper.contains("DDR4") {
        "DDR4".to_owned()
    } else if upper.contains("DDR3") {
        "DDR3".to_owned()
    } else {
        nonempty(memory_type).to_owned()
    }
}

fn memory_type_prefix(memory_type: &str) -> &'static str {
    let memory_type = memory_type.to_ascii_uppercase();
    if memory_type.contains("DDR5") {
        "DDR5-"
    } else if memory_type.contains("DDR4") {
        "DDR4-"
    } else if memory_type.contains("DDR3") {
        "DDR3-"
    } else {
        ""
    }
}

fn benchmark_row(ui: &mut egui::Ui, label: &str, value: f64, color: Color32) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(13.0).color(TEXT_DIM));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format!("{value:.2} GiB/s"))
                    .size(19.0)
                    .strong()
                    .color(color),
            );
        });
    });
}

fn speed_summary(hardware: &MemoryHardware) -> (String, String) {
    let total = hardware.modules.len();
    let mut values: Vec<u32> = hardware
        .modules
        .iter()
        .filter_map(|module| module.configured_speed_mt_s)
        .collect();
    let provided = values.len();
    values.sort_unstable();
    values.dedup();

    if total == 0 || provided == 0 {
        ("未提供".into(), "固件未报告当前频率".into())
    } else if provided < total {
        (
            "部分未提供".into(),
            format!(
                "已提供 {} MT/s；{} 根缺字段",
                join_u32(&values),
                total - provided
            ),
        )
    } else if values.len() == 1 {
        (format!("{} MT/s", values[0]), "固件报告当前值".into())
    } else {
        ("混插".into(), format!("{} MT/s", join_u32(&values)))
    }
}

fn rated_summary(hardware: &MemoryHardware) -> String {
    let total = hardware.modules.len();
    let mut values: Vec<u32> = hardware
        .modules
        .iter()
        .filter_map(|module| module.speed_mt_s)
        .collect();
    let provided = values.len();
    values.sort_unstable();
    values.dedup();

    if total == 0 || provided == 0 {
        "未提供".into()
    } else if provided < total {
        format!("部分未提供（已提供 {} MT/s）", join_u32(&values))
    } else {
        format!("{} MT/s", join_u32(&values))
    }
}

fn kind_summary(hardware: &MemoryHardware) -> (String, String) {
    let total = hardware.modules.len();
    let mut kinds: Vec<String> = hardware
        .modules
        .iter()
        .filter_map(|module| module.memory_type.clone())
        .collect();
    let provided = kinds.len();
    kinds.sort();
    kinds.dedup();

    if total == 0 || provided == 0 {
        ("未提供".into(), "固件未报告规格".into())
    } else if provided < total {
        (
            "部分未提供".into(),
            format!(
                "已提供 {}；{} 根缺字段",
                kinds.join(" / "),
                total - provided
            ),
        )
    } else if kinds.len() == 1 {
        let mut forms: Vec<String> = hardware
            .modules
            .iter()
            .filter_map(|module| module.form_factor.clone())
            .collect();
        let form_count = forms.len();
        forms.sort();
        forms.dedup();
        let form_note = if forms.is_empty() {
            "固件报告规格".into()
        } else if form_count < total {
            format!("{}（部分未提供）", forms.join(" / "))
        } else {
            forms.join(" / ")
        };
        (kinds.remove(0), form_note)
    } else {
        ("混合规格".into(), kinds.join(" / "))
    }
}

fn join_u32(values: &[u32]) -> String {
    values
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(" / ")
}

fn module_label(module: &SpdModule) -> String {
    let bus_name = module.bus_name.trim();
    let bus = if bus_name.is_empty() {
        format!("总线 {}", module.bus)
    } else {
        bus_name.to_owned()
    };
    let mut label = format!(
        "{bus} · SPD 0x{:02X} · {}",
        module.address,
        display_memory_type(&module.memory_type)
    );
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
    value
        .map(|v| format!("{v} MT/s"))
        .unwrap_or_else(|| "未提供".into())
}

fn nonempty(value: &str) -> &str {
    if value.trim().is_empty() {
        "未提供"
    } else {
        value.trim()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_module(
        locator: &str,
        serial: &str,
        part: &str,
        configured_speed: Option<u32>,
        kind: Option<&str>,
        form: Option<&str>,
    ) -> MemoryModule {
        MemoryModule {
            device_locator: locator.to_owned(),
            bank_label: String::new(),
            capacity: 16 * 1024 * 1024 * 1024,
            speed_mt_s: Some(4800),
            configured_speed_mt_s: configured_speed,
            manufacturer: "Vendor".to_owned(),
            part_number: part.to_owned(),
            serial_number: serial.to_owned(),
            memory_type: kind.map(str::to_owned),
            form_factor: form.map(str::to_owned),
            voltage_mv: Some(1100),
        }
    }

    fn hardware(modules: Vec<MemoryModule>) -> MemoryHardware {
        MemoryHardware {
            modules,
            total_slots: Some(4),
            installed_bytes: 0,
            max_capacity_bytes: None,
            warnings: Vec::new(),
        }
    }

    fn spd_module(serial: &str, part: &str) -> SpdModule {
        SpdModule {
            bus: 0,
            bus_name: "SMBus".to_owned(),
            address: 0x50,
            memory_type: "DDR5".to_owned(),
            capacity_gb: 16.0,
            manufacturer: "Vendor".to_owned(),
            dram_manufacturer: "DRAM Vendor".to_owned(),
            part_number: part.to_owned(),
            serial_number: serial.to_owned(),
            raw: Vec::new(),
            profiles: Vec::new(),
            legacy_timings: Vec::new(),
            error: None,
        }
    }

    #[test]
    fn speed_summary_distinguishes_mixed_and_partial() {
        let same = hardware(vec![
            memory_module("A1", "1", "P1", Some(4800), Some("DDR5"), Some("DIMM")),
            memory_module("A2", "2", "P1", Some(4800), Some("DDR5"), Some("DIMM")),
        ]);
        assert_eq!(speed_summary(&same).0, "4800 MT/s");

        let mixed = hardware(vec![
            memory_module("A1", "1", "P1", Some(4800), Some("DDR5"), Some("DIMM")),
            memory_module("A2", "2", "P1", Some(5200), Some("DDR5"), Some("DIMM")),
        ]);
        assert_eq!(speed_summary(&mixed).0, "混插");

        let partial = hardware(vec![
            memory_module("A1", "1", "P1", Some(4800), Some("DDR5"), Some("DIMM")),
            memory_module("A2", "2", "P1", None, Some("DDR5"), Some("DIMM")),
        ]);
        assert_eq!(speed_summary(&partial).0, "部分未提供");
        assert!(speed_summary(&partial).1.contains("1 根缺字段"));
    }

    #[test]
    fn kind_summary_marks_partial_instead_of_single_kind() {
        let same = hardware(vec![
            memory_module("A1", "1", "P1", Some(4800), Some("DDR5"), Some("DIMM")),
            memory_module("A2", "2", "P1", Some(4800), Some("DDR5"), Some("DIMM")),
        ]);
        assert_eq!(kind_summary(&same), ("DDR5".into(), "DIMM".into()));

        let partial = hardware(vec![
            memory_module("A1", "1", "P1", Some(4800), Some("DDR5"), Some("DIMM")),
            memory_module("A2", "2", "P1", Some(4800), None, Some("DIMM")),
        ]);
        assert_eq!(kind_summary(&partial).0, "部分未提供");

        let mixed = hardware(vec![
            memory_module("A1", "1", "P1", Some(4800), Some("DDR5"), Some("DIMM")),
            memory_module("A2", "2", "P1", Some(4800), Some("DDR4"), Some("DIMM")),
        ]);
        assert_eq!(kind_summary(&mixed).0, "混合规格");
    }

    #[test]
    fn matching_slot_requires_a_unique_identifier() {
        let modules = vec![
            memory_module(
                "DIMM_A1",
                " ab c ",
                "P1",
                Some(4800),
                Some("DDR5"),
                Some("DIMM"),
            ),
            memory_module(
                "DIMM_B1",
                "ZZZ",
                "P2",
                Some(4800),
                Some("DDR5"),
                Some("DIMM"),
            ),
        ];
        assert_eq!(
            matching_slot(&spd_module("ABC", "P1"), &modules),
            Some("DIMM_A1")
        );
        assert_eq!(
            matching_slot(&spd_module("", "P2"), &modules),
            Some("DIMM_B1")
        );
        assert_eq!(matching_slot(&spd_module("MISMATCH", "P2"), &modules), None);

        let duplicates = vec![
            memory_module("DIMM_A1", "ABC", "", Some(4800), Some("DDR5"), Some("DIMM")),
            memory_module("DIMM_B1", "abc", "", Some(4800), Some("DDR5"), Some("DIMM")),
        ];
        assert_eq!(matching_slot(&spd_module("ABC", ""), &duplicates), None);
    }
}
