#[cfg(windows)]
mod preprocess;
#[cfg(windows)]
mod windows_ocr;

use crossbeam_channel::Receiver;
use std::sync::{Arc, RwLock};
use std::thread;

use crate::hardware::FrameBuffer;
use crate::video::CropArea;

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

/// フェーズ遷移の対象1組の判定パラメータ。
/// cropは判定領域の相対座標、target_charsはOCRテキストに含まれると判定する文字集合、
/// thresholdは判定成立に必要な文字の種類数、enter_textは判定成立時の表示文字列。
#[derive(Debug, Clone, Copy)]
pub struct PhaseTarget {
    pub crop: CropArea,
    pub target_chars: &'static [char],
    pub threshold: usize,
    pub enter_text: &'static str,
}

/// フェーズ遷移の全パラメータを束ねたconfig。
/// ゲーム種別やレイアウトが変わっても、このconfigの追加・変更だけで対応する。
#[derive(Debug, Clone, Copy)]
pub struct PhaseRules {
    pub ribbon: PhaseTarget,
    pub ended: PhaseTarget,
    pub waiting_text: &'static str,
    pub battling_text: &'static str,
}

impl Default for PhaseRules {
    fn default() -> Self {
        Self {
            ribbon: PhaseTarget {
                crop: CropArea {
                    x: 0.3838,
                    y: 0.0175,
                    width: 0.2325,
                    height: 0.0433,
                },
                target_chars: &['ラ', 'ン', 'ク', 'バ', 'ト', 'ル', 'シ', 'グ'],
                threshold: 3,
                enter_text: "フェーズ：選出",
            },
            ended: PhaseTarget {
                crop: CropArea {
                    x: 0.1463,
                    y: 0.8925,
                    width: 0.7050,
                    height: 0.0508,
                },
                target_chars: &[
                    '対', '戦', 'を', 'や', 'め', 'る', 'チ', 'ー', 'ム', '編', '成', 'す', '続',
                    'け',
                ],
                threshold: 5,
                enter_text: "フェーズ：対戦終了",
            },
            waiting_text: "フェーズ：待機",
            battling_text: "フェーズ：バトル",
        }
    }
}

/// OCR結果から導出したUI表示用ステータス文字列の共有領域。空文字列は「非表示」。
pub type PhaseStatus = Arc<RwLock<String>>;

pub struct InferenceWorker;

impl InferenceWorker {
    pub fn spawn(
        rx_ml: Receiver<FrameBuffer>,
        config: InferenceConfig,
        phase_rules: PhaseRules,
        crop_area: Arc<RwLock<CropArea>>,
        phase_status: PhaseStatus,
    ) {
        thread::spawn(move || {
            #[cfg(windows)]
            if let Err(e) =
                windows_ocr::run_ocr_loop(rx_ml, config, &phase_rules, crop_area, phase_status)
            {
                eprintln!("OCR Worker error: {e}");
            }

            #[cfg(not(windows))]
            {
                let _ = phase_status;
                let _ = phase_rules;
                eprintln!("Windows.Media.Ocr is only supported on Windows.");
            }
        });
    }
}
