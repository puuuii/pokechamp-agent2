use crossbeam_channel::Receiver;
use std::thread;

use crate::hardware::FrameBuffer;

#[allow(dead_code)]
pub struct ModelInputResolution {
    pub width: u32,
    pub height: u32,
}

impl ModelInputResolution {
    pub const STANDARD_224X224: Self = Self {
        width: 224,
        height: 224,
    };
}

#[allow(dead_code)]
pub struct InferenceConfig {
    pub resolution: ModelInputResolution,
}

pub struct InferenceWorker;

impl InferenceWorker {
    pub fn spawn(rx_ml: Receiver<FrameBuffer>, _config: InferenceConfig) {
        thread::spawn(move || {
            for frame in rx_ml.iter() {
                let _ = frame.len();
            }
        });
    }
}
