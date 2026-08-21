use crossbeam_channel::Receiver;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use windows::{
    Globalization::Language,
    Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap},
    Media::Ocr::OcrEngine,
    Storage::Streams::DataWriter,
};

use crate::hardware::FrameBuffer;
use crate::video::CropArea;

use super::InferenceConfig;
use super::preprocess::preprocess_white_text_extraction;

pub fn run_ocr_loop(
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

        let crop = crop_area.read().unwrap().to_pixels(
            config.resolution.width as usize,
            config.resolution.height as usize,
        );

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

        let bitmap = match create_software_bitmap(&rgba_bytes, scaled_w as u32, scaled_h as u32) {
            Ok(bm) => bm,
            Err(e) => {
                eprintln!("Failed to create SoftwareBitmap: {e}");
                continue;
            }
        };

        let async_op = ocr_engine.RecognizeAsync(&bitmap)?;
        if let Ok(result) = async_op.get()
            && let Ok(recognized_text) = result.Text()
        {
            let raw_text = recognized_text.to_string();
            let normalized_text: String = raw_text.chars().filter(|c| !c.is_whitespace()).collect();

            println!("[OCR Normalized] {normalized_text}");
        }
    }

    Ok(())
}

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
