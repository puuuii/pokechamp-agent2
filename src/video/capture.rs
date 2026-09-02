use anyhow::Result;
use nokhwa::{
    Camera,
    pixel_format::RgbFormat,
    query,
    utils::{
        ApiBackend, CameraFormat, CameraIndex, RequestedFormat, RequestedFormatType, Resolution,
    },
};
use std::sync::Arc;
use tracing::info;

use crate::hardware::{FrameBuffer, VideoSource, VideoSpec};

use super::colorspace::decode_yuyv_to_packed_rgb_parallel;

pub struct NokhwaCapture {
    camera: Camera,
    width: usize,
    height: usize,
}

impl NokhwaCapture {
    pub fn new(video: &VideoSpec, device_keyword: &str) -> Result<Self> {
        let desired_format = CameraFormat::new(
            Resolution::new(video.width, video.height),
            video.frame_format,
            video.fps,
        );

        let requested_format =
            RequestedFormat::new::<RgbFormat>(RequestedFormatType::Exact(desired_format));

        let device_index = Self::find_device_index(device_keyword)?;

        let mut camera = Camera::new(device_index, requested_format).map_err(|e| {
            anyhow::anyhow!(
                "Failed to open video device with format {:?}: {e}.",
                desired_format
            )
        })?;

        camera.open_stream()?;
        let actual_format = camera.camera_format();
        info!("Camera opened. Actual format: {actual_format:?}");

        anyhow::ensure!(
            actual_format.format() == video.frame_format,
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

    /// デバイス列挙し、デバイス名(部分一致)で対象映像デバイスを特定する。
    ///
    /// 名前マッチとすることで、他デバイスの挿入でデバイスindexがずれ込んでも安定する。
    fn find_device_index(keyword: &str) -> Result<CameraIndex> {
        let devices = query(ApiBackend::Auto)
            .map_err(|e| anyhow::anyhow!("Failed to enumerate video devices: {e}"))?;

        let keyword = keyword.to_lowercase();
        let Some(wanted) = devices
            .iter()
            .find(|device| device.human_name().to_lowercase().contains(&keyword))
        else {
            let available: Vec<String> = devices.iter().map(|device| device.human_name()).collect();
            anyhow::bail!(
                "No video device matching \"{keyword}\" was found. Available devices: {available:?}"
            );
        };

        Ok(wanted.index().clone())
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
