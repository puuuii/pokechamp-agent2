use crate::video::PixelCropArea;
#[inline(always)]
fn bilinear_sample(frame: &[u32], full_w: usize, full_h: usize, x: f32, y: f32) -> (u8, u8, u8) {
    let x0 = (x.floor() as usize).min(full_w - 1);
    let y0 = (y.floor() as usize).min(full_h - 1);
    let x1 = (x0 + 1).min(full_w - 1);
    let y1 = (y0 + 1).min(full_h - 1);

    let dx = x - x0 as f32;
    let dy = y - y0 as f32;

    let p00 = frame[y0 * full_w + x0];
    let p10 = frame[y0 * full_w + x1];
    let p01 = frame[y1 * full_w + x0];
    let p11 = frame[y1 * full_w + x1];

    let unpack = |p: u32| {
        (
            ((p >> 16) & 0xFF) as f32,
            ((p >> 8) & 0xFF) as f32,
            (p & 0xFF) as f32,
        )
    };

    let (r00, g00, b00) = unpack(p00);
    let (r10, g10, b10) = unpack(p10);
    let (r01, g01, b01) = unpack(p01);
    let (r11, g11, b11) = unpack(p11);

    let r = r00 * (1.0 - dx) * (1.0 - dy)
        + r10 * dx * (1.0 - dy)
        + r01 * (1.0 - dx) * dy
        + r11 * dx * dy;
    let g = g00 * (1.0 - dx) * (1.0 - dy)
        + g10 * dx * (1.0 - dy)
        + g01 * (1.0 - dx) * dy
        + g11 * dx * dy;
    let b = b00 * (1.0 - dx) * (1.0 - dy)
        + b10 * dx * (1.0 - dy)
        + b01 * (1.0 - dx) * dy
        + b11 * dx * dy;

    (r as u8, g as u8, b as u8)
}

pub fn preprocess_white_text_extraction(
    frame: &[u32],
    full_width: usize,
    full_height: usize,
    crop: PixelCropArea,
    scale_factor: f32,
) -> (Vec<u8>, usize, usize) {
    let scaled_w = ((crop.width as f32) * scale_factor) as usize;
    let scaled_h = ((crop.height as f32) * scale_factor) as usize;

    if scaled_w == 0 || scaled_h == 0 {
        return (Vec::new(), 0, 0);
    }

    let mut rgba = Vec::with_capacity(scaled_w * scaled_h * 4);

    let max_x = (crop.x + crop.width).min(full_width);
    let max_y = (crop.y + crop.height).min(full_height);
    let crop_w = max_x.saturating_sub(crop.x);
    let crop_h = max_y.saturating_sub(crop.y);

    if crop_w == 0 || crop_h == 0 {
        return (Vec::new(), 0, 0);
    }

    const WHITE_THRESHOLD: u8 = 180;

    for sy in 0..scaled_h {
        let src_y = crop.y as f32 + (sy as f32 / scale_factor).min((crop_h - 1) as f32);

        for sx in 0..scaled_w {
            let src_x = crop.x as f32 + (sx as f32 / scale_factor).min((crop_w - 1) as f32);

            let (r, g, b) = bilinear_sample(frame, full_width, full_height, src_x, src_y);

            let is_white_text_core =
                r >= WHITE_THRESHOLD && g >= WHITE_THRESHOLD && b >= WHITE_THRESHOLD;

            let val = if is_white_text_core { 0u8 } else { 255u8 };

            rgba.push(val);
            rgba.push(val);
            rgba.push(val);
            rgba.push(255);
        }
    }

    (rgba, scaled_w, scaled_h)
}
