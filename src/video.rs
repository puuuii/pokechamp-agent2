use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, bounded};
use minifb::{Key, Window, WindowOptions};
use nokhwa::{
    Camera,
    pixel_format::RgbFormat,
    utils::{
        CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution,
    },
};
use rayon::prelude::*;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::hardware::{FrameBuffer, VideoSource};

const SINGLE_SLOT_LATEST_FRAME_ONLY: usize = 1;

#[derive(Debug, Clone, Copy)]
pub struct CropArea {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl CropArea {
    pub fn default_720p() -> Self {
        Self {
            x: 520,
            y: 90,
            width: 240,
            height: 60,
        }
    }

    pub fn clamp(&mut self, max_w: usize, max_h: usize) {
        self.width = self.width.clamp(20, max_w);
        self.height = self.height.clamp(20, max_h);
        self.x = self.x.clamp(0, max_w.saturating_sub(self.width));
        self.y = self.y.clamp(0, max_h.saturating_sub(self.height));
    }
}

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
            actual_format.format() == FrameFormat::YUYV,
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

struct Bt601ColorSpace;

impl Bt601ColorSpace {
    const LUMA_STUDIO_MIN_OFFSET: i32 = 16;
    const CHROMA_CENTER_OFFSET: i32 = 128;

    #[inline(always)]
    fn yuv_to_packed_rgb(luma: i32, chroma_u: i32, chroma_v: i32) -> u32 {
        let normalized_luma = luma - Self::LUMA_STUDIO_MIN_OFFSET;
        let normalized_u = chroma_u - Self::CHROMA_CENTER_OFFSET;
        let normalized_v = chroma_v - Self::CHROMA_CENTER_OFFSET;

        let red = ((298 * normalized_luma + 409 * normalized_v + 128) >> 8).clamp(0, 255) as u32;
        let green = ((298 * normalized_luma - 100 * normalized_u - 208 * normalized_v + 128) >> 8)
            .clamp(0, 255) as u32;
        let blue = ((298 * normalized_luma + 516 * normalized_u + 128) >> 8).clamp(0, 255) as u32;

        (red << 16) | (green << 8) | blue
    }
}

fn create_uninitialized_pixel_buffer(pixel_count: usize) -> Vec<u32> {
    let mut buffer = Vec::with_capacity(pixel_count);
    unsafe {
        buffer.set_len(pixel_count);
    }
    buffer
}

fn decode_yuyv_to_packed_rgb_parallel(
    yuyv_raw_bytes: &[u8],
    output_pixels: &mut [u32],
    image_width: usize,
) {
    let bytes_per_yuyv_row = image_width * 2;

    output_pixels
        .par_chunks_exact_mut(image_width)
        .zip(yuyv_raw_bytes.par_chunks_exact(bytes_per_yuyv_row))
        .for_each(|(output_row, input_row_bytes)| {
            let output_pixel_pairs = output_row.chunks_exact_mut(2);
            let input_yuyv_quads = input_row_bytes.chunks_exact(4);

            for (pixel_pair, yuyv_quad) in output_pixel_pairs.zip(input_yuyv_quads) {
                let luma_0 = yuyv_quad[0] as i32;
                let chroma_u = yuyv_quad[1] as i32;
                let luma_1 = yuyv_quad[2] as i32;
                let chroma_v = yuyv_quad[3] as i32;

                pixel_pair[0] = Bt601ColorSpace::yuv_to_packed_rgb(luma_0, chroma_u, chroma_v);
                pixel_pair[1] = Bt601ColorSpace::yuv_to_packed_rgb(luma_1, chroma_u, chroma_v);
            }
        });
}

impl VideoSource for NokhwaCapture {
    fn capture_frame(&mut self) -> Result<FrameBuffer> {
        let frame = self.camera.frame()?;
        let raw_yuyv_bytes = frame.buffer();

        let total_pixel_count = self.width * self.height;
        let mut rgb_pixels = create_uninitialized_pixel_buffer(total_pixel_count);

        decode_yuyv_to_packed_rgb_parallel(raw_yuyv_bytes, &mut rgb_pixels, self.width);

        Ok(Arc::new(rgb_pixels))
    }
}

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

pub struct DisplayWindow {
    window: Window,
    width: usize,
    height: usize,
    current_frame: FrameBuffer,
    render_buffer: Vec<u32>,
    last_input_time: Instant,
    show_debug_frame: bool,
}

impl DisplayWindow {
    pub fn open_uncapped(title: &str, resolution: (usize, usize)) -> Result<Self> {
        let (width, height) = resolution;
        let mut window = Window::new(title, width, height, WindowOptions::default())?;

        const UNCAPPED_FPS: usize = 0;
        window.set_target_fps(UNCAPPED_FPS);

        Ok(Self {
            window,
            width,
            height,
            current_frame: Arc::new(vec![0u32; width * height]),
            render_buffer: vec![0u32; width * height],
            last_input_time: Instant::now(),
            // デバッグビルド(cargo run)時のみデフォルトON
            show_debug_frame: cfg!(debug_assertions),
        })
    }

