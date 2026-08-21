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
        buffer: &mut [u32],
        buf_width: usize,
        buf_height: usize,
        x: usize,
        y: usize,
        text: &str,
        color: u32,
        pixel_height: f32,
    ) -> (usize, usize) {
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

                    let bg = buffer[py * buf_width + px];
                    buffer[py * buf_width + px] = blend(bg, color, coverage);
                });
            }
        }

        (max_x, text_height)
    }
}

fn blend(bg: u32, fg: u32, alpha: f32) -> u32 {
    let alpha = alpha.clamp(0.0, 1.0);
    let bg_r = ((bg >> 16) & 0xFF) as f32;
    let bg_g = ((bg >> 8) & 0xFF) as f32;
    let bg_b = (bg & 0xFF) as f32;

    let fg_r = ((fg >> 16) & 0xFF) as f32;
    let fg_g = ((fg >> 8) & 0xFF) as f32;
    let fg_b = (fg & 0xFF) as f32;

    let r = (fg_r * alpha + bg_r * (1.0 - alpha)) as u32;
    let g = (fg_g * alpha + bg_g * (1.0 - alpha)) as u32;
    let b = (fg_b * alpha + bg_b * (1.0 - alpha)) as u32;

    (r << 16) | (g << 8) | b
}
