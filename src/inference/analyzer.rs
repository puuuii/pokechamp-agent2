use crate::hardware::FrameBuffer;
use crate::video::CropArea;

use super::phase_detector::PhaseChange;

pub trait FrameAnalyzer {
    fn tick(&mut self, frame: &FrameBuffer) -> anyhow::Result<Option<PhaseChange>>;

    fn phase_text(&self) -> String;

    fn advance_manually(&mut self) -> String;

    fn recognize_party_name(
        &self,
        frame: &FrameBuffer,
        crop: &CropArea,
    ) -> anyhow::Result<Option<String>>;

    /// 現在のフェーズが「選出」かどうか。
    /// 選出フェーズ限定の処理(パーティアイコンマッチングなど)をトリガーするために使う。
    fn is_selecting(&self) -> bool {
        false
    }
}
