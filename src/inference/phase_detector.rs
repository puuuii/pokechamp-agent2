use windows::{
    core::HSTRING,
    Globalization::Language,
    Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap},
    Media::Ocr::OcrEngine,
    Storage::Streams::DataWriter,
};

use crate::hardware::FrameBuffer;
use crate::video::{CropArea, PixelCropArea};

use super::analyzer::FrameAnalyzer;
use super::preprocess::preprocess_white_text_extraction;
use super::{InferenceConfig, ModelInputResolution, OcrConfig, PhaseRules, PhaseTarget};

/// ゲームのフェーズ。
/// 「待機」と「対戦終了」は自動判定上は同一の監視状態だが、
/// 表示テキスト(および手動進行のサイクル)で区別するため別状態として扱う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Waiting,
    Selecting,
    Battling,
    Ended,
}

impl Phase {
    /// 手動進行時の次のフェーズ(選出→バトル→対戦終了→選出…)。
    /// 待機は起動時のみで、サイクルには含めない。
    pub const fn next(self) -> Self {
        match self {
            Self::Waiting => Self::Selecting,
            Self::Selecting => Self::Battling,
            Self::Battling => Self::Ended,
            Self::Ended => Self::Selecting,
        }
    }
}

/// フェーズ遷移の結果(遷移先フェーズと表示テキスト)。
pub struct PhaseChange {
    pub phase: Phase,
    pub display_text: String,
}

/// フェーズ遷移判定(OCRベース)。
///
/// OCRエンジン・現在フェーズ・`PhaseRules` を保持する。
/// ループ制御(スロットリング、手動進行リクエストの消費)は呼び出し側が担当する。
pub struct PhaseDetector {
    current_phase: Phase,
    ocr_engine: OcrEngine,
    phase_rules: PhaseRules,
    ocr: OcrConfig,
    resolution: ModelInputResolution,
}

impl PhaseDetector {
    /// 初期状態(待機)の検出器を作成する。
    ///
    /// 日本語言語パック(OCR)が未インストールの場合はエラーになる。
    pub fn new(phase_rules: PhaseRules, config: &InferenceConfig) -> anyhow::Result<Self> {
        let ja_lang = Language::CreateLanguage(&HSTRING::from("ja-JP"))?;

        if !OcrEngine::IsLanguageSupported(&ja_lang)? {
            anyhow::bail!("Windowsの「日本語」言語パック（OCR）がインストールされていません。");
        }

        let ocr_engine = OcrEngine::TryCreateFromLanguage(&ja_lang)?;

        Ok(Self {
            current_phase: Phase::Waiting,
            ocr_engine,
            phase_rules,
            ocr: config.ocr,
            resolution: config.resolution,
        })
    }

    /// 待機・対戦終了監視状態の判定。
    ///
    /// 「対戦終了」文字列が残っている間は対戦終了に留まり、
    /// 消えたらランクバトルのリボン(選出)を監視する。
    fn detect_from_idle(&self, frame: &FrameBuffer) -> anyhow::Result<Phase> {
        let ended_matched = self.ocr_target_match(frame, &self.phase_rules.ended)?;
        if ended_matched >= self.phase_rules.ended.threshold {
            return Ok(Phase::Ended);
        }

        let ribbon_matched = self.ocr_target_match(frame, &self.phase_rules.ribbon)?;
        if ribbon_matched >= self.phase_rules.ribbon.threshold {
            return Ok(Phase::Selecting);
        }

        Ok(self.current_phase)
    }

    /// 選出中は「ランクバトル」リボンが表示され続けている。
    /// リボンが検出されなくなった時点でバトルフェーズに遷移する。
    fn detect_selecting(&self, frame: &FrameBuffer) -> anyhow::Result<Phase> {
        let ribbon_matched = self.ocr_target_match(frame, &self.phase_rules.ribbon)?;
        if ribbon_matched < self.phase_rules.ribbon.threshold {
            return Ok(Phase::Battling);
        }

        Ok(self.current_phase)
    }

    /// バトル中のときだけ、対戦終了判定を評価する。
    fn detect_battling(&self, frame: &FrameBuffer) -> anyhow::Result<Phase> {
        let ended_matched = self.ocr_target_match(frame, &self.phase_rules.ended)?;
        if ended_matched >= self.phase_rules.ended.threshold {
            return Ok(Phase::Ended);
        }

        Ok(self.current_phase)
    }

