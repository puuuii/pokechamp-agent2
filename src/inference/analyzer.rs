use crate::hardware::FrameBuffer;
use crate::video::CropArea;

use super::phase_detector::PhaseChange;

/// フレーム分析器の契約。
///
/// 実装者は1つの分析器種別(OCR・MLなど)の検出状態を持つ。
/// ループ制御(シャットダウン、手動進行、実行間隔スロットリング)は呼び出し側が担当する。
/// 新しい分析器(例: ONNXベースの `MlPhaseDetector`)を追加するときは、
/// 新規 struct にこの trait を実装するだけで、ループ制御・チャンネル配線・
/// シャットダウン処理には一切触れなくて済む。
pub trait FrameAnalyzer {
    /// 1フレーム分だけフェーズ遷移判定を実行する。
    ///
    /// 遷移が起きた場合のみ `Some(遷移先フェーズ, 表示テキスト)` を返す。
    fn tick(&mut self, frame: &FrameBuffer) -> anyhow::Result<Option<PhaseChange>>;

    /// 現在フェーズの表示テキスト。
    fn phase_text(&self) -> String;

    /// 手動進行でフェーズを1つ進める。進行後の表示テキストを返す。
    fn advance_manually(&mut self) -> String;

    /// ユーザー調整クロップ領域(パーティ名など)のテキスト認識。
    fn recognize_party_name(
        &self,
        frame: &FrameBuffer,
        crop: &CropArea,
    ) -> anyhow::Result<Option<String>>;
}
