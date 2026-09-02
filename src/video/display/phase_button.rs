use crate::video::buffer::PixelBuffer;
use minifb::{MouseButton, MouseMode, Window};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::info;

const WIDTH: usize = 12;
const HEIGHT: usize = 16;
const TEXT_GAP: usize = 8;
const COLOR: u32 = 0x00C8_00FF;

/// フェーズテキスト右横の▶ボタン(手動フェーズ進行)。
///
/// ヒットテスト・押下状態・描画を担当する。クリック成立時は
/// 手動フェーズ進行フラグを押下エッジ1回分だけ立てる。
pub struct PhaseButton {
    rect: Option<(usize, usize, usize, usize)>,
    pressed: bool,
    manual_phase_advance: Arc<AtomicBool>,
}

impl PhaseButton {
    pub fn new(manual_phase_advance: Arc<AtomicBool>) -> Self {
        Self {
            rect: None,
            pressed: false,
            manual_phase_advance,
        }
    }

    /// ▶ボタンのクリックを検出する。
    pub fn handle_click(&mut self, window: &Window) {
        let pressed = window.get_mouse_down(MouseButton::Left);
        if !pressed {
            self.pressed = false;
            return;
        }

        if self.pressed {
            return;
        }

        let Some((button_x, button_y, button_w, button_h)) = self.rect else {
            return;
        };
        let Some((mouse_x, mouse_y)) = window.get_mouse_pos(MouseMode::Discard) else {
            return;
        };
        let (mouse_x, mouse_y) = (mouse_x as usize, mouse_y as usize);

        if button_x <= mouse_x
            && mouse_x < button_x + button_w
            && button_y <= mouse_y
            && mouse_y < button_y + button_h
        {
            self.pressed = true;
            self.manual_phase_advance.store(true, Ordering::Relaxed);
            info!("手動フェーズ進行リクエスト");
        }
    }

    /// フェーズテキストのサイズに合わせて配置し描画する。
    /// `text_size.0`が0(テキスト非表示)ならボタンも隠す。
    pub fn update_for_text(
        &mut self,
        buffer: &mut PixelBuffer,
        text_x: usize,
        text_y: usize,
        text_size: (usize, usize),
    ) {
        if text_size.0 == 0 {
            self.rect = None;
            return;
        }

        let button_x = text_x + text_size.0 + TEXT_GAP;
        let button_y = text_y + (text_size.1.saturating_sub(HEIGHT)) / 2;
        draw_button(buffer, button_x, button_y);
        self.rect = Some((button_x, button_y, WIDTH, HEIGHT));
    }

    /// 旧ボタン領域をクリアする(フェーズテキスト再描画の前に使う)。
    pub fn clear(&self, buffer: &mut PixelBuffer, color: u32) {
        if let Some((x, y, w, h)) = self.rect {
            buffer.clear_rect(x, y, w, h, color);
        }
    }
}

/// (x, y)を左上とする右向き▶三角(WIDTH × HEIGHT)を描く。
fn draw_button(buffer: &mut PixelBuffer, x: usize, y: usize) {
    let buf_w = buffer.width;
    let buf_h = buffer.height;
    let pixels = buffer.pixels_mut();
    let center = HEIGHT as f32 / 2.0;

    for dy in 0..HEIGHT {
        let dist = (dy as f32 - center).abs();
        let span = (WIDTH as f32 * (1.0 - dist / center)) as usize;
        for dx in 0..span {
            let px = x + dx;
            let py = y + dy;
            if px < buf_w && py < buf_h {
                pixels[py * buf_w + px] = COLOR;
            }
        }
    }
}