    /// PhaseTarget のクロップ範囲でOCRを実行し、対象文字の種類数を返す。
    fn ocr_target_match(&self, frame: &FrameBuffer, target: &PhaseTarget) -> anyhow::Result<usize> {
        let (model_w, model_h) = self.resolution.as_usize();
        let crop = target.crop.to_pixels(model_w, model_h);

        let text =
            self.recognize_text_in_crop(frame, crop, self.ocr.upscale_factor)?;

        Ok(text
            .as_deref()
            .map(|text| count_matched_chars(&target.target_chars, text))
            .unwrap_or(0))
    }

    /// 指定クロップ範囲に対してOCRを実行し、空白除去済みの認識テキストを返す。
    fn recognize_text_in_crop(
        &self,
        frame: &FrameBuffer,
        crop: PixelCropArea,
        scale_factor: f32,
    ) -> anyhow::Result<Option<String>> {
        let (model_w, model_h) = self.resolution.as_usize();
        let (rgba_bytes, scaled_w, scaled_h) = preprocess_white_text_extraction(
            frame,
            model_w,
            model_h,
            crop,
            scale_factor,
            self.ocr.white_text_threshold,
        );

        if rgba_bytes.is_empty() {
            return Ok(None);
        }

        let bitmap = create_software_bitmap(&rgba_bytes, scaled_w as u32, scaled_h as u32)?;

        let async_op = self.ocr_engine.RecognizeAsync(&bitmap)?;
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

    /// 各フェーズの表示テキスト。
    fn text_for(&self, phase: Phase) -> String {
        match phase {
            Phase::Waiting => self.phase_rules.waiting_text.clone(),
            Phase::Selecting => self.phase_rules.ribbon.enter_text.clone(),
            Phase::Battling => self.phase_rules.battling_text.clone(),
            Phase::Ended => self.phase_rules.ended.enter_text.clone(),
        }
    }
}

impl FrameAnalyzer for PhaseDetector {
    /// 1フレーム分だけフェーズ遷移判定を実行する。
    ///
    /// 遷移が起きた場合のみ `Some(遷移先フェーズ, 表示テキスト)` を返す。
    fn tick(&mut self, frame: &FrameBuffer) -> anyhow::Result<Option<PhaseChange>> {
        let next_phase = match self.current_phase {
            // 待機・対戦終了は同一の監視状態として扱う。
            Phase::Waiting | Phase::Ended => self.detect_from_idle(frame)?,
            Phase::Selecting => self.detect_selecting(frame)?,
            Phase::Battling => self.detect_battling(frame)?,
        };

        if next_phase == self.current_phase {
            return Ok(None);
        }

        self.current_phase = next_phase;
        Ok(Some(PhaseChange {
            phase: next_phase,
            display_text: self.text_for(next_phase),
        }))
    }

    /// 現在フェーズの表示テキスト。
    fn phase_text(&self) -> String {
        self.text_for(self.current_phase)
    }

    /// 手動進行でフェーズを1つ進める。進行後の表示テキストを返す。
    fn advance_manually(&mut self) -> String {
        self.current_phase = self.current_phase.next();
        self.text_for(self.current_phase)
    }

    /// ユーザー調整クロップ領域(パーティ名など)のOCR。
    /// 未完成機能: 現状は認識テキストを返すだけ。
    fn recognize_party_name(
        &self,
        frame: &FrameBuffer,
        crop: &CropArea,
    ) -> anyhow::Result<Option<String>> {
        let (model_w, model_h) = self.resolution.as_usize();
        let crop = crop.to_pixels(model_w, model_h);
        self.recognize_text_in_crop(frame, crop, self.ocr.upscale_factor)
    }
}

/// target_chars のうち text に含まれる文字の種類数を返す(重複はカウントしない)。
fn count_matched_chars(target_chars: &[String], text: &str) -> usize {
    target_chars
        .iter()
        .filter(|target_char| text.contains(target_char.as_str()))
        .count()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_advance_cycles_selecting_battling_ended() {
        assert_eq!(Phase::Selecting.next(), Phase::Battling);
        assert_eq!(Phase::Battling.next(), Phase::Ended);
        assert_eq!(Phase::Ended.next(), Phase::Selecting);
    }

    #[test]
    fn waiting_advances_to_selecting_but_is_not_part_of_the_cycle() {
        assert_eq!(Phase::Waiting.next(), Phase::Selecting);
        // サイクルは選出から始まるため、待機に戻ることはない。
        assert_ne!(Phase::Selecting.next(), Phase::Waiting);
    }
}