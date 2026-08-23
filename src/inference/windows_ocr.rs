use crossbeam_channel::Receiver;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tracing::{debug, info};

use crate::hardware::FrameBuffer;
use crate::video::CropArea;

use super::phase_detector::PhaseDetector;
use super::{InferenceConfig, ManualPhaseAdvance, PhaseRules};

/// OCR実行ループ。
///
/// ループ制御(シャットダウン、手動進行リクエスト、OCR間隔スロットリング)のみを担当し、
/// フェーズ判定ロジックは `PhaseDetector` に委譲する。
pub fn run_ocr_loop(
    rx_ml: Receiver<FrameBuffer>,
    config: InferenceConfig,
    phase_rules: PhaseRules,
    crop_area: Arc<RwLock<CropArea>>,
    phase_status: Arc<RwLock<String>>,
    manual_phase_advance: ManualPhaseAdvance,
    shutdown: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let ocr_interval = Duration::from_secs(config.ocr_interval_secs);
    let mut detector = PhaseDetector::new(phase_rules, config)?;
    let mut last_ocr_time = Instant::now() - ocr_interval;

    // 起動直後は必ず待機状態から始まる。
    set_phase_text(&phase_status, &detector.phase_text());

    for frame in rx_ml.iter() {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        // 表示側の▶ボタンからの手動進行リクエスト。
        if manual_phase_advance.swap(false, Ordering::Relaxed) {
            let text = detector.advance_manually();
            set_phase_text(&phase_status, &text);
            continue;
        }

        if last_ocr_time.elapsed() < ocr_interval {
            continue;
        }
        last_ocr_time = Instant::now();

        // --- 既存: パーティ名などユーザー調整枠のOCR ---
        run_party_name_ocr(&detector, &frame, &crop_area)?;

        // --- フェーズ遷移判定 ---
        if let Some(change) = detector.tick(&frame)? {
            info!(?change.phase, "Phase transition: {}", change.display_text);
            set_phase_text(&phase_status, &change.display_text);
        }
    }

    Ok(())
}

/// パーティ名などユーザー調整枠のOCR。未完成機能のため現状はログ出力のみ。
fn run_party_name_ocr(
    detector: &PhaseDetector,
    frame: &FrameBuffer,
    crop_area: &Arc<RwLock<CropArea>>,
) -> anyhow::Result<()> {
    let crop = crop_area.read().unwrap();
    if let Some(normalized_text) = detector.recognize_party_name(frame, &crop)? {
        debug!("OCR Normalized: {normalized_text}");
    }
    Ok(())
}

/// 表示側のフェーズテキストを更新する(変化がなければ何もしない)。
fn set_phase_text(phase_status: &Arc<RwLock<String>>, text: &str) {
    let mut guard = phase_status.write().unwrap();
    if guard.as_str() != text {
        *guard = text.to_string();
    }
}
