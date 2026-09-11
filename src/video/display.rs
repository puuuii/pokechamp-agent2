use super::pixel::unpack_rgb;
use super::{CropArea, PixelCropArea};
use crate::hardware::FrameBuffer;
use crate::inference::{PartyMatchStatus, PartySlots, PhaseStatus};
use anyhow::{Context, Result};
use crossbeam_channel::Receiver;
use egui::{Color32, ColorImage, Key, TextureHandle, TextureOptions, Vec2};
use rayon::prelude::*;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct DisplayPanelConfig {
    pub left_panel_width: usize,
    pub right_panel_width: usize,
    pub bottom_panel_height: usize,
    pub crop_adjust_step: f32,
}

impl Default for DisplayPanelConfig {
    fn default() -> Self {
        Self {
            left_panel_width: 200,
            right_panel_width: 200,
            bottom_panel_height: 100,
            crop_adjust_step: 0.0025,
        }
    }
}

const CROP_KEY_REPEAT_INTERVAL: Duration = Duration::from_millis(50);

const CANDIDATE_FONT_PATHS: &[&str] = &[
    r"C:\Windows\Fonts\YuGothR.ttc",
    r"C:\Windows\Fonts\meiryo.ttc",
    r"C:\Windows\Fonts\msgothic.ttc",
];

fn install_jp_font(ctx: &egui::Context) {
    for path in CANDIDATE_FONT_PATHS {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "jp_font".to_owned(),
                egui::FontData::from_owned(bytes).into(),
            );
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "jp_font".to_owned());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .insert(0, "jp_font".to_owned());
            ctx.set_fonts(fonts);
            return;
        }
    }
    warn!(
        "日本語フォントが見つかりませんでした。候補パス: {:?}",
        CANDIDATE_FONT_PATHS
    );
}

fn usage_script_python_path() -> PathBuf {
    PathBuf::from("scripts")
        .join(".venv")
        .join("Scripts")
        .join("python.exe")
}

fn usage_script_path() -> PathBuf {
    PathBuf::from("scripts").join("dl_usage.py")
}

fn run_usage_update_script() -> Result<()> {
    let python = usage_script_python_path();
    let script = usage_script_path();

    let status = std::process::Command::new(&python)
        .arg(&script)
        .status()
        .with_context(|| format!("Pythonの起動に失敗しました: {}", python.display()))?;

    anyhow::ensure!(
        status.success(),
        "dl_usage.py が異常終了しました (exit status: {status})"
    );

    Ok(())
}

pub struct DisplayApp {
    panel: DisplayPanelConfig,
    video_width: usize,
    video_height: usize,
    rx_display: Receiver<FrameBuffer>,
    crop_area: Arc<RwLock<CropArea>>,
    phase_status: PhaseStatus,
    manual_phase_advance: Arc<AtomicBool>,
    party_match_status: PartyMatchStatus,
    party_slots: PartySlots,
    usage_update_running: Arc<AtomicBool>,
    texture: Option<TextureHandle>,
    show_debug_frame: bool,
    last_crop_key_time: Instant,
}

impl DisplayApp {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        video_resolution: (usize, usize),
        panel: DisplayPanelConfig,
        rx_display: Receiver<FrameBuffer>,
        crop_area: Arc<RwLock<CropArea>>,
        phase_status: PhaseStatus,
        manual_phase_advance: Arc<AtomicBool>,
        party_match_status: PartyMatchStatus,
        party_slots: PartySlots,
    ) -> Self {
        install_jp_font(&cc.egui_ctx);
        let (video_width, video_height) = video_resolution;

        Self {
            panel,
            video_width,
            video_height,
            rx_display,
            crop_area,
            phase_status,
            manual_phase_advance,
            party_match_status,
            party_slots,
            usage_update_running: Arc::new(AtomicBool::new(false)),
            texture: None,
            show_debug_frame: cfg!(debug_assertions),
            last_crop_key_time: Instant::now(),
        }
    }

    fn drain_latest_frame(&mut self) -> Option<FrameBuffer> {
        let mut latest = None;
        while let Ok(frame) = self.rx_display.try_recv() {
            latest = Some(frame);
        }
        latest
    }

    fn frame_to_color_image(frame: &FrameBuffer, width: usize, height: usize) -> ColorImage {
        let pixels: Vec<Color32> = frame
            .par_iter()
            .map(|&packed| {
                let (r, g, b) = unpack_rgb(packed);
                Color32::from_rgb(r, g, b)
            })
            .collect();

        ColorImage {
            size: [width, height],
            source_size: Vec2::new(width as f32, height as f32),
            pixels,
        }
    }

    fn handle_crop_keys(&mut self, ctx: &egui::Context) {
        let (shift, left, right, up, down, toggle_debug) = ctx.input(|i| {
            (
                i.modifiers.shift,
                i.key_down(Key::ArrowLeft),
                i.key_down(Key::ArrowRight),
                i.key_down(Key::ArrowUp),
                i.key_down(Key::ArrowDown),
                cfg!(debug_assertions) && i.key_pressed(Key::D),
            )
        });

        if toggle_debug {
            self.show_debug_frame = !self.show_debug_frame;
            tracing::debug!("デバッグフレーム表示: {}", self.show_debug_frame);
        }

        if !(left || right || up || down) {
            return;
        }
        if self.last_crop_key_time.elapsed() < CROP_KEY_REPEAT_INTERVAL {
            return;
        }

        let step = self.panel.crop_adjust_step;
        let mut crop_guard = self.crop_area.write().unwrap();
        let crop: &mut CropArea = &mut crop_guard;
        let (horizontal, vertical) = if shift {
            (&mut crop.width, &mut crop.height)
        } else {
            (&mut crop.x, &mut crop.y)
        };
        if left {
            *horizontal -= step;
        }
        if right {
            *horizontal += step;
        }
        if up {
            *vertical -= step;
        }
        if down {
            *vertical += step;
        }
        crop.clamp();
        tracing::debug!(
            "クロップ枠 x: {:.4}, y: {:.4}, w: {:.4}, h: {:.4}",
            crop.x,
            crop.y,
            crop.width,
            crop.height
        );
        drop(crop_guard);
        self.last_crop_key_time = Instant::now();
    }

    fn trigger_usage_update(&self) {
        if self.usage_update_running.swap(true, Ordering::Relaxed) {
            warn!("使用率更新は既に実行中のためスキップしました");
            return;
        }

        let running = Arc::clone(&self.usage_update_running);
        thread::spawn(move || {
            let result = run_usage_update_script();
            running.store(false, Ordering::Relaxed);
            match result {
                Ok(()) => println!("使用率更新が完了しました"),
                Err(e) => eprintln!("使用率更新の実行に失敗しました: {e}"),
            }
        });
    }
}

