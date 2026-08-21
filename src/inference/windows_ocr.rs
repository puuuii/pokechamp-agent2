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

/// ゲームのフェーズ。「待機」と「対戦終了」は同一状態として扱う
/// (対戦終了後は必ず選出待ちに戻るため、内部状態としては同じ場所)。
/// 表示テキストだけ、どちらの経路で入ったかで出し分ける。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GamePhase {
    WaitingOrEnded,
    Selecting,
    Battling,
}

/// 「ランクバトル」リボン表示の固定クロップ範囲(相対座標)。
/// リボンが表示され始めたら「選出」フェーズに入り、
/// リボンが消えたタイミングで「バトル」フェーズに遷移する。
const PHASE_RIBBON_CROP: CropArea = CropArea {
    x: 0.3838,
    y: 0.0175,
    width: 0.2325,
    height: 0.0433,
};
const PHASE_RIBBON_TARGET_CHARS: [char; 8] = ['ラ', 'ン', 'ク', 'バ', 'ト', 'ル', 'シ', 'グ'];
const PHASE_RIBBON_MATCH_THRESHOLD: usize = 3;

/// 「フェーズ：対戦終了」判定用の固定クロップ範囲(相対座標)。
const PHASE_ENDED_CROP: CropArea = CropArea {
    x: 0.1463,
    y: 0.8925,
    width: 0.7050,
    height: 0.0508,
};
const PHASE_ENDED_TARGET_CHARS: [char; 14] = [
    '対', '戦', 'を', 'や', 'め', 'る', 'チ', 'ー', 'ム', '編', '成', 'す', '続', 'け',
];
const PHASE_ENDED_MATCH_THRESHOLD: usize = 5;

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

    // 起動直後は必ず待機状態から始まる。
    let mut current_phase = GamePhase::WaitingOrEnded;
    set_phase_text(&phase_status, "フェーズ：待機");

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

        // --- フェーズ遷移判定 ---
        // 今の状態に応じて、評価すべき判定だけを実行する(順序を担保する要)。
        match current_phase {
            GamePhase::WaitingOrEnded => {
                // 待機/終了のときだけ、リボン検出を評価する。
                let ribbon_crop = PHASE_RIBBON_CROP.to_pixels(
                    config.resolution.width as usize,
                    config.resolution.height as usize,
                );
                let ribbon_text =
                    recognize_text_in_crop(&ocr_engine, &frame, &config, ribbon_crop, 3.0)?;
                let ribbon_matched = ribbon_text
                    .as_deref()
                    .map(count_matched_chars(&PHASE_RIBBON_TARGET_CHARS))
                    .unwrap_or(0);

                if ribbon_matched >= PHASE_RIBBON_MATCH_THRESHOLD {
                    current_phase = GamePhase::Selecting;
                    set_phase_text(&phase_status, "フェーズ：選出");
                }
            }
            GamePhase::Selecting => {
                // 選出中は「ランクバトル」リボンが表示され続けている。
                // リボンが検出されなくなった時点でバトルフェーズに遷移する。
                let ribbon_crop = PHASE_RIBBON_CROP.to_pixels(
                    config.resolution.width as usize,
                    config.resolution.height as usize,
                );
                let ribbon_text =
                    recognize_text_in_crop(&ocr_engine, &frame, &config, ribbon_crop, 3.0)?;
                let ribbon_matched = ribbon_text
                    .as_deref()
                    .map(count_matched_chars(&PHASE_RIBBON_TARGET_CHARS))
                    .unwrap_or(0);

                if ribbon_matched < PHASE_RIBBON_MATCH_THRESHOLD {
                    current_phase = GamePhase::Battling;
                    set_phase_text(&phase_status, "フェーズ：バトル");
                }
            }
            GamePhase::Battling => {
                // バトル中のときだけ、対戦終了判定を評価する。
                let ended_crop = PHASE_ENDED_CROP.to_pixels(
                    config.resolution.width as usize,
                    config.resolution.height as usize,
                );
                let ended_text =
                    recognize_text_in_crop(&ocr_engine, &frame, &config, ended_crop, 3.0)?;
                let ended_matched = ended_text
                    .as_deref()
                    .map(count_matched_chars(&PHASE_ENDED_TARGET_CHARS))
                    .unwrap_or(0);

                if ended_matched >= PHASE_ENDED_MATCH_THRESHOLD {
                    current_phase = GamePhase::WaitingOrEnded;
                    set_phase_text(&phase_status, "フェーズ：対戦終了");
                }
            }
        }
    }

    Ok(())
}

fn set_phase_text(phase_status: &Arc<RwLock<String>>, text: &str) {
    let mut guard = phase_status.write().unwrap();
    if guard.as_str() != text {
        *guard = text.to_string();
    }
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
fn count_matched_chars(target_chars: &[char]) -> impl Fn(&str) -> usize + '_ {
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
