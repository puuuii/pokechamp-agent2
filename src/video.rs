mod buffer;
mod capture;
mod colorspace;
mod crop;
mod display;
mod jp_text;
mod pipeline;
#[allow(unused_imports)]
pub use capture::NokhwaCapture;
pub use crop::{CropArea, PixelCropArea};
pub use display::DisplayWindow;
pub use pipeline::CaptureService;

use nokhwa::utils::FrameFormat;

#[derive(Debug, Clone, Copy)]
pub struct VideoConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub camera_index: u32,
    pub frame_format: FrameFormat,
}

impl VideoConfig {
    pub fn resolution(&self) -> (usize, usize) {
        (self.width as usize, self.height as usize)
    }
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            fps: 60,
            camera_index: 0,
            frame_format: FrameFormat::YUYV,
        }
    }
}
