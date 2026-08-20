use crossbeam_channel::Receiver;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::hardware::FrameBuffer;
use crate::video::CropArea;

#[cfg(windows)]
use windows::{
    Globalization::Language,
    Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap},
    Media::Ocr::OcrEngine,
    Storage::Streams::DataWriter,
};

#[allow(dead_code)]
pub struct ModelInputResolution {
    pub width: u32,
    pub height: u32,
}

impl ModelInputResolution {
    pub const STANDARD_1280X720: Self = Self {
        width: 1280,
        height: 720,
    };
}

#[allow(dead_code)]
pub struct InferenceConfig {
    pub resolution: ModelInputResolution,
}

pub struct InferenceWorker;

impl InferenceWorker {
    pub fn spawn(
        rx_ml: Receiver<FrameBuffer>,
        config: InferenceConfig,
        crop_area: Arc<RwLock<CropArea>>,
    ) {
        thread::spawn(move || {
            #[cfg(windows)]
            if let Err(e) = Self::run_ocr_loop(rx_ml, config, crop_area) {
                eprintln!("OCR Worker error: {e}");
            }

            #[cfg(not(windows))]
            eprintln!("Windows.Media.Ocr is only supported on Windows.");
        });
    }

    #[cfg(windows)]
    fn run_ocr_loop(
        rx_ml: Receiver<FrameBuffer>,
        config: InferenceConfig,
        crop_area: Arc<RwLock<CropArea>>,
    ) -> anyhow::Result<()> {
        let ja_lang = Language::CreateLanguage(&windows::core::HSTRING::from("ja-JP"))?;

        if !OcrEngine::IsLanguageSupported(&ja_lang)? {
            anyhow::bail!("Windowsの「日本語」言語パック（OCR）がインストールされていません。");
        }

        let ocr_engine = OcrEngine::TryCreateFromLanguage(&ja_lang)?;

        const OCR_INTERVAL: Duration = Duration::from_secs(3);
        let mut last_ocr_time = Instant::now() - OCR_INTERVAL;

        for frame in rx_ml.iter() {
            if last_ocr_time.elapsed() < OCR_INTERVAL {
                continue;
            }
            last_ocr_time = Instant::now();

            let crop = *crop_area.read().unwrap();

            // 1. バイリニア3倍拡大 ＋ 高輝度（白文字の芯）抽出 ＋ 白背景黒文字化
            let scale_factor = 3.0f32;
            let (rgba_bytes, scaled_w, scaled_h) = preprocess_white_text_extraction(
                &frame,
                config.resolution.width as usize,
                config.resolution.height as usize,
                crop,
                scale_factor,
            );

            if rgba_bytes.is_empty() {
                continue;
            }

            // 2. SoftwareBitmap を生成
            let bitmap = match create_software_bitmap(&rgba_bytes, scaled_w as u32, scaled_h as u32)
            {
                Ok(bm) => bm,
                Err(e) => {
                    eprintln!("Failed to create SoftwareBitmap: {e}");
                    continue;
                }
            };

            // 3. OCR実行とテキスト正規化
            let async_op = ocr_engine.RecognizeAsync(&bitmap)?;
            if let Ok(result) = async_op.get() {
                if let Ok(recognized_text) = result.Text() {
                    let raw_text = recognized_text.to_string();
                    let normalized_text: String =
                        raw_text.chars().filter(|c| !c.is_whitespace()).collect();

                    println!("[OCR Normalized] {normalized_text}");
                }
            }
        }

        Ok(())
    }
}

/// バイリニア補間（2D）で画素を取得
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

/// ゲーム画面のUI文字（高輝度な白文字）を切り出し、白背景・黒文字化する前処理
#[cfg(windows)]
fn preprocess_white_text_extraction(
    frame: &[u32],
    full_width: usize,
    full_height: usize,
    crop: CropArea,
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

    // 白文字抽出の閾値 (RGBがすべてこの値以上＝ほぼ白色の芯だけ抽出)
    const WHITE_THRESHOLD: u8 = 180;

    for sy in 0..scaled_h {
        let src_y = crop.y as f32 + (sy as f32 / scale_factor).min((crop_h - 1) as f32);

        for sx in 0..scaled_w {
            let src_x = crop.x as f32 + (sx as f32 / scale_factor).min((crop_w - 1) as f32);

            let (r, g, b) = bilinear_sample(frame, full_width, full_height, src_x, src_y);

            // R, G, B のすべてが高く「白い文字の芯」と判定できるピクセルか？
            let is_white_text_core =
                r >= WHITE_THRESHOLD && g >= WHITE_THRESHOLD && b >= WHITE_THRESHOLD;

            // OCRが最も認識しやすい「白背景(255) に 黒文字(0)」へ変換
            let val = if is_white_text_core { 0u8 } else { 255u8 };

            rgba.push(val); // R
            rgba.push(val); // G
            rgba.push(val); // B
            rgba.push(255); // A
        }
    }

    (rgba, scaled_w, scaled_h)
}

#[cfg(windows)]
fn create_software_bitmap(
    rgba_bytes: &[u8],
    width: u32,
    height: u32,
) -> windows::core::Result<SoftwareBitmap> {
    let writer = DataWriter::new()?;
    writer.WriteBytes(rgba_bytes)?;
    let buffer = writer.DetachBuffer()?;

    SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
        &buffer,
        BitmapPixelFormat::Rgba8,
        width as i32,
        height as i32,
        BitmapAlphaMode::Ignore,
    )
}