impl eframe::App for DisplayApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint();

        self.handle_crop_keys(ctx);

        if let Some(frame_buf) = self.drain_latest_frame() {
            let image = Self::frame_to_color_image(&frame_buf, self.video_width, self.video_height);
            match &mut self.texture {
                Some(tex) => tex.set(image, TextureOptions::LINEAR),
                None => {
                    self.texture =
                        Some(ctx.load_texture("video_frame", image, TextureOptions::LINEAR));
                }
            }
        }

        egui::SidePanel::left("left_panel")
            .exact_width(self.panel.left_panel_width as f32)
            .resizable(false)
            .show(ctx, |ui| {
                let matches = self.party_match_status.read().unwrap();

                ui.label("味方");
                for (i, name) in matches.iter().take(6).enumerate() {
                    let display = if name.is_empty() { "-" } else { name.as_str() };
                    ui.monospace(format!("{}: {}", i + 1, display));
                }

                ui.separator();

                ui.label("相手");
                for (i, name) in matches.iter().skip(6).take(6).enumerate() {
                    let display = if name.is_empty() { "-" } else { name.as_str() };
                    ui.monospace(format!("{}: {}", i + 1, display));
                }
            });

        egui::SidePanel::right("right_panel")
            .exact_width(self.panel.right_panel_width as f32)
            .resizable(false)
            .show(ctx, |ui| {
                let phase_text = self.phase_status.read().unwrap().clone();
                ui.horizontal(|ui| {
                    ui.label(&phase_text);
                    if !phase_text.is_empty() && ui.button("▶").clicked() {
                        self.manual_phase_advance.store(true, Ordering::Relaxed);
                        info!("手動フェーズ進行リクエスト");
                    }
                });

                ui.add_space(8.0);
                if ui.button("使用率更新").clicked() {
                    self.trigger_usage_update();
                }

                ui.separator();
                let crop = *self.crop_area.read().unwrap();
                ui.label("クロップ枠(相対座標)");
                ui.monospace(format!("x: {:.4}", crop.x));
                ui.monospace(format!("y: {:.4}", crop.y));
                ui.monospace(format!("w: {:.4}", crop.width));
                ui.monospace(format!("h: {:.4}", crop.height));
            });

        egui::TopBottomPanel::bottom("bottom_panel")
            .exact_height(self.panel.bottom_panel_height as f32)
            .show(ctx, |ui| {
                ui.label("TEST TEXT");
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            let Some(texture) = &self.texture else {
                ui.label("映像待機中...");
                return;
            };

            let size = Vec2::new(self.video_width as f32, self.video_height as f32);
            let response = ui.add(egui::Image::new(texture).fit_to_exact_size(size));

            if cfg!(debug_assertions) && self.show_debug_frame {
                let crop = self
                    .crop_area
                    .read()
                    .unwrap()
                    .to_pixels(self.video_width, self.video_height);
                draw_crop_overlay(ui, response.rect, &crop);
                draw_party_slot_overlays(
                    ui,
                    response.rect,
                    &self.party_slots,
                    self.video_width,
                    self.video_height,
                );
            }
        });
    }
}

fn draw_crop_overlay(ui: &egui::Ui, image_rect: egui::Rect, crop: &PixelCropArea) {
    let painter = ui.painter_at(image_rect);
    let top_left = image_rect.min + Vec2::new(crop.x as f32, crop.y as f32);
    let rect =
        egui::Rect::from_min_size(top_left, Vec2::new(crop.width as f32, crop.height as f32));
    painter.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(3.0, Color32::RED),
        egui::StrokeKind::Middle,
    );
}

/// 選出パーティ12箇所のクロップ範囲を黄枠でオーバーレイ表示する(デバッグ用)。
fn draw_party_slot_overlays(
    ui: &egui::Ui,
    image_rect: egui::Rect,
    party_slots: &PartySlots,
    video_width: usize,
    video_height: usize,
) {
    let painter = ui.painter_at(image_rect);
    for crop in party_slots.all_crops() {
        let pixel_crop = crop.to_pixels(video_width, video_height);
        let top_left = image_rect.min + Vec2::new(pixel_crop.x as f32, pixel_crop.y as f32);
        let rect = egui::Rect::from_min_size(
            top_left,
            Vec2::new(pixel_crop.width as f32, pixel_crop.height as f32),
        );
        painter.rect_stroke(
            rect,
            0.0,
            egui::Stroke::new(2.0, Color32::YELLOW),
            egui::StrokeKind::Middle,
        );
    }
}
