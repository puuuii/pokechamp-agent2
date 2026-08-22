use super::buffer::PixelBuffer;
use super::jp_text::JpTextRenderer;
use super::text::draw_text;
use super::{CropArea, PixelCropArea};
use crate::hardware::FrameBuffer;
use crate::inference::PhaseStatus;
use anyhow::Result;
use crossbeam_channel::Receiver;
use minifb::{Key, MouseButton, MouseMode, Window, WindowOptions};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

const STEP: f32 = 0.0025; // 移動・リサイズの相対ステップ

// --- UI余白パネル関連 -----------------------------------------------
const LEFT_PANEL_WIDTH: usize = 200;
const RIGHT_PANEL_WIDTH: usize = 200;
const BOTTOM_PANEL_HEIGHT: usize = 100;

const PANEL_BACKGROUND_COLOR: u32 = 0x0020_2020;
const PANEL_TEXT_COLOR: u32 = 0x00FF_FFFF;

const PHASE_TEXT_X: usize = 10;
const PHASE_TEXT_Y: usize = 40;
const PHASE_TEXT_PIXEL_HEIGHT: f32 = 20.0;

// フェーズテキスト右横の▶ボタン(手動で次のフェーズへ進行)関連
const PHASE_BUTTON_GAP: usize = 8;
const PHASE_BUTTON_WIDTH: usize = 12;
const PHASE_BUTTON_HEIGHT: usize = 16;
const PHASE_BUTTON_COLOR: u32 = 0x00C8_00FF;

pub struct DisplayWindow {
    window: Window,
    video_width: usize,
    video_height: usize,
    total_width: usize,
    total_height: usize,
    current_frame: FrameBuffer,
    render_buffer: Vec<u32>,
    last_input_time: Instant,
    show_debug_frame: bool,
    jp_text_renderer: Option<JpTextRenderer>,
    last_phase_text_size: (usize, usize),
    last_phase_text: String,
    phase_button_rect: Option<(usize, usize, usize, usize)>,
    phase_button_down: bool,
    manual_phase_advance: Arc<AtomicBool>,
}

impl DisplayWindow {
    pub fn open_uncapped(
        title: &str,
        video_resolution: (usize, usize),
        manual_phase_advance: &Arc<AtomicBool>,
    ) -> Result<Self> {
        let (video_width, video_height) = video_resolution;

        let total_width = LEFT_PANEL_WIDTH + video_width + RIGHT_PANEL_WIDTH;
        let total_height = video_height + BOTTOM_PANEL_HEIGHT;

        let mut window = Window::new(title, total_width, total_height, WindowOptions::default())?;

        const UNCAPPED_FPS: usize = 0;
        window.set_target_fps(UNCAPPED_FPS);

        let jp_text_renderer = match JpTextRenderer::load_system_font() {
            Ok(renderer) => Some(renderer),
            Err(e) => {
                eprintln!("[UI] 日本語フォント読み込み失敗。フェーズ表示は無効化: {e}");
                None
            }
        };

        let mut display = Self {
            window,
            video_width,
            video_height,
            total_width,
            total_height,
            current_frame: Arc::new(vec![0u32; video_width * video_height]),
            render_buffer: vec![0u32; total_width * total_height],
            last_input_time: Instant::now(),
            show_debug_frame: cfg!(debug_assertions),
            jp_text_renderer,
            last_phase_text_size: (0, 0),
            last_phase_text: String::new(),
            phase_button_rect: None,
            phase_button_down: false,
            manual_phase_advance: Arc::clone(manual_phase_advance),
        };

        display.init_static_ui();

        Ok(display)
    }

    fn init_static_ui(&mut self) {
        for pixel in self.render_buffer.iter_mut() {
            *pixel = PANEL_BACKGROUND_COLOR;
        }

        let mut buffer =
            PixelBuffer::new(&mut self.render_buffer, self.total_width, self.total_height);

        let right_panel_x = LEFT_PANEL_WIDTH + self.video_width + 10;
        draw_text(
            &mut buffer,
            right_panel_x,
            10,
            "TEST TEXT",
            PANEL_TEXT_COLOR,
            2,
            2,
        );

        let bottom_panel_y = self.video_height + 10;
        draw_text(
            &mut buffer,
            10,
            bottom_panel_y,
            "TEST TEXT",
            PANEL_TEXT_COLOR,
            2,
            2,
        );
    }

    pub fn is_open(&self) -> bool {
        self.window.is_open()
    }

