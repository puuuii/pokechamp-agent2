use super::buffer::PixelBuffer;
use super::pixel::{pack_rgb, unpack_rgb};
use anyhow::Result;
use rusttype::{Font, Scale, point};

/// Windowsに標準搭載されている日本語対応フォントの候補。
/// 上から順に探して、最初に読み込めたものを使う。
const CANDIDATE_FONT_PATHS: &[&str] = &[
    r"C:\Windows\Fonts\YuGothR.ttc",
    r"C:\Windows\Fonts\meiryo.ttc",
    r"C:\Windows\Fonts\msgothic.ttc",
];

pub struct JpTextRenderer {
    font: Font<'static>,
}

impl JpTextRenderer {
    pub fn load_system_font() -> Result<Self> {
        for path in CANDIDATE_FONT_PATHS {
            if let Ok(bytes) = std::fs::read(path) {
                // rusttype 0.9では`FontCollection`が廃止されており、
                // ttcファイルでも先頭フォントを直接 Font::try_from_vec で読み込める。
                if let Some(font) = Font::try_from_vec(bytes) {
                    return Ok(Self { font });
                }
            }
        }

        anyhow::bail!(
            "日本語フォントが見つかりませんでした。候補パス: {:?}",
            CANDIDATE_FONT_PATHS
        )
    }

    /// buffer上の (x, y) を基準に text をラスタライズして描画する。
    /// 描画した領域の (width, height) を返す(次回の再描画時のクリアに使う)。
    pub fn draw(
        &self,
        buffer: &mut PixelBuffer,
        x: usize,
        y: usize,
        text: &str,
        color: u32,
        pixel_height: f32,
    ) -> (usize, usize) {
        let buf_width = buffer.width;
        let buf_height = buffer.height;
        let pixels = buffer.pixels_mut();
        let scale = Scale::uniform(pixel_height);
        let v_metrics = self.font.v_metrics(scale);
        let offset = point(0.0, v_metrics.ascent);

        let glyphs: Vec<_> = self.font.layout(text, scale, offset).collect();

        let mut max_x = 0usize;
        let text_height = (v_metrics.ascent - v_metrics.descent).ceil() as usize;

        for glyph in &glyphs {
            if let Some(bounding_box) = glyph.pixel_bounding_box() {
                glyph.draw(|gx, gy, coverage| {
                    if coverage <= 0.0 {
                        return;
                    }

                    let px = x as i32 + bounding_box.min.x + gx as i32;
                    let py = y as i32 + bounding_box.min.y + gy as i32;

                    if px < 0 || py < 0 {
                        return;
                    }
                    let (px, py) = (px as usize, py as usize);
                    if px >= buf_width || py >= buf_height {
                        return;
                    }

                    max_x = max_x.max(px + 1 - x);

                    let bg = pixels[py * buf_width + px];
                    pixels[py * buf_width + px] = blend(bg, color, coverage);
                });
            }
        }

        (max_x, text_height)
    }
}

fn blend(bg: u32, fg: u32, alpha: f32) -> u32 {
    let alpha = alpha.clamp(0.0, 1.0);
    let (bg_r, bg_g, bg_b) = unpack_rgb(bg);
    let (fg_r, fg_g, fg_b) = unpack_rgb(fg);

    let r = (fg_r as f32 * alpha + bg_r as f32 * (1.0 - alpha)) as u8;
    let g = (fg_g as f32 * alpha + bg_g as f32 * (1.0 - alpha)) as u8;
    let b = (fg_b as f32 * alpha + bg_b as f32 * (1.0 - alpha)) as u8;

    pack_rgb(r, g, b)
}
