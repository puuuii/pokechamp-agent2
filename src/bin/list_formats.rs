use anyhow::Result;
use nokhwa::{
    pixel_format::RgbFormat,
    utils::{CameraIndex, RequestedFormat, RequestedFormatType},
    Camera,
};

fn main() -> Result<()> {
    let index = CameraIndex::Index(0);
    let requested = RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestResolution);
    let mut camera = Camera::new(index, requested)?;

    for fmt in camera.compatible_camera_formats()? {
        println!("{:?}", fmt);
    }
    Ok(())
}
