use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, bounded};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::hardware::{FrameBuffer, VideoSource};

use super::VideoConfig;
use super::capture::NokhwaCapture;

const SINGLE_SLOT_LATEST_FRAME_ONLY: usize = 1;

fn publish_latest_frame_dropping_lagging(
    sender: &Sender<FrameBuffer>,
    receiver_drain_handle: &Receiver<FrameBuffer>,
    new_frame: FrameBuffer,
) {
    if let Err(crossbeam_channel::TrySendError::Full(rejected_frame)) = sender.try_send(new_frame) {
        let _ = receiver_drain_handle.try_recv();
        let _ = sender.try_send(rejected_frame);
    }
}

pub struct CaptureService {
    config: VideoConfig,
    ml_sample_interval_frames: u32,
}

impl CaptureService {
    pub fn new(config: VideoConfig, ml_sample_interval_frames: u32) -> Self {
        Self {
            config,
            ml_sample_interval_frames: ml_sample_interval_frames.max(1),
        }
    }

    pub fn spawn_loop(self) -> Result<(Receiver<FrameBuffer>, Receiver<FrameBuffer>)> {
        let (tx_display, rx_display) = bounded::<FrameBuffer>(SINGLE_SLOT_LATEST_FRAME_ONLY);
        let (tx_ml, rx_ml) = bounded::<FrameBuffer>(SINGLE_SLOT_LATEST_FRAME_ONLY);

        let rx_display_drain_handle = rx_display.clone();
        let rx_ml_drain_handle = rx_ml.clone();

        thread::spawn(move || {
            let mut camera_source = match NokhwaCapture::new(&self.config) {
                Ok(source) => source,
                Err(e) => {
                    eprintln!("Failed to initialize camera: {e}");
                    return;
                }
            };

            let mut captured_frames_this_second = 0u32;
            let mut frames_since_last_ml_sample = 0u32;
            let mut fps_timer = Instant::now();

            loop {
                let frame_buffer = match camera_source.capture_frame() {
                    Ok(buffer) => buffer,
                    Err(e) => {
                        eprintln!("Capture frame error: {e}");
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                };

                publish_latest_frame_dropping_lagging(
                    &tx_display,
                    &rx_display_drain_handle,
                    Arc::clone(&frame_buffer),
                );

                frames_since_last_ml_sample += 1;
                if frames_since_last_ml_sample >= self.ml_sample_interval_frames {
                    frames_since_last_ml_sample = 0;
                    publish_latest_frame_dropping_lagging(
                        &tx_ml,
                        &rx_ml_drain_handle,
                        frame_buffer,
                    );
                }

                captured_frames_this_second += 1;
                if fps_timer.elapsed().as_secs() >= 1 {
                    println!("Capture FPS: {captured_frames_this_second}");
                    captured_frames_this_second = 0;
                    fps_timer = Instant::now();
                }
            }
        });

        Ok((rx_display, rx_ml))
    }
}
