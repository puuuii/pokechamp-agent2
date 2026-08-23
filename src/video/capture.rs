use anyhow::Result;
use nokhwa::{
    Camera,
    pixel_format::RgbFormat,
    utils::{CameraFormat, CameraIndex, RequestedFormat, RequestedFormatType, Resolution},
};
use std::sync::Arc;

use crate::hardware::{FrameBuffer, VideoSource};

use super::VideoConfig;
use super::colorspace::decode_yuyv_to_packed_rgb_parallel;

pub struct NokhwaCapture {
    camera: Camera,
    width: usize,
    height: usize,
}

impl NokhwaCapture {
    pub fn new(config: &VideoConfig) -> Result<Self> {
        let desired_format = CameraFormat::new(
            Resolution::new(config.width, config.height),
            config.frame_format,
            config.fps,
        );

        let requested_format =
            RequestedFormat::new::<RgbFormat>(RequestedFormatType::Exact(desired_format));

        let mut camera = Camera::new(CameraIndex::Index(config.camera_index), requested_format)
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to open camera index {} with format {:?}: {e}.",
                    config.camera_index,
                    desired_format
                )
            })?;

        camera.open_stream()?;
        let actual_format = camera.camera_format();
        println!("Camera opened. Actual format: {actual_format:?}");

        anyhow::ensure!(
            actual_format.format() == config.frame_format,
            "Direct YUYV decode requires FrameFormat::YUYV, but got {:?}.",
            actual_format.format()
        );

        let width = actual_format.resolution().width() as usize;
        let height = actual_format.resolution().height() as usize;

        anyhow::ensure!(width % 2 == 0, "YUYV requires an even width, got {width}");

        Ok(Self {
            camera,
            width,
            height,
        })
    }
}

impl VideoSource for NokhwaCapture {
    fn capture_frame(&mut self) -> Result<FrameBuffer> {
        let frame = self.camera.frame()?;
        let raw_yuyv_bytes = frame.buffer();

        let total_pixel_count = self.width * self.height;
        let mut rgb_pixels = vec![0u32; total_pixel_count];

        decode_yuyv_to_packed_rgb_parallel(raw_yuyv_bytes, &mut rgb_pixels, self.width);

        Ok(Arc::new(rgb_pixels))
    }
}
