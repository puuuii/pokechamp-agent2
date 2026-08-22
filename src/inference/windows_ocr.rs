use crossbeam_channel::Receiver;
use std::sync::atomic::Ordering;
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

use super::preprocess::preprocess_white_text_extraction;
use super::{InferenceConfig, ManualPhaseAdvance, PhaseRules, PhaseTarget};

/// ゲームのフェーズ。
/// 「待機」と「対戦終了」は自動判定上は同一の監視状態だが、
/// 表示テキスト(および手動進行のサイクル)で区別するため別状態として扱う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Waiting,
    Selecting,
    Battling,
    Ended,
}

impl Phase {
    /// 手動進行時の次のフェーズ(選出→バトル→対戦終了→選出…)。
    /// 待機は起動時のみで、サイクルには含めない。
    const fn next(self) -> Self {
        match self {
            Self::Waiting => Self::Selecting,
            Self::Selecting => Self::Battling,
            Self::Battling => Self::Ended,
            Self::Ended => Self::Selecting,
        }
    }
}

/// 各フェーズの表示テキスト。
fn phase_display_text(phase_rules: &PhaseRules, phase: Phase) -> &'static str {
    match phase {
        Phase::Waiting => phase_rules.waiting_text,
        Phase::Selecting => phase_rules.ribbon.enter_text,
        Phase::Battling => phase_rules.battling_text,
        Phase::Ended => phase_rules.ended.enter_text,
    }
}

pub fn run_ocr_loop(
    rx_ml: Receiver<FrameBuffer>,
    config: InferenceConfig,
    phase_rules: &PhaseRules,
    crop_area: Arc<RwLock<CropArea>>,
    phase_status: Arc<RwLock<String>>,
    manual_phase_advance: ManualPhaseAdvance,
) -> anyhow::Result<()> {
    let ja_lang = Language::CreateLanguage(&windows::core::HSTRING::from("ja-JP"))?;

    if !OcrEngine::IsLanguageSupported(&ja_lang)? {
        anyhow::bail!("Windowsの「日本語」言語パック（OCR）がインストールされていません。");
    }

    let ocr_engine = OcrEngine::TryCreateFromLanguage(&ja_lang)?;

    const OCR_INTERVAL: Duration = Duration::from_secs(3);
    let mut last_ocr_time = Instant::now() - OCR_INTERVAL;

    // 起動直後は必ず待機状態から始まる。
    let mut current_phase = Phase::Waiting;
    set_phase_text(&phase_status, phase_rules.waiting_text);

    for frame in rx_ml.iter() {
        // 表示側の▶ボタンからの手動進行リクエスト。
        if manual_phase_advance.swap(false, Ordering::Relaxed) {
            current_phase = current_phase.next();
            set_phase_text(
                &phase_status,
                phase_display_text(phase_rules, current_phase),
            );
            continue;
        }

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
            // 待機・対戦終了は同一の監視状態として扱う。
            Phase::Waiting | Phase::Ended => {
                // まず「対戦終了」文字列がまだ表示されているか確認
                let ended_matched =
                    ocr_target_match(&ocr_engine, &frame, &config, &phase_rules.ended)?;

                // 対戦終了文字列が残っている間は、このフェーズに留まる
                if ended_matched >= phase_rules.ended.threshold {
                    current_phase = Phase::Ended;
                    set_phase_text(&phase_status, phase_rules.ended.enter_text);
                    continue;
                }

                // 対戦終了文字列が消えたら、待機状態としてランクバトルを監視
                let ribbon_matched =
                    ocr_target_match(&ocr_engine, &frame, &config, &phase_rules.ribbon)?;

                if ribbon_matched >= phase_rules.ribbon.threshold {
                    current_phase = Phase::Selecting;
                    set_phase_text(&phase_status, phase_rules.ribbon.enter_text);
                }
            }
            Phase::Selecting => {
                // 選出中は「ランクバトル」リボンが表示され続けている。
                // リボンが検出されなくなった時点でバトルフェーズに遷移する。
                let ribbon_matched =
                    ocr_target_match(&ocr_engine, &frame, &config, &phase_rules.ribbon)?;

                if ribbon_matched < phase_rules.ribbon.threshold {
                    current_phase = Phase::Battling;
                    set_phase_text(&phase_status, phase_rules.battling_text);
                }
            }
            Phase::Battling => {
                // バトル中のときだけ、対戦終了判定を評価する。
                let ended_matched =
                    ocr_target_match(&ocr_engine, &frame, &config, &phase_rules.ended)?;

                if ended_matched >= phase_rules.ended.threshold {
                    current_phase = Phase::Ended;
                    set_phase_text(&phase_status, phase_rules.ended.enter_text);
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

/// PhaseTarget のクロップ範囲でOCRを実行し、対象文字の種類数を返す。
fn ocr_target_match(
    ocr_engine: &OcrEngine,
    frame: &FrameBuffer,
    config: &InferenceConfig,
    target: &PhaseTarget,
) -> anyhow::Result<usize> {
    let crop = target.crop.to_pixels(
        config.resolution.width as usize,
        config.resolution.height as usize,
    );

    let text = recognize_text_in_crop(ocr_engine, frame, config, crop, 3.0)?;

    Ok(text
        .as_deref()
        .map(count_matched_chars(target.target_chars))
        .unwrap_or(0))
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