    fn handle_input(&mut self, crop_area: &Arc<RwLock<CropArea>>) {
        if cfg!(debug_assertions) && self.window.is_key_pressed(Key::D, minifb::KeyRepeat::No) {
            self.show_debug_frame = !self.show_debug_frame;
            println!("[Debug Frame] Visibility: {}", self.show_debug_frame);
            return;
        }

        if self.last_input_time.elapsed() < Duration::from_millis(50) {
            return;
        }

        let mut crop = crop_area.write().unwrap();
        let shift =
            self.window.is_key_down(Key::LeftShift) || self.window.is_key_down(Key::RightShift);

        let mut changed = false;

        if shift {
            if self.window.is_key_down(Key::Left) {
                crop.width -= STEP;
                changed = true;
            }
            if self.window.is_key_down(Key::Right) {
                crop.width += STEP;
                changed = true;
            }
            if self.window.is_key_down(Key::Up) {
                crop.height -= STEP;
                changed = true;
            }
            if self.window.is_key_down(Key::Down) {
                crop.height += STEP;
                changed = true;
            }
        } else {
            if self.window.is_key_down(Key::Left) {
                crop.x -= STEP;
                changed = true;
            }
            if self.window.is_key_down(Key::Right) {
                crop.x += STEP;
                changed = true;
            }
            if self.window.is_key_down(Key::Up) {
                crop.y -= STEP;
                changed = true;
            }
            if self.window.is_key_down(Key::Down) {
                crop.y += STEP;
                changed = true;
            }
        }

        if changed {
            crop.clamp();
            self.last_input_time = Instant::now();
            println!(
                "[Crop Box] x: {:.4}, y: {:.4}, w: {:.4}, h: {:.4}",
                crop.x, crop.y, crop.width, crop.height
            );
        }
    }

    /// フェーズテキスト右横の▶ボタンのクリックを検出する。
    /// クリック成立時に手動フェーズ進行フラグを立てる(押下エッジ1回分)。
    fn handle_phase_button(&mut self) {
        let pressed = self.window.get_mouse_down(MouseButton::Left);
        if !pressed {
            self.phase_button_down = false;
            return;
        }

        if self.phase_button_down {
            return;
        }

        let Some((button_x, button_y, button_w, button_h)) = self.phase_button_rect else {
            return;
        };
        let Some((mouse_x, mouse_y)) = self.window.get_mouse_pos(MouseMode::Discard) else {
            return;
        };
        let (mouse_x, mouse_y) = (mouse_x as usize, mouse_y as usize);

        if button_x <= mouse_x
            && mouse_x < button_x + button_w
            && button_y <= mouse_y
            && mouse_y < button_y + button_h
        {
            self.phase_button_down = true;
            self.manual_phase_advance.store(true, Ordering::Relaxed);
            println!("[Phase] 手動フェーズ進行リクエスト");
        }
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

            let dst_start = row * self.total_width + LEFT_PANEL_WIDTH;
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
        clear_rect(
            &mut buffer,
            PHASE_TEXT_X,
            PHASE_TEXT_Y,
            last_w,
            last_h,
            PANEL_BACKGROUND_COLOR,
        );
        if let Some((button_x, button_y, button_w, button_h)) = self.phase_button_rect {
            clear_rect(
                &mut buffer,
                button_x,
                button_y,
                button_w,
                button_h,
                PANEL_BACKGROUND_COLOR,
            );
        }

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
        self.phase_button_rect = if text_size.0 == 0 {
            None
        } else {
            let button_x = PHASE_TEXT_X + text_size.0 + PHASE_BUTTON_GAP;
            let button_y = PHASE_TEXT_Y + (text_size.1.saturating_sub(PHASE_BUTTON_HEIGHT)) / 2;
            draw_phase_button(&mut buffer, button_x, button_y);
            Some((button_x, button_y, PHASE_BUTTON_WIDTH, PHASE_BUTTON_HEIGHT))
        };

        self.last_phase_text = current_text;
    }

    pub fn render_latest(
        &mut self,
        rx_frame: &Receiver<FrameBuffer>,
        crop_area: &Arc<RwLock<CropArea>>,
        phase_status: &PhaseStatus,
    ) -> Result<()> {
        self.handle_input(crop_area);
        self.handle_phase_button();
        self.update_phase_panel(phase_status);

        let has_new_frame = self.drain_queue_and_get_latest_frame(rx_frame);

        if has_new_frame {
            self.blit_video_frame();

            if cfg!(debug_assertions) && self.show_debug_frame {
                let crop = crop_area
                    .read()
                    .unwrap()
                    .to_pixels(self.video_width, self.video_height);
                let mut buffer =
                    PixelBuffer::new(&mut self.render_buffer, self.total_width, self.total_height);
                draw_red_box(&mut buffer, LEFT_PANEL_WIDTH, 0, &crop);
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

fn clear_rect(buffer: &mut PixelBuffer, x: usize, y: usize, w: usize, h: usize, color: u32) {
    let buf_w = buffer.width;
    let buf_h = buffer.height;
    let pixels = buffer.pixels_mut();
    for row in 0..h {
        let py = y + row;
        if py >= buf_h {
            break;
        }
        for col in 0..w {
            let px = x + col;
            if px >= buf_w {
                break;
            }
            pixels[py * buf_w + px] = color;
        }
    }
}

/// (x, y)を左上とする右向き▶三角(PHASE_BUTTON_WIDTH × PHASE_BUTTON_HEIGHT)を描く。
fn draw_phase_button(buffer: &mut PixelBuffer, x: usize, y: usize) {
    let buf_w = buffer.width;
    let buf_h = buffer.height;
    let pixels = buffer.pixels_mut();
    let center = PHASE_BUTTON_HEIGHT as f32 / 2.0;

    for dy in 0..PHASE_BUTTON_HEIGHT {
        let dist = (dy as f32 - center).abs();
        let span = (PHASE_BUTTON_WIDTH as f32 * (1.0 - dist / center)) as usize;
        for dx in 0..span {
            let px = x + dx;
            let py = y + dy;
            if px < buf_w && py < buf_h {
                pixels[py * buf_w + px] = PHASE_BUTTON_COLOR;
            }
        }
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
