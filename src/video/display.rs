use super::text::draw_text;
use super::{CropArea, PixelCropArea};
use crate::hardware::FrameBuffer;
use anyhow::Result;
use crossbeam_channel::Receiver;
use minifb::{Key, Window, WindowOptions};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

const STEP: f32 = 0.0025; // 移動・リサイズの相対ステップ

// --- UI余白パネル関連 -----------------------------------------------
// 今後ここに項目一覧やステータス表示などのUIコンポーネントを足していく想定。
// サイズは仮値。
const LEFT_PANEL_WIDTH: usize = 200;
const RIGHT_PANEL_WIDTH: usize = 200;
const BOTTOM_PANEL_HEIGHT: usize = 100;

const PANEL_BACKGROUND_COLOR: u32 = 0x0020_2020; // ダークグレー
const PANEL_TEXT_COLOR: u32 = 0x00FF_FFFF; // 白

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
}

impl DisplayWindow {
    pub fn open_uncapped(title: &str, video_resolution: (usize, usize)) -> Result<Self> {
        let (video_width, video_height) = video_resolution;

        let total_width = LEFT_PANEL_WIDTH + video_width + RIGHT_PANEL_WIDTH;
        let total_height = video_height + BOTTOM_PANEL_HEIGHT;

        let mut window = Window::new(title, total_width, total_height, WindowOptions::default())?;

        const UNCAPPED_FPS: usize = 0;
        window.set_target_fps(UNCAPPED_FPS);

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
        };

        display.init_static_ui();

        Ok(display)
    }

    /// 映像描画領域以外(左右・下の余白パネル)を初期化時に一度だけ描画する。
    /// 映像フレームは毎フレーム中央部分だけ上書きされるので、ここで描いた
    /// 背景色や文字はそのまま残り続ける。
    fn init_static_ui(&mut self) {
        // 背景を塗る(映像領域は後で毎フレーム上書きされるので気にしなくてよい)
        for pixel in self.render_buffer.iter_mut() {
            *pixel = PANEL_BACKGROUND_COLOR;
        }

        // 右パネルに動作確認用テキスト
        let right_panel_x = LEFT_PANEL_WIDTH + self.video_width + 10;
        draw_text(
            &mut self.render_buffer,
            self.total_width,
            self.total_height,
            right_panel_x,
            10,
            "TEST TEXT",
            PANEL_TEXT_COLOR,
            2,
            2,
        );

        // 下パネルに動作確認用テキスト
        let bottom_panel_y = self.video_height + 10;
        draw_text(
            &mut self.render_buffer,
            self.total_width,
            self.total_height,
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

    fn drain_queue_and_get_latest_frame(&mut self, rx_frame: &Receiver<FrameBuffer>) -> bool {
        let mut received_new_frame = false;
        while let Ok(latest_frame) = rx_frame.try_recv() {
            self.current_frame = latest_frame;
            received_new_frame = true;
        }
        received_new_frame
    }

    /// 映像フレーム(video_width x video_height)を render_buffer 内の
    /// 映像描画領域(左余白ぶんオフセットした位置)に行単位でコピーする。
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

    pub fn render_latest(
        &mut self,
        rx_frame: &Receiver<FrameBuffer>,
        crop_area: &Arc<RwLock<CropArea>>,
    ) -> Result<()> {
        self.handle_input(crop_area);

        let has_new_frame = self.drain_queue_and_get_latest_frame(rx_frame);

        if has_new_frame {
            self.blit_video_frame();

            if cfg!(debug_assertions) && self.show_debug_frame {
                let crop = crop_area
                    .read()
                    .unwrap()
                    .to_pixels(self.video_width, self.video_height);
                draw_red_box(
                    &mut self.render_buffer,
                    self.total_width,
                    self.total_height,
                    LEFT_PANEL_WIDTH,
                    0,
                    &crop,
                );
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

/// crop は映像座標系(オフセットなし)のピクセル範囲。
/// offset_x / offset_y で render_buffer 内の映像描画開始位置ぶんずらして描画する。
fn draw_red_box(
    buffer: &mut [u32],
    buf_w: usize,
    buf_h: usize,
    offset_x: usize,
    offset_y: usize,
    crop: &PixelCropArea,
) {
    const RED_COLOR: u32 = 0x00FF_0000;
    let thickness = 3;

    let x1 = offset_x + crop.x;
    let y1 = offset_y + crop.y;
    let x2 = (offset_x + crop.x + crop.width).min(buf_w);
    let y2 = (offset_y + crop.y + crop.height).min(buf_h);

    for t in 0..thickness {
        for x in x1..x2 {
            if y1 + t < buf_h {
                buffer[(y1 + t) * buf_w + x] = RED_COLOR;
            }
            if y2.saturating_sub(1 + t) < buf_h {
                buffer[(y2.saturating_sub(1 + t)) * buf_w + x] = RED_COLOR;
            }
        }
        for y in y1..y2 {
            if x1 + t < buf_w {
                buffer[y * buf_w + (x1 + t)] = RED_COLOR;
            }
            if x2.saturating_sub(1 + t) < buf_w {
                buffer[y * buf_w + x2.saturating_sub(1 + t)] = RED_COLOR;
            }
        }
    }
}
