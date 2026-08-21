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

pub struct InferenceWorker;

impl InferenceWorker {
    pub fn spawn(
        rx_ml: Receiver<FrameBuffer>,
        config: InferenceConfig,
        crop_area: Arc<RwLock<CropArea>>,
    ) {
        thread::spawn(move || {
            #[cfg(windows)]
            if let Err(e) = windows_ocr::run_ocr_loop(rx_ml, config, crop_area) {
                eprintln!("OCR Worker error: {e}");
            }

            #[cfg(not(windows))]
            eprintln!("Windows.Media.Ocr is only supported on Windows.");
        });
    }
}
