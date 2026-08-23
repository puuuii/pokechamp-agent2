#[cfg(windows)]
mod preprocess;
#[cfg(windows)]
mod windows_ocr;

use anyhow::Context;
use crossbeam_channel::Receiver;
use serde::Deserialize;
use std::fs;
use std::sync::atomic::AtomicBool;
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

    /// (幅, 高さ) の usize 対として返す。
    #[allow(dead_code)]
    pub fn as_usize(&self) -> (usize, usize) {
        (self.width as usize, self.height as usize)
    }
}

#[allow(dead_code)]
pub struct InferenceConfig {
    pub resolution: ModelInputResolution,
}

/// フェーズ遷移の対象1組の判定パラメータ。
/// cropは判定領域の相対座標、target_charsはOCRテキストに含まれると判定する文字集合、
/// thresholdは判定成立に必要な文字の種類数、enter_textは判定成立時の表示文字列。
#[derive(Debug, Clone, Deserialize)]
pub struct PhaseTarget {
    pub crop: CropArea,
    pub target_chars: Vec<String>,
    pub threshold: usize,
    pub enter_text: String,
}

/// フェーズ遷移の全パラメータを束ねたconfig。
/// ゲーム種別やレイアウトが変わっても、このconfigの追加・変更だけで対応する。
/// 通常は TOML ファイル(config/phase_rules.toml)から読み込む。
#[derive(Debug, Clone, Deserialize)]
pub struct PhaseRules {
    pub ribbon: PhaseTarget,
    pub ended: PhaseTarget,
    pub waiting_text: String,
    pub battling_text: String,
}

impl PhaseRules {
    /// TOML ファイルからフェーズルールを読み込む。
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("フェーズルールの設定ファイルが読めません: {path}"))?;
        let rules: PhaseRules = toml::from_str(&contents)
            .with_context(|| format!("フェーズルールの設定ファイルの解析に失敗しました: {path}"))?;
        Ok(rules)
    }
}

/// 組み込みデフォルト。TOML 設定が読めないときのフォールバック用。
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
                target_chars: ["ラ", "ン", "ク", "バ", "ト", "ル", "シ", "グ"]
                    .map(str::to_string)
                    .to_vec(),
                threshold: 3,
                enter_text: "フェーズ：選出".to_string(),
            },
            ended: PhaseTarget {
                crop: CropArea {
                    x: 0.1463,
                    y: 0.8925,
                    width: 0.7050,
                    height: 0.0508,
                },
                target_chars: [
                    "対", "戦", "を", "や", "め", "る", "チ", "ー", "ム", "編", "成", "す", "続", "け",
                ]
                .map(str::to_string)
                .to_vec(),
                threshold: 5,
                enter_text: "フェーズ：対戦終了".to_string(),
            },
            waiting_text: "フェーズ：待機".to_string(),
            battling_text: "フェーズ：バトル".to_string(),
        }
    }
}

/// OCR結果から導出したUI表示用ステータス文字列の共有領域。空文字列は「非表示」。
pub type PhaseStatus = Arc<RwLock<String>>;

/// 表示側からの手動フェーズ進行リクエスト。
/// OCRワーカーが swap(false) で消費するフラグ。
pub type ManualPhaseAdvance = Arc<AtomicBool>;

pub struct InferenceWorker;

impl InferenceWorker {
    pub fn spawn(
        rx_ml: Receiver<FrameBuffer>,
        config: InferenceConfig,
        phase_rules: PhaseRules,
        crop_area: Arc<RwLock<CropArea>>,
        phase_status: PhaseStatus,
        manual_phase_advance: ManualPhaseAdvance,
    ) {
        thread::spawn(move || {
            #[cfg(windows)]
            if let Err(e) = windows_ocr::run_ocr_loop(
                rx_ml,
                config,
                &phase_rules,
                crop_area,
                phase_status,
                manual_phase_advance,
            ) {
                eprintln!("OCR Worker error: {e}");
            }

            #[cfg(not(windows))]
            {
                let _ = phase_status;
                let _ = phase_rules;
                let _ = manual_phase_advance;
                eprintln!("Windows.Media.Ocr is only supported on Windows.");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::PhaseRules;

    /// TOML 設定がパースでき、組み込みデフォルトと一致することを確認する。
    #[test]
    fn toml_config_parses_and_matches_default() {
        let rules = PhaseRules::load("config/phase_rules.toml")
            .expect("config/phase_rules.toml must parse");
        let default = PhaseRules::default();
        assert_eq!(rules.ribbon.crop, default.ribbon.crop);
        assert_eq!(rules.ribbon.target_chars, default.ribbon.target_chars);
        assert_eq!(rules.ribbon.threshold, default.ribbon.threshold);
        assert_eq!(rules.ribbon.enter_text, default.ribbon.enter_text);
        assert_eq!(rules.ended.crop, default.ended.crop);
        assert_eq!(rules.ended.target_chars, default.ended.target_chars);
        assert_eq!(rules.ended.threshold, default.ended.threshold);
        assert_eq!(rules.ended.enter_text, default.ended.enter_text);
        assert_eq!(rules.waiting_text, default.waiting_text);
        assert_eq!(rules.battling_text, default.battling_text);
    }
}
