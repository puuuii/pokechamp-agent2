mod crop_input;
mod phase_button;

pub use crop_input::CropInputController;
pub use phase_button::PhaseButton;

use super::buffer::PixelBuffer;
use super::jp_text::JpTextRenderer;
use super::{CropArea, PixelCropArea};
use crate::hardware::FrameBuffer;
use crate::inference::PhaseStatus;
use anyhow::Result;
use crossbeam_channel::Receiver;
use minifb::{Window, WindowOptions};
use serde::Deserialize;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;
use tracing::warn;

// --- UI余白パネル関連 -----------------------------------------------
/// 表示ウィンドウのレイアウトパラメータ。
/// 通常は TOML ファイル(config/display.toml)から読み込む。
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct DisplayPanelConfig {
    /// 左パネル幅(ピクセル)。
    pub left_panel_width: usize,
    /// 右パネル幅(ピクセル)。
    pub right_panel_width: usize,
    /// 下パネル高さ(ピクセル)。
    pub bottom_panel_height: usize,
    /// クロップ調整(矢印キー1押し)の相対ステップ。
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

const PANEL_BACKGROUND_COLOR: u32 = 0x0020_2020;
const PANEL_TEXT_COLOR: u32 = 0x00FF_FFFF;

const PHASE_TEXT_X: usize = 10;
const PHASE_TEXT_Y: usize = 40;
const PHASE_TEXT_PIXEL_HEIGHT: f32 = 20.0;

/// 静的パネルテキスト(プレースホルダー等)のピクセル高さ。
const STATIC_TEXT_PIXEL_HEIGHT: f32 = 14.0;

/// 静的パネルのプレースホルダーテキスト。
const PANEL_PLACEHOLDER_TEXT: &str = "TEST TEXT";

pub struct DisplayWindow {
    window: Window,
    panel: DisplayPanelConfig,
    video_width: usize,
    video_height: usize,
    total_width: usize,
    total_height: usize,
    current_frame: FrameBuffer,
    render_buffer: Vec<u32>,
    crop_input: CropInputController,
    phase_button: PhaseButton,
    jp_text_renderer: Option<JpTextRenderer>,
    last_phase_text_size: (usize, usize),
    last_phase_text: String,
}

impl DisplayWindow {
    pub fn open_uncapped(
        title: &str,
        video_resolution: (usize, usize),
        panel: DisplayPanelConfig,
        manual_phase_advance: &Arc<AtomicBool>,
    ) -> Result<Self> {
        let (video_width, video_height) = video_resolution;

        let total_width = panel.left_panel_width + video_width + panel.right_panel_width;
        let total_height = video_height + panel.bottom_panel_height;

        let mut window = Window::new(title, total_width, total_height, WindowOptions::default())?;

        const UNCAPPED_FPS: usize = 0;
        window.set_target_fps(UNCAPPED_FPS);

        let jp_text_renderer = match JpTextRenderer::load_system_font() {
            Ok(renderer) => Some(renderer),
            Err(e) => {
                warn!("日本語フォント読み込み失敗。フェーズ表示は無効化: {e}");
                None
            }
        };

        let mut display = Self {
            window,
            panel,
            video_width,
            video_height,
            total_width,
            total_height,
            current_frame: Arc::new(vec![0u32; video_width * video_height]),
            render_buffer: vec![0u32; total_width * total_height],
            crop_input: CropInputController::new(panel.crop_adjust_step),
            phase_button: PhaseButton::new(Arc::clone(manual_phase_advance)),
            jp_text_renderer,
            last_phase_text_size: (0, 0),
            last_phase_text: String::new(),
        };

        display.init_static_ui();

        Ok(display)
    }

    fn init_static_ui(&mut self) {
        for pixel in self.render_buffer.iter_mut() {
            *pixel = PANEL_BACKGROUND_COLOR;
        }

        let Some(renderer) = &self.jp_text_renderer else {
            return;
        };

        let mut buffer =
            PixelBuffer::new(&mut self.render_buffer, self.total_width, self.total_height);

        // TODO: 右パネル・下パネルの実際の表示内容を決定し、このプレースホルダーを置き換える。
        let right_panel_x = self.panel.left_panel_width + self.video_width + 10;
        renderer.draw(
            &mut buffer,
            right_panel_x,
            10,
            PANEL_PLACEHOLDER_TEXT,
            PANEL_TEXT_COLOR,
            STATIC_TEXT_PIXEL_HEIGHT,
        );

        let bottom_panel_y = self.video_height + 10;
        renderer.draw(
            &mut buffer,
            10,
            bottom_panel_y,
            PANEL_PLACEHOLDER_TEXT,
            PANEL_TEXT_COLOR,
            STATIC_TEXT_PIXEL_HEIGHT,
        );
    }

    pub fn is_open(&self) -> bool {
        self.window.is_open()
    }

    fn drain_queue_and_get_latest_frame(&mut self, rx_frame: &Receiver<FrameBuffer>) -> bool {
        let mut received_new_frame = false;
        while let Ok(latest_frame) = rx_frame.try_recv() {
            self.current_frame = latest_frame;
            received_new_frame = true;
        }
        received_new_frame
    }

    fn blit_video_frame(&mut self) {
        for row in 0..self.video_height {
            let src_start = row * self.video_width;
            let src_end = src_start + self.video_width;

            let dst_start = row * self.total_width + self.panel.left_panel_width;
            let dst_end = dst_start + self.video_width;

            self.render_buffer[dst_start..dst_end]
                .copy_from_slice(&self.current_frame[src_start..src_end]);
        }
    }

    fn update_phase_panel(&mut self, phase_status: &PhaseStatus) {
        let current_text = phase_status.read().unwrap().clone();

        if current_text == self.last_phase_text {
            return;
        }

        let Some(renderer) = &self.jp_text_renderer else {
            self.last_phase_text = current_text;
            return;
        };

        let (last_w, last_h) = self.last_phase_text_size;
        let mut buffer =
            PixelBuffer::new(&mut self.render_buffer, self.total_width, self.total_height);
        buffer.clear_rect(
            PHASE_TEXT_X,
            PHASE_TEXT_Y,
            last_w,
            last_h,
            PANEL_BACKGROUND_COLOR,
        );
        self.phase_button.clear(&mut buffer, PANEL_BACKGROUND_COLOR);

        let text_size = if current_text.is_empty() {
            (0, 0)
        } else {
            renderer.draw(
                &mut buffer,
                PHASE_TEXT_X,
                PHASE_TEXT_Y,
                &current_text,
                PANEL_TEXT_COLOR,
                PHASE_TEXT_PIXEL_HEIGHT,
            )
        };
        self.last_phase_text_size = text_size;

        // テキストの右側に▶ボタンを描く(表示幅に合わせて配置)。
        self.phase_button.update_for_text(&mut buffer, PHASE_TEXT_X, PHASE_TEXT_Y, text_size);

        self.last_phase_text = current_text;
    }

    pub fn render_latest(
        &mut self,
        rx_frame: &Receiver<FrameBuffer>,
        crop_area: &Arc<RwLock<CropArea>>,
        phase_status: &PhaseStatus,
    ) -> Result<()> {
        self.crop_input.handle(&self.window, crop_area);
        self.phase_button.handle_click(&self.window);
        self.update_phase_panel(phase_status);

        let has_new_frame = self.drain_queue_and_get_latest_frame(rx_frame);

        if has_new_frame {
            self.blit_video_frame();

            if cfg!(debug_assertions) && self.crop_input.show_debug_frame() {
                let crop = crop_area
                    .read()
                    .unwrap()
                    .to_pixels(self.video_width, self.video_height);
                let mut buffer =
                    PixelBuffer::new(&mut self.render_buffer, self.total_width, self.total_height);
                draw_red_box(&mut buffer, self.panel.left_panel_width, 0, &crop);
            }

            self.window.update_with_buffer(
                &self.render_buffer,
                self.total_width,
                self.total_height,
            )?;
        } else {
            self.window.update();
            thread::sleep(Duration::from_millis(1));
        }

        Ok(())
    }
}

fn draw_red_box(buffer: &mut PixelBuffer, offset_x: usize, offset_y: usize, crop: &PixelCropArea) {
    const RED_COLOR: u32 = 0x00FF_0000;
    let thickness = 3;
    let buf_w = buffer.width;
    let buf_h = buffer.height;
    let pixels = buffer.pixels_mut();

    let x1 = offset_x + crop.x;
    let y1 = offset_y + crop.y;
    let x2 = (offset_x + crop.x + crop.width).min(buf_w);
    let y2 = (offset_y + crop.y + crop.height).min(buf_h);

    for t in 0..thickness {
        for x in x1..x2 {
            if y1 + t < buf_h {
                pixels[(y1 + t) * buf_w + x] = RED_COLOR;
            }
            if y2.saturating_sub(1 + t) < buf_h {
                pixels[(y2.saturating_sub(1 + t)) * buf_w + x] = RED_COLOR;
            }
        }
        for y in y1..y2 {
            if x1 + t < buf_w {
                pixels[y * buf_w + (x1 + t)] = RED_COLOR;
            }
            if x2.saturating_sub(1 + t) < buf_w {
                pixels[y * buf_w + x2.saturating_sub(1 + t)] = RED_COLOR;
            }
        }
    }
}
