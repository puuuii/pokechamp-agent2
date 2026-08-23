use anyhow::Result;
use nokhwa::utils::FrameFormat;
use std::sync::Arc;

pub type FrameBuffer = Arc<Vec<u32>>;

/// 映像キャプチャの形式仕様(解像度・fps・ピクセルフォーマット)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoSpec {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub frame_format: FrameFormat,
}

impl VideoSpec {
    /// (幅, 高さ) の usize 対として返す。
    pub fn resolution(&self) -> (usize, usize) {
        (self.width as usize, self.height as usize)
    }
}

/// ハードウェアプロファイル。
///
/// 1台のキャプチャデバイスで必要な識別情報を束ねる:
/// 音声・映像のデバイス名キーワード(部分一致)と映像キャプチャ形式。
/// 新しいキャプチャ機種を追加するときは、ここに const を追加するだけ。
#[derive(Debug, Clone, Copy)]
pub struct HardwareProfile {
    pub name: &'static str,
    /// 音声デバイス名キーワード(大文字小文字を無視した部分一致で探す)。
    pub audio_device_keyword: &'static str,
    /// 映像デバイス名キーワード(大文字小文字を無視した部分一致で探す)。
    pub video_device_keyword: &'static str,
    /// 映像キャプチャ形式。
    pub video: VideoSpec,
}

impl HardwareProfile {
    pub const AVERMEDIA_LIVE_GAMER_MINI_GC311: Self = Self {
        name: "AVerMedia Live Gamer MINI (GC311)",
        audio_device_keyword: "gc311",
        video_device_keyword: "avermedia",
        video: VideoSpec {
            width: 1280,
            height: 720,
            fps: 60,
            frame_format: FrameFormat::YUYV,
        },
    };
}

/// 映像キャプチャ手段の抽象化。
///
/// 新規キャプチャ手段(nokhwa 以外の SDK・別ピクセルフォーマットなど)は、
/// 新規 struct にこの trait を実装するだけで差し替え可能。
pub trait VideoSource: Send {
    fn capture_frame(&mut self) -> Result<FrameBuffer>;
}

pub trait AudioPipeline: Send + 'static {
    fn start(self) -> Result<()>;
}