#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod memory;
mod memory_page;
mod spd;
mod sys;

use std::time::Duration;

use eframe::egui::{self, Color32};
use eframe::{App, Frame};

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("内存透镜 Memory Lens")
            .with_inner_size([1060.0, 780.0])
            .with_min_inner_size([960.0, 720.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "MemoryLens",
        options,
        Box::new(|cc| {
            load_cjk_font(&cc.egui_ctx);
            apply_dark_style(&cc.egui_ctx);
            Ok(Box::new(app::MemoryLensApp::new()))
        }),
    )
}

/// 运行时加载系统中文字体（微软雅黑优先），不打包进 exe。
fn load_cjk_font(ctx: &egui::Context) {
    let candidates = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msjh.ttc",
        r"C:\Windows\Fonts\Deng.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
    ];
    for path in candidates {
        let Ok(data) = std::fs::read(path) else { continue };
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "cjk".into(),
            std::sync::Arc::new(egui::FontData::from_owned(data)),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "cjk".into());
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .push("cjk".into());
        ctx.set_fonts(fonts);
        return;
    }
}

fn apply_dark_style(ctx: &egui::Context) {
    use egui::{FontFamily, FontId, TextStyle};
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = Color32::from_rgb(0x10, 0x14, 0x1A);
    style.visuals.extreme_bg_color = Color32::from_rgb(0x0B, 0x0E, 0x12);
    // 弹窗 / tooltip 背景：深色（egui 的 popup/tooltip 与 window 共用 window_fill）
    style.visuals.window_fill = Color32::from_rgb(0x1E, 0x24, 0x30);
    style.visuals.window_stroke =
        egui::Stroke::new(1.0_f32, Color32::from_rgb(0x3A, 0x44, 0x55));
    // 注意：不设置 override_text_color，让控件用 Visuals::dark() 自带的亮色文字，
    // 避免强制覆盖颜色与自定义深色背景冲突。

    // 全局禁止 label 变成可编辑文本（消除 I-beam 文本插入光标，让右键正常触发）
    style.interaction.selectable_labels = false;

    // 全局调大字号
    style.text_styles = [
        (TextStyle::Small, FontId::new(12.5, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(14.5, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(14.5, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(14.0, FontFamily::Monospace)),
        (TextStyle::Heading, FontId::new(22.0, FontFamily::Proportional)),
    ]
    .into();
    ctx.set_style(style);
}

impl App for app::MemoryLensApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut Frame) {
        self.tick(ctx);
        // 只在接近下次采样时重绘，平时不占用 CPU/GPU
        let until_next = self.next_refresh_in();
        ctx.request_repaint_after(until_next.min(Duration::from_millis(300)));

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(0x10, 0x14, 0x1A))
                    .inner_margin(egui::Margin::symmetric(16, 12)),
            )
            .show(ctx, |ui| self.ui(ui));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shutdown();
    }
}