    pub fn is_open(&self) -> bool {
        self.window.is_open()
    }

    fn handle_input(&mut self, crop_area: &Arc<RwLock<CropArea>>) {
        // Dキーのトグル判定（押した一瞬だけ検知し、長押し連打を防ぐ）
        if cfg!(debug_assertions) && self.window.is_key_pressed(Key::D, minifb::KeyRepeat::No) {
            self.show_debug_frame = !self.show_debug_frame;
            println!("[Debug Frame] Visibility: {}", self.show_debug_frame);
            return;
        }

        // 矢印キーによる連続移動用の入力インターバル（50ms）
        if self.last_input_time.elapsed() < Duration::from_millis(50) {
            return;
        }

        let mut crop = crop_area.write().unwrap();
        let shift =
            self.window.is_key_down(Key::LeftShift) || self.window.is_key_down(Key::RightShift);
        let step = 2;

        let mut changed = false;

        if shift {
            if self.window.is_key_down(Key::Left) {
                crop.width = crop.width.saturating_sub(step);
                changed = true;
            }
            if self.window.is_key_down(Key::Right) {
                crop.width += step;
                changed = true;
            }
            if self.window.is_key_down(Key::Up) {
                crop.height = crop.height.saturating_sub(step);
                changed = true;
            }
            if self.window.is_key_down(Key::Down) {
                crop.height += step;
                changed = true;
            }
        } else {
            if self.window.is_key_down(Key::Left) {
                crop.x = crop.x.saturating_sub(step);
                changed = true;
            }
            if self.window.is_key_down(Key::Right) {
                crop.x += step;
                changed = true;
            }
            if self.window.is_key_down(Key::Up) {
                crop.y = crop.y.saturating_sub(step);
                changed = true;
            }
            if self.window.is_key_down(Key::Down) {
                crop.y += step;
                changed = true;
            }
        }

        if changed {
            crop.clamp(self.width, self.height);
            self.last_input_time = Instant::now();
            println!(
                "[Crop Box] x: {}, y: {}, w: {}, h: {}",
                crop.x, crop.y, crop.width, crop.height
            );
        }
    }

    fn drain_queue_and_get_latest_frame(&mut self, rx_frame: &Receiver<FrameBuffer>) -> bool {
        let mut received_new_frame = false;
        while let Ok(latest_frame) = rx_frame.try_recv() {
            self.current_frame = latest_frame;
            received_new_frame = true;
        }
        received_new_frame
    }

    pub fn render_latest(
        &mut self,
        rx_frame: &Receiver<FrameBuffer>,
        crop_area: &Arc<RwLock<CropArea>>,
    ) -> Result<()> {
        self.handle_input(crop_area);

        let has_new_frame = self.drain_queue_and_get_latest_frame(rx_frame);

        if has_new_frame {
            self.render_buffer.copy_from_slice(&self.current_frame);

            // デバッグビルド 且つ show_debug_frame が true の場合のみ描画
            if cfg!(debug_assertions) && self.show_debug_frame {
                let crop = *crop_area.read().unwrap();
                draw_red_box(&mut self.render_buffer, self.width, self.height, &crop);
            }

            self.window
                .update_with_buffer(&self.render_buffer, self.width, self.height)?;
        } else {
            self.window.update();
            thread::sleep(Duration::from_millis(1));
        }

        Ok(())
    }
}

fn draw_red_box(buffer: &mut [u32], img_w: usize, img_h: usize, crop: &CropArea) {
    const RED_COLOR: u32 = 0x00FF_0000;
    let thickness = 3;

    let x1 = crop.x;
    let y1 = crop.y;
    let x2 = (crop.x + crop.width).min(img_w);
    let y2 = (crop.y + crop.height).min(img_h);

    for t in 0..thickness {
        for x in x1..x2 {
            if y1 + t < img_h {
                buffer[(y1 + t) * img_w + x] = RED_COLOR;
            }
            if y2.saturating_sub(1 + t) < img_h {
                buffer[(y2.saturating_sub(1 + t)) * img_w + x] = RED_COLOR;
            }
        }
        for y in y1..y2 {
            if x1 + t < img_w {
                buffer[y * img_w + (x1 + t)] = RED_COLOR;
            }
            if x2.saturating_sub(1 + t) < img_w {
                buffer[y * img_w + x2.saturating_sub(1 + t)] = RED_COLOR;
            }
        }
    }
}
