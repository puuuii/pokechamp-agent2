use anyhow::Result;
use std::sync::Arc;

pub type FrameBuffer = Arc<Vec<u32>>;

[allow(dead_code)]
pub struct HardwareProfile {
    pub name: &'static str,
    pub audio_device_keyword: &'static str,
}

impl HardwareProfile {
    pub const AVERMEDIA_LIVE_GAMER_MINI_GC311: Self = Self {
        name: "AVerMedia Live Gamer MINI (GC311)",
        audio_device_keyword: "gc311",
    };
}

pub trait VideoSource {
    fn capture_frame(&mut self) -> Result<FrameBuffer>;
}

pub trait AudioPipeline: Send + 'static {
    fn start(self) -> Result<()>;
}
