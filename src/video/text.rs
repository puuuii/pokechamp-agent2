// 依存なしの最小5x7ビットマップフォント。
// 現状は "TEST TEXT" の表示に必要な文字だけ用意している。
// 新しい文字が要る場合はここにグリフを追加すること。

pub const GLYPH_WIDTH: usize = 5;
pub const GLYPH_HEIGHT: usize = 7;

fn glyph_rows(c: char) -> Option<[&'static str; GLYPH_HEIGHT]> {
    match c.to_ascii_uppercase() {
        'T' => Some([
            "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#..",
        ]),
        'E' => Some([
            "#####", "#....", "#....", "####.", "#....", "#....", "#####",
        ]),
        'S' => Some([
            ".####", "#....", "#....", ".###.", "....#", "....#", "####.",
        ]),
        'X' => Some([
            "#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#",
        ]),
        ' ' => Some([
            ".....", ".....", ".....", ".....", ".....", ".....", ".....",
        ]),
        _ => None,
    }
}

/// buffer(幅buf_width, 高さbuf_height の u32ピクセルバッファ)に
/// (start_x, start_y) を左上として text を描画する。
/// scale はピクセルの拡大倍率、char_spacing は文字間の余白(px)。
pub fn draw_text(
    buffer: &mut [u32],
    buf_width: usize,
    buf_height: usize,
    start_x: usize,
    start_y: usize,
    text: &str,
    color: u32,
    scale: usize,
    char_spacing: usize,
) {
    let scale = scale.max(1);
    let mut cursor_x = start_x;

    for ch in text.chars() {
        let Some(rows) = glyph_rows(ch) else {
            cursor_x += (GLYPH_WIDTH * scale) + char_spacing;
            continue;
        };

        for (row_idx, row) in rows.iter().enumerate() {
            for (col_idx, cell) in row.chars().enumerate() {
                if cell != '#' {
                    continue;
                }

                let px_origin_x = cursor_x + col_idx * scale;
                let px_origin_y = start_y + row_idx * scale;

                for dy in 0..scale {
                    let py = px_origin_y + dy;
                    if py >= buf_height {
                        continue;
                    }
                    for dx in 0..scale {
                        let px = px_origin_x + dx;
                        if px >= buf_width {
                            continue;
                        }
                        buffer[py * buf_width + px] = color;
                    }
                }
            }
        }

        cursor_x += (GLYPH_WIDTH * scale) + char_spacing;
    }
}
