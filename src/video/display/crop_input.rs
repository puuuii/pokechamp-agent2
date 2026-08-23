use crate::video::CropArea;
use minifb::{Key, KeyRepeat, Window};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const STEP: f32 = 0.0025; // 移動・リサイズの相対ステップ

/// クロップ領域(赤枠)のキーボードコントローラ。
///
/// 矢印キーで移動、Shift+矢印でリサイズ、Dキーでデバッグ表示切替(debuge buildのみ)。
pub struct CropInputController {
    last_input_time: Instant,
    show_debug_frame: bool,
}

impl CropInputController {
    pub fn new() -> Self {
        Self {
            last_input_time: Instant::now(),
            show_debug_frame: cfg!(debug_assertions),
        }
    }

    /// キーボード状態を読み込み、クロップ領域を更新する。
    pub fn handle(&mut self, window: &Window, crop_area: &Arc<RwLock<CropArea>>) {
        if cfg!(debug_assertions) && window.is_key_pressed(Key::D, KeyRepeat::No) {
            self.show_debug_frame = !self.show_debug_frame;
            println!("[Debug Frame] Visibility: {}", self.show_debug_frame);
            return;
        }

        if self.last_input_time.elapsed() < Duration::from_millis(50) {
            return;
        }

        let mut crop = crop_area.write().unwrap();
        let shift =
            window.is_key_down(Key::LeftShift) || window.is_key_down(Key::RightShift);

        let mut changed = false;

        if shift {
            if window.is_key_down(Key::Left) {
                crop.width -= STEP;
                changed = true;
            }
            if window.is_key_down(Key::Right) {
                crop.width += STEP;
                changed = true;
            }
            if window.is_key_down(Key::Up) {
                crop.height -= STEP;
                changed = true;
            }
            if window.is_key_down(Key::Down) {
                crop.height += STEP;
                changed = true;
            }
        } else {
            if window.is_key_down(Key::Left) {
                crop.x -= STEP;
                changed = true;
            }
            if window.is_key_down(Key::Right) {
                crop.x += STEP;
                changed = true;
            }
            if window.is_key_down(Key::Up) {
                crop.y -= STEP;
                changed = true;
            }
            if window.is_key_down(Key::Down) {
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

    /// 動画領域にクロップ赤枠をオーバーレイするかどうか。
    pub fn show_debug_frame(&self) -> bool {
        self.show_debug_frame
    }
}
