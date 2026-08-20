use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, bounded};
use minifb::{Window, WindowOptions};
use nokhwa::{
    Camera,
    pixel_format::RgbFormat,
    utils::{
        CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution,
    },
};
use rayon::prelude::*;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

// ==========================================
// 1. Hardware Profile & Domain Definitions
// ==========================================

/// キャプチャカード等のハードウェア個体識別・動作プロファイル
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

pub type FrameBuffer = Arc<Vec<u32>>;

const SINGLE_SLOT_LATEST_FRAME_ONLY: usize = 1;

pub trait VideoSource {
    fn capture_frame(&mut self) -> Result<FrameBuffer>;
}

pub trait AudioPipeline: Send + 'static {
    fn start(self) -> Result<()>;
}

// ==========================================
// 2. Video Capture Component
// ==========================================
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
            width: 1920,
            height: 1080,
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
                    "Failed to open camera index {} with format {:?}: {e}. \
                     Please run `cargo run --bin list_devices` to check available formats.",
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

        anyhow::ensure!(
            width % 2 == 0,
            "YUYV requires an even width (2 bytes = 1 pixel pair), got {width}"
        );

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
    // SAFETY: All elements are guaranteed to be overwritten in parallel before read.
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

// ==========================================
// 3. Frame Dispatcher (Capture Service)
// ==========================================

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

// ==========================================
// 4. Display Window Component
// ==========================================

pub struct DisplayWindow {
    window: Window,
    width: usize,
    height: usize,
    current_frame: FrameBuffer,
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
        })
    }

    pub fn is_open(&self) -> bool {
        self.window.is_open()
    }

    fn drain_queue_and_get_latest_frame(&mut self, rx_frame: &Receiver<FrameBuffer>) -> bool {
        let mut received_new_frame = false;
        while let Ok(latest_frame) = rx_frame.try_recv() {
            self.current_frame = latest_frame;
            received_new_frame = true;
        }
        received_new_frame
    }

    pub fn render_latest(&mut self, rx_frame: &Receiver<FrameBuffer>) -> Result<()> {
        let has_new_frame = self.drain_queue_and_get_latest_frame(rx_frame);

        if has_new_frame {
            self.window
                .update_with_buffer(&self.current_frame, self.width, self.height)?;
        } else {
            self.window.update();
            thread::sleep(Duration::from_millis(1));
        }

        Ok(())
    }
}

// ==========================================
// 5. Audio Component
// ==========================================

pub mod audio {
    use super::{AudioPipeline, HardwareProfile};
    use anyhow::{Context, Result};
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use ringbuf::{
        HeapRb,
        traits::{Consumer, Producer, Split},
    };
    use std::thread;
    use std::time::Duration;

    const TARGET_AUDIO_LATENCY: Duration = Duration::from_millis(50);
    const MILLISECONDS_PER_SECOND: u64 = 1000;
    const SILENCE_SAMPLE: f32 = 0.0;

    fn calculate_ring_buffer_capacity(sample_rate: u32, channel_count: u32) -> usize {
        let samples_per_second_all_channels = (sample_rate * channel_count) as u64;
        let latency_ms = TARGET_AUDIO_LATENCY.as_millis() as u64;

        ((samples_per_second_all_channels * latency_ms) / MILLISECONDS_PER_SECOND) as usize
    }

    fn downmix_interleaved_to_mono(interleaved_samples: &[f32], channel_count: usize) -> Vec<f32> {
        interleaved_samples
            .chunks_exact(channel_count)
            .map(|frame| frame.iter().sum::<f32>() / channel_count as f32)
            .collect()
    }

    #[inline(always)]
    fn linear_interpolate(start_sample: f32, end_sample: f32, interpolation_factor: f32) -> f32 {
        start_sample + (end_sample - start_sample) * interpolation_factor
    }

    pub struct CpalAudioPassthrough {
        target_device_keyword: String,
    }

    impl CpalAudioPassthrough {
        pub fn for_hardware(profile: &HardwareProfile) -> Self {
            Self {
                target_device_keyword: profile.audio_device_keyword.to_lowercase(),
            }
        }
    }

