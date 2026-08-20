use crossbeam_channel::Receiver;
use std::thread;
use std::time::{Duration, Instant};

use crate::hardware::FrameBuffer;

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
    pub fn spawn(rx_ml: Receiver<FrameBuffer>, config: InferenceConfig) {
        thread::spawn(move || {
            #[cfg(windows)]
            if let Err(e) = Self::run_ocr_loop(rx_ml, config) {
                eprintln!("OCR Worker error: {e}");
            }

            #[cfg(not(windows))]
            eprintln!("Windows.Media.Ocr is only supported on Windows.");
        });
    }

    #[cfg(windows)]
    fn run_ocr_loop(rx_ml: Receiver<FrameBuffer>, config: InferenceConfig) -> anyhow::Result<()> {
        let ja_lang = Language::CreateLanguage(&windows::core::HSTRING::from("ja-JP"))?;

        if !OcrEngine::IsLanguageSupported(&ja_lang)? {
            anyhow::bail!("Windowsの「日本語」言語パック（OCR）がインストールされていません。");
        }

        let ocr_engine = OcrEngine::TryCreateFromLanguage(&ja_lang)?;

        const OCR_INTERVAL: Duration = Duration::from_secs(3);
        let mut last_ocr_time = Instant::now() - OCR_INTERVAL;

        let target_text = "選出してください";

        for frame in rx_ml.iter() {
            if last_ocr_time.elapsed() < OCR_INTERVAL {
                continue;
            }
            last_ocr_time = Instant::now();

            // 1. 画面全体の前処理（白黒二値化でコントラスト強調）
            let rgba_bytes = preprocess_full_frame(&frame);

            // 2. ビットマップの作成
            let bitmap = match create_software_bitmap(
                &rgba_bytes,
                config.resolution.width,
                config.resolution.height,
            ) {
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

                    // スペース、全角スペース、改行をすべて削除して1つの文字列に結合
                    let normalized_text: String =
                        raw_text.chars().filter(|c| !c.is_whitespace()).collect();

                    if normalized_text.contains(target_text) {
                        println!("[OCR SUCCESS] 画面内に「{target_text}」を検出しました！");
                    } else {
                        println!("[OCR Debug] 検出結果(正規化済): {normalized_text}");
                    }
                }
            }
        }

        Ok(())
    }
}

/// 画面全体のコントラストを強調（白黒二値化）して RGBA バイト列に変換する
#[cfg(windows)]
fn preprocess_full_frame(frame: &[u32]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(frame.len() * 4);

    for &pixel in frame {
        let r = ((pixel >> 16) & 0xFF) as u32;
        let g = ((pixel >> 8) & 0xFF) as u32;
        let b = (pixel & 0xFF) as u32;

        // 輝度（明るさ）の計算
        let luma = (r * 299 + g * 587 + b * 114) / 1000;

        // 二値化処理: 明るい文字（白など）を強調し、背景ノイズを切る
        // ※ゲーム画面の文字色に合わせて閾値(170)は適宜調整可能
        let val = if luma > 170 { 255u8 } else { 0u8 };

        rgba.push(val); // R
        rgba.push(val); // G
        rgba.push(val); // B
        rgba.push(255); // A
    }

    rgba
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
