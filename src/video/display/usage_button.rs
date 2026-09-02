use crate::video::buffer::PixelBuffer;
use crate::video::jp_text::JpTextRenderer;
use minifb::{MouseButton, MouseMode, Window};

/// ボタン枠の色(シアン)。
const BORDER_COLOR: u32 = 0x00C8_00FF;
/// ボタン内のテキスト色。
const LABEL_COLOR: u32 = 0x00FF_FFFF;
/// テキストのピクセル高さ。
const LABEL_PIXEL_HEIGHT: f32 = 14.0;
/// テキストと枠の間(パディング)。
const PADDING: usize = 6;

/// フェーズ情報の下に表示する「使用率更新」ボタン。
///
/// ヒットテスト・押下状態・描画を担当する。クリック成立時は
/// `handle_click` が `true` を返す(1フレームだけ)。実際の処理は
/// 呼び出し側が実行する。
pub struct UsageButton {
    rect: Option<(usize, usize, usize, usize)>,
    pressed: bool,
}

impl UsageButton {
    pub fn new() -> Self {
        Self {
            rect: None,
            pressed: false,
        }
    }

    /// (x, y)を左上としてボタン(枠+ラベル)を描画し、ヒット領域を設定する。
    pub fn update(&mut self, buffer: &mut PixelBuffer, renderer: &JpTextRenderer, x: usize, y: usize) {
        let label = "使用率更新";

        // 先にテキストを描いてサイズを取得(枠は描画後に引く)。
        let text_size = renderer.draw(
            buffer,
            x + PADDING,
            y + PADDING,
            label,
            LABEL_COLOR,
            LABEL_PIXEL_HEIGHT,
        );

        let width = text_size.0 + PADDING * 2;
        let height = text_size.1 + PADDING * 2;

        draw_border(buffer, x, y, width, height);
        self.rect = Some((x, y, width, height));
    }

    /// ボタンのクリックを検出する。押下エッジ1回だけ `true` を返す。
    pub fn handle_click(&mut self, window: &Window) -> bool {
        let pressed = window.get_mouse_down(MouseButton::Left);
        if !pressed {
            self.pressed = false;
            return false;
        }

        if self.pressed {
            return false;
        }

        let Some((button_x, button_y, button_w, button_h)) = self.rect else {
            return false;
        };
        let Some((mouse_x, mouse_y)) = window.get_mouse_pos(MouseMode::Discard) else {
            return false;
        };
        let (mouse_x, mouse_y) = (mouse_x as usize, mouse_y as usize);

        if button_x <= mouse_x
            && mouse_x < button_x + button_w
            && button_y <= mouse_y
            && mouse_y < button_y + button_h
        {
            self.pressed = true;
            return true;
        }
        false
    }
}

/// (x, y)を左上とする幅 width・高さ height の矩形枠(1px)を描く。
fn draw_border(buffer: &mut PixelBuffer, x: usize, y: usize, width: usize, height: usize) {
    let buf_w = buffer.width;
    let buf_h = buffer.height;
    let pixels = buffer.pixels_mut();

    for col in 0..width {
        set_pixel(pixels, buf_w, x + col, y);
        if height >= 2 {
            set_pixel(pixels, buf_w, x + col, y + height - 1);
        }
    }
    for row in 0..height {
        set_pixel(pixels, buf_w, x, y + row);
        if width >= 2 {
            set_pixel(pixels, buf_w, x + width - 1, y + row);
        }
    }

    fn set_pixel(pixels: &mut [u32], buf_w: usize, px: usize, py: usize) {
        // 境界チェックは pixels の長さを基準に行う。
        let idx = py * buf_w + px;
        if idx < pixels.len() {
            pixels[idx] = BORDER_COLOR;
        }
    }
}
