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
use crate::video::{CropArea, PixelCropArea};

use super::InferenceConfig;
use super::preprocess::preprocess_white_text_extraction;

/// 「フェーズ：対戦中」判定用の固定クロップ範囲(相対座標)。
/// ユーザーが矢印キーで動かす crop_area とは別枠。
const PHASE_DETECTION_CROP: CropArea = CropArea {
    x: 0.3838,
    y: 0.0175,
    width: 0.2325,
    height: 0.0433,
};

/// このうち3文字以上がOCR結果に含まれていれば「対戦中」とみなす対象文字群。
/// OCRの誤認識・表記ゆれに強くするため、完全一致ではなく文字単位の部分一致数で判定する。
const PHASE_TARGET_CHARS: [char; 7] = ['ラ', 'ン', 'ク', 'バ', 'ト', 'ル', 'グ'];
const PHASE_MATCH_THRESHOLD: usize = 3;
const PHASE_ACTIVE_TEXT: &str = "フェーズ：対戦中";

/// 「フェーズ：対戦終了」判定用の固定クロップ範囲(相対座標)。
const PHASE_END_DETECTION_CROP: CropArea = CropArea {
    x: 0.1463,
    y: 0.8925,
    width: 0.7050,
    height: 0.0508,
};

const PHASE_END_TARGET_CHARS: [char; 14] = [
    '対', '戦', 'を', 'や', 'め', 'る', 'チ', 'ー', 'ム', '編', '成', 'す', '続', 'け',
];
const PHASE_END_MATCH_THRESHOLD: usize = 5;
const PHASE_END_TEXT: &str = "フェーズ: 対戦終了";

pub fn run_ocr_loop(
    rx_ml: Receiver<FrameBuffer>,
    config: InferenceConfig,
    crop_area: Arc<RwLock<CropArea>>,
    phase_status: Arc<RwLock<String>>,
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

        // --- 既存: パーティ名などユーザー調整枠のOCR ---
        let crop = crop_area.read().unwrap().to_pixels(
            config.resolution.width as usize,
            config.resolution.height as usize,
        );

        if let Some(normalized_text) =
            recognize_text_in_crop(&ocr_engine, &frame, &config, crop, 3.0)?
        {
            println!("[OCR Normalized] {normalized_text}");
        }

        // --- 追加: 「ランクバトル」フェーズ判定用の固定枠OCR ---
        let phase_crop = PHASE_DETECTION_CROP.to_pixels(
            config.resolution.width as usize,
            config.resolution.height as usize,
        );

        let phase_text = recognize_text_in_crop(&ocr_engine, &frame, &config, phase_crop, 3.0)?;
        let matched_count = phase_text
            .as_deref()
            .map(count_matched_target_chars_in_set(&PHASE_TARGET_CHARS))
            .unwrap_or(0);

        // 一度「対戦中」と判定したら、以後OCRが外れても表示を消さずに出しっぱなしにする。
        if matched_count >= PHASE_MATCH_THRESHOLD {
            let mut phase_status_guard = phase_status.write().unwrap();
            if *phase_status_guard != PHASE_ACTIVE_TEXT {
                *phase_status_guard = PHASE_ACTIVE_TEXT.to_string();
            }
        }

        // --- 追加: 「対戦をやめる/チーム編成する/続ける」フェーズ判定用の固定枠OCR ---
        // 対戦中の判定より後に評価することで、両方マッチした場合は対戦終了を優先する。
        let phase_end_crop = PHASE_END_DETECTION_CROP.to_pixels(
            config.resolution.width as usize,
            config.resolution.height as usize,
        );

        let phase_end_text =
            recognize_text_in_crop(&ocr_engine, &frame, &config, phase_end_crop, 3.0)?;
        let end_matched_count = phase_end_text
            .as_deref()
            .map(count_matched_target_chars_in_set(&PHASE_END_TARGET_CHARS))
            .unwrap_or(0);

        if end_matched_count >= PHASE_END_MATCH_THRESHOLD {
            let mut phase_status_guard = phase_status.write().unwrap();
            if *phase_status_guard != PHASE_END_TEXT {
                *phase_status_guard = PHASE_END_TEXT.to_string();
            }
        }
    }

    Ok(())
}

/// 指定クロップ範囲に対してOCRを実行し、空白除去済みの認識テキストを返す。
fn recognize_text_in_crop(
    ocr_engine: &OcrEngine,
    frame: &FrameBuffer,
    config: &InferenceConfig,
    crop: PixelCropArea,
    scale_factor: f32,
) -> anyhow::Result<Option<String>> {
    let (rgba_bytes, scaled_w, scaled_h) = preprocess_white_text_extraction(
        frame,
        config.resolution.width as usize,
        config.resolution.height as usize,
        crop,
        scale_factor,
    );

    if rgba_bytes.is_empty() {
        return Ok(None);
    }

    let bitmap = create_software_bitmap(&rgba_bytes, scaled_w as u32, scaled_h as u32)?;

    let async_op = ocr_engine.RecognizeAsync(&bitmap)?;
    let Ok(result) = async_op.get() else {
        return Ok(None);
    };
    let Ok(recognized_text) = result.Text() else {
        return Ok(None);
    };

    let raw_text = recognized_text.to_string();
    let normalized_text: String = raw_text.chars().filter(|c| !c.is_whitespace()).collect();

    Ok(Some(normalized_text))
}

/// target_chars のうち text に含まれる文字の種類数を返す(重複はカウントしない)。
fn count_matched_target_chars_in_set(target_chars: &[char]) -> impl Fn(&str) -> usize + '_ {
    move |text: &str| {
        target_chars
            .iter()
            .filter(|target_char| text.contains(**target_char))
            .count()
    }
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