    impl AudioPipeline for CpalAudioPassthrough {
        fn start(self) -> Result<()> {
            let host = cpal::default_host();

            let input_device = host
                .input_devices()?
                .find(|device| {
                    device
                        .name()
                        .map(|name| name.to_lowercase().contains(&self.target_device_keyword))
                        .unwrap_or(false)
                })
                .context("Target capture audio device not found")?;

            let output_device = host
                .default_output_device()
                .context("Default output device not found")?;

            let input_config = input_device.default_input_config()?;
            let output_config = output_device.default_output_config()?;

            let in_channels = input_config.channels() as usize;
            let in_sample_rate = input_config.sample_rate().0;
            let out_channels = output_config.channels() as usize;
            let out_sample_rate = output_config.sample_rate().0;

            println!("Audio Input: {in_channels} ch, {in_sample_rate} Hz");
            println!("Audio Output: {out_channels} ch, {out_sample_rate} Hz");

            let buffer_capacity =
                calculate_ring_buffer_capacity(out_sample_rate, out_channels as u32);
            let ring_buffer = HeapRb::<f32>::new(buffer_capacity);
            let (mut audio_producer, mut audio_consumer) = ring_buffer.split();

            let sample_rate_resample_ratio = out_sample_rate as f64 / in_sample_rate as f64;
            let mut previous_block_last_mono_sample: f32 = SILENCE_SAMPLE;

            let input_stream = input_device.build_input_stream(
                &input_config.into(),
                move |raw_input_data: &[f32], _| {
                    let mono_input_frames =
                        downmix_interleaved_to_mono(raw_input_data, in_channels);
                    let input_frame_count = mono_input_frames.len();
                    if input_frame_count == 0 {
                        return;
                    }

                    let output_frame_count =
                        ((input_frame_count as f64) * sample_rate_resample_ratio).round() as usize;

                    for output_index in 0..output_frame_count {
                        let source_position = output_index as f64 / sample_rate_resample_ratio;
                        let lower_sample_index = source_position.floor() as isize;
                        let interpolation_fraction =
                            (source_position - source_position.floor()) as f32;

                        let start_sample = if lower_sample_index < 0 {
                            previous_block_last_mono_sample
                        } else {
                            mono_input_frames[lower_sample_index as usize]
                        };

                        let end_sample = if (lower_sample_index + 1) as usize >= input_frame_count {
                            *mono_input_frames.last().unwrap()
                        } else {
                            mono_input_frames[(lower_sample_index + 1) as usize]
                        };

                        let resampled_value =
                            linear_interpolate(start_sample, end_sample, interpolation_fraction);

                        for _ in 0..out_channels {
                            let _ = audio_producer.try_push(resampled_value);
                        }
                    }

                    previous_block_last_mono_sample = *mono_input_frames.last().unwrap();
                },
                move |err| eprintln!("Audio input stream error: {err}"),
                None,
            )?;

            let output_stream = output_device.build_output_stream(
                &output_config.into(),
                move |output_buffer: &mut [f32], _| {
                    for destination_sample in output_buffer.iter_mut() {
                        *destination_sample = audio_consumer.try_pop().unwrap_or(SILENCE_SAMPLE);
                    }
                },
                move |err| eprintln!("Audio output stream error: {err}"),
                None,
            )?;

            input_stream.play()?;
            output_stream.play()?;

            // 音声ストリームのバックグラウンド動作中、現在のスレッドをブロックして生存を維持
            thread::park();
            Ok(())
        }
    }
}

// ==========================================
// 6. ML Inference Component (Placeholder)
// ==========================================

pub mod inference {
    use super::FrameBuffer;
    use crossbeam_channel::Receiver;
    use std::thread;

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
}

// ==========================================
// 7. Application Entry Point
// ==========================================

fn main() -> Result<()> {
    let video_config = VideoConfig::default();

    const ML_SUBSAMPLING_INTERVAL_FRAMES: u32 = 30; // ~2 FPS under 60 FPS capture

    let capture_service = CaptureService::new(video_config, ML_SUBSAMPLING_INTERVAL_FRAMES);
    let (rx_display, rx_ml) = capture_service.spawn_loop()?;

    thread::spawn(move || {
        let audio_pipeline = audio::CpalAudioPassthrough::for_hardware(
            &HardwareProfile::AVERMEDIA_LIVE_GAMER_MINI_GC311,
        );
        if let Err(e) = audio_pipeline.start() {
            eprintln!("Audio pipeline error: {e}");
        }
    });

    inference::InferenceWorker::spawn(
        rx_ml,
        inference::InferenceConfig {
            resolution: inference::ModelInputResolution::STANDARD_224X224,
        },
    );

    let display_resolution = video_config.resolution();
    let mut window = DisplayWindow::open_uncapped("Switch Capture", display_resolution)?;

    while window.is_open() {
        window.render_latest(&rx_display)?;
    }

    Ok(())
}
