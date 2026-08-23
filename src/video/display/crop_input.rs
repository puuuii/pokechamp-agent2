use crate::video::CropArea;
use minifb::{Key, KeyRepeat, Window};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tracing::debug;

/// クロップ領域(赤枠)のキーボードコントローラ。
///
/// 矢印キーで移動、Shift+矢印でリサイズ、Dキーでデバッグ表示切替(debuge buildのみ)。
pub struct CropInputController {
    last_input_time: Instant,
    show_debug_frame: bool,
    /// 移動・リサイズの相対ステップ。
    step: f32,
}

impl CropInputController {
    pub fn new(step: f32) -> Self {
        Self {
            last_input_time: Instant::now(),
            show_debug_frame: cfg!(debug_assertions),
            step,
        }
    }

    /// キーボード状態を読み込み、クロップ領域を更新する。
    pub fn handle(&mut self, window: &Window, crop_area: &Arc<RwLock<CropArea>>) {
        if cfg!(debug_assertions) && window.is_key_pressed(Key::D, KeyRepeat::No) {
            self.show_debug_frame = !self.show_debug_frame;
            debug!("デバッグフレーム表示: {}", self.show_debug_frame);
            return;
        }

        if self.last_input_time.elapsed() < Duration::from_millis(50) {
            return;
        }

        let mut crop = crop_area.write().unwrap();
        let shift =
            window.is_key_down(Key::LeftShift) || window.is_key_down(Key::RightShift);

        if self.process_directional_keys(window, &mut crop, shift) {
            crop.clamp();
            self.last_input_time = Instant::now();
            debug!(
                "クロップ枠 x: {:.4}, y: {:.4}, w: {:.4}, h: {:.4}",
                crop.x, crop.y, crop.width, crop.height
            );
        }
    }

    /// 矢印キーの移動(Shiftなし)/リサイズ(Shiftあり)を処理し、適用があればtrue。
    fn process_directional_keys(&self, window: &Window, crop: &mut CropArea, shift: bool) -> bool {
        // Shift中はリサイズ(width/height)、通常時は移動(x/y)。
        let (horizontal, vertical) = if shift {
            (&mut crop.width, &mut crop.height)
        } else {
            (&mut crop.x, &mut crop.y)
        };

        let mut changed = false;
        changed |= Self::adjust_field(window, horizontal, Key::Left, -self.step);
        changed |= Self::adjust_field(window, horizontal, Key::Right, self.step);
        changed |= Self::adjust_field(window, vertical, Key::Up, -self.step);
        changed |= Self::adjust_field(window, vertical, Key::Down, self.step);
        changed
    }

    /// keyが押されているならfieldにdeltaを適用してtrueを返す。
    fn adjust_field(window: &Window, field: &mut f32, key: Key, delta: f32) -> bool {
        if window.is_key_down(key) {
            *field += delta;
            true
        } else {
            false
        }
    }

    /// 動画領域にクロップ赤枠をオーバーレイするかどうか。
    pub fn show_debug_frame(&self) -> bool {
        self.show_debug_frame
    }
}
