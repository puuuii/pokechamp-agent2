mod analyzer;
mod phase_detector;
mod preprocess;
mod windows_ocr;

use crossbeam_channel::Receiver;
use serde::Deserialize;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;
use tracing::error;

use crate::hardware::FrameBuffer;
use crate::video::CropArea;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct ModelInputResolution {
    pub width: u32,
    pub height: u32,
}

impl ModelInputResolution {
    pub const STANDARD_1280X720: Self = Self {
        width: 1280,
        height: 720,
    };

    /// (幅, 高さ) の usize 対として返す(OCR前処理で使う)。
    pub fn as_usize(&self) -> (usize, usize) {
        (self.width as usize, self.height as usize)
    }
}

/// 推論パラメータのconfig。
/// 通常は TOML ファイル(config/inference.toml)から読み込む。
///
/// 分析器(analyzer)の種類ごとにパラメータを個別 struct に束ね、
/// それを `InferenceConfig` のフィールドとして追加していく。
#[derive(Debug, Clone, Deserialize)]
pub struct InferenceConfig {
    pub resolution: ModelInputResolution,
    /// OCR(`PhaseDetector`)用パラメータ。
    pub ocr: OcrConfig,
}

/// OCR(`PhaseDetector`)用パラメータ。
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct OcrConfig {
    /// フェーズ判定OCRの実行間隔(秒)。
    pub interval_secs: u64,
    /// OCR用の拡大率。
    pub upscale_factor: f32,
    /// 白文字抽出のしきい値(0-255)。
    pub white_text_threshold: u8,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            resolution: ModelInputResolution::STANDARD_1280X720,
            ocr: OcrConfig::default(),
        }
    }
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            interval_secs: 3,
            upscale_factor: 3.0,
            white_text_threshold: 180,
        }
    }
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
    /// 推論スレッドを起動する。
    ///
    /// シャットダウン時にjoinするための `JoinHandle` を返す。
    pub fn spawn(
        rx_ml: Receiver<FrameBuffer>,
        config: InferenceConfig,
        phase_rules: PhaseRules,
        crop_area: Arc<RwLock<CropArea>>,
        phase_status: PhaseStatus,
        manual_phase_advance: ManualPhaseAdvance,
        shutdown: Arc<AtomicBool>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            if let Err(e) = run_inference_thread(
                rx_ml,
                config,
                phase_rules,
                crop_area,
                phase_status,
                manual_phase_advance,
                shutdown,
            ) {
                error!("OCR Worker error: {e}");
            }
        })
    }
}

/// 推論スレッド本体。
///
/// OCR分析器を構築し、ループ制御は汎用分析ループに委譲する。
/// 新しい分析器(ML等)を追加するときは、こちらの構築部分だけを差し替える。
fn run_inference_thread(
    rx_ml: Receiver<FrameBuffer>,
    config: InferenceConfig,
    phase_rules: PhaseRules,
    crop_area: Arc<RwLock<CropArea>>,
    phase_status: PhaseStatus,
    manual_phase_advance: ManualPhaseAdvance,
    shutdown: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let detector = phase_detector::PhaseDetector::new(phase_rules, &config)?;

    windows_ocr::run_analysis_loop(
        rx_ml,
        Duration::from_secs(config.ocr.interval_secs),
        detector,
        crop_area,
        phase_status,
        manual_phase_advance,
        shutdown,
    )
}

#[cfg(test)]
mod tests {
    use super::{InferenceConfig, PhaseRules};

    /// `config/phase_rules.toml` のコンパイル時埋め込みコピー。
    /// テスト実行時のカレントディレクトリに依存しない。
    const EMBEDDED_PHASE_RULES_TOML: &str =
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/phase_rules.toml"));

    /// `config/inference.toml` のコンパイル時埋め込みコピー。
    const EMBEDDED_INFERENCE_TOML: &str =
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/inference.toml"));

    /// TOML 設定がパースでき、組み込みデフォルトと一致することを確認する。
    #[test]
    fn toml_config_parses_and_matches_default() {
        let rules: PhaseRules = toml::from_str(EMBEDDED_PHASE_RULES_TOML)
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

    /// 推論TOML(`[ocr]`テーブル含む)がパースでき、組み込みデフォルトと一致することを確認する。
    #[test]
    fn inference_toml_parses_and_matches_default() {
        let config: InferenceConfig = toml::from_str(EMBEDDED_INFERENCE_TOML)
            .expect("config/inference.toml must parse");
        let default = InferenceConfig::default();
        assert_eq!(config.resolution, default.resolution);
        assert_eq!(config.ocr, default.ocr);
    }
}